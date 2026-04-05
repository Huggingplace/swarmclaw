use crate::skills::Skill;
use crate::tools::Tool;
use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use reqwest::{Client as HttpClient, Method};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use wasi_common::WasiCtx;
use wasmtime::{
    Caller, Config, Engine, Extern, Instance, InstanceAllocationStrategy, Linker, Module,
    PoolingAllocationConfig, Store,
};

// Import the generated FlatBuffers code.
#[allow(dead_code, unused_imports)]
#[path = "../plugin_generated.rs"]
mod plugin_generated;
use flatbuffers::{root, FlatBufferBuilder};
use plugin_generated::swarmclaw_plugin::{
    PluginManifest, PluginRequest, PluginRequestArgs, PluginResponse,
};

struct WasmState {
    wasi: WasiCtx,
    capabilities: Vec<String>,
    http_client: HttpClient,
    http_response: Vec<u8>,
}

#[derive(Debug, Clone, Deserialize)]
struct WasmToolDescriptor {
    name: String,
    description: String,
    #[serde(default)]
    parameters: Value,
}

#[derive(Debug, Deserialize)]
struct HostHttpRequest {
    method: String,
    url: String,
    #[serde(default)]
    headers: BTreeMap<String, String>,
    #[serde(default)]
    body: Option<String>,
}

struct WasmTool {
    name: String,
    description: String,
    parameters: Value,
    engine: Engine,
    module: Module,
    capabilities: Vec<String>,
}

#[async_trait]
impl Tool for WasmTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters(&self) -> Value {
        self.parameters.clone()
    }

    async fn execute(&self, args: Value) -> Result<String> {
        let (mut store, instance) =
            instantiate(&self.engine, &self.module, self.capabilities.clone())?;

        let memory = instance
            .get_memory(&mut store, "memory")
            .context("WASM module missing 'memory' export")?;
        let malloc = instance
            .get_typed_func::<i32, i32>(&mut store, "claw_malloc")
            .context("WASM module missing 'claw_malloc' export")?;
        let free = instance
            .get_typed_func::<(i32, i32), ()>(&mut store, "claw_free")
            .context("WASM module missing 'claw_free' export")?;
        let execute = instance
            .get_typed_func::<(i32, i32), i64>(&mut store, "claw_execute")
            .context("WASM module missing 'claw_execute' export")?;

        let request_bytes = build_plugin_request(&self.name, &args)?;
        let request_len = request_bytes.len() as i32;
        let request_ptr = malloc.call(&mut store, request_len)?;
        memory.write(&mut store, request_ptr as usize, &request_bytes)?;

        let response_handle = execute.call(&mut store, (request_ptr, request_len))?;
        free.call(&mut store, (request_ptr, request_len))?;

        let (response_ptr, response_len) = unpack_ptr_len(response_handle);
        if response_ptr == 0 || response_len == 0 {
            bail!(
                "WASM plugin '{}' returned an empty response buffer",
                self.name
            );
        }

        let mut response_bytes = vec![0u8; response_len];
        memory.read(&mut store, response_ptr, &mut response_bytes)?;
        free.call(&mut store, (response_ptr as i32, response_len as i32))?;

        let response = root::<PluginResponse>(&response_bytes)
            .context("Failed to decode PluginResponse from WASM module")?;
        if !response.success() {
            bail!(
                "{}",
                response
                    .error_message()
                    .or_else(|| response.result_json())
                    .unwrap_or("WASM tool failed")
            );
        }

        Ok(response.result_json().unwrap_or("{}").to_string())
    }
}

pub struct WasmSkill {
    name: String,
    tools: Vec<Arc<dyn Tool>>,
}

impl WasmSkill {
    pub fn new(path: PathBuf) -> Result<Self> {
        let mut config = Config::new();

        let mut pool = PoolingAllocationConfig::default();
        pool.total_component_instances(100);
        pool.total_core_instances(100);
        pool.total_memories(100);
        pool.total_tables(100);

        config.allocation_strategy(InstanceAllocationStrategy::Pooling(pool));
        let engine =
            Engine::new(&config).context("Failed to create Wasmtime Engine with pooling")?;

        let module = load_module(&engine, &path)?;
        let manifest = discover_manifest(&engine, &module, &path)?;
        let WasmManifest {
            name,
            tools,
            capabilities,
        } = manifest;

        let tools = tools
            .into_iter()
            .map(|tool| {
                Arc::new(WasmTool {
                    name: tool.name,
                    description: tool.description,
                    parameters: if tool.parameters.is_null() {
                        serde_json::json!({ "type": "object", "properties": {} })
                    } else {
                        tool.parameters
                    },
                    engine: engine.clone(),
                    module: module.clone(),
                    capabilities: capabilities.clone(),
                }) as Arc<dyn Tool>
            })
            .collect::<Vec<_>>();

        Ok(Self { name, tools })
    }
}

#[async_trait]
impl Skill for WasmSkill {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        "Dynamically loaded WASM skill"
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.tools.clone()
    }
}

#[derive(Debug)]
struct WasmManifest {
    name: String,
    tools: Vec<WasmToolDescriptor>,
    capabilities: Vec<String>,
}

fn load_module(engine: &Engine, path: &Path) -> Result<Module> {
    let cwasm_path = path.with_extension("cwasm");
    if cwasm_path.exists() {
        unsafe {
            return Module::deserialize_file(engine, &cwasm_path)
                .context("Failed to deserialize cached .cwasm module");
        }
    }

    let module =
        Module::from_file(engine, path).context("Failed to load and compile WASM module")?;
    if let Ok(serialized) = module.serialize() {
        let _ = fs::write(&cwasm_path, serialized);
    }
    Ok(module)
}

fn discover_manifest(engine: &Engine, module: &Module, path: &Path) -> Result<WasmManifest> {
    let fallback_name = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("unknown")
        .to_string();
    let (mut store, instance) = instantiate(engine, module, Vec::new())?;

    let memory = instance
        .get_memory(&mut store, "memory")
        .context("WASM module missing 'memory' export")?;
    let free = instance
        .get_typed_func::<(i32, i32), ()>(&mut store, "claw_free")
        .context("WASM module missing 'claw_free' export")?;
    let get_manifest = instance
        .get_typed_func::<(), i64>(&mut store, "claw_get_manifest")
        .context("WASM module missing 'claw_get_manifest' export")?;

    let manifest_handle = get_manifest.call(&mut store, ())?;
    let (manifest_ptr, manifest_len) = unpack_ptr_len(manifest_handle);
    if manifest_ptr == 0 || manifest_len == 0 {
        bail!("WASM module '{}' returned an empty manifest", fallback_name);
    }

    let mut manifest_bytes = vec![0u8; manifest_len];
    memory.read(&mut store, manifest_ptr, &mut manifest_bytes)?;
    free.call(&mut store, (manifest_ptr as i32, manifest_len as i32))?;

    let manifest = root::<PluginManifest>(&manifest_bytes)
        .context("Failed to decode PluginManifest from WASM module")?;
    let tools =
        serde_json::from_str::<Vec<WasmToolDescriptor>>(manifest.tools_json().unwrap_or("[]"))
            .context("Failed to decode PluginManifest tools_json")?;
    let capabilities = manifest
        .capabilities()
        .map(|items| {
            items
                .iter()
                .map(|item| item.to_string())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Ok(WasmManifest {
        name: manifest.name().unwrap_or(&fallback_name).to_string(),
        tools,
        capabilities,
    })
}

fn instantiate(
    engine: &Engine,
    module: &Module,
    capabilities: Vec<String>,
) -> Result<(Store<WasmState>, Instance)> {
    let wasi = wasi_common::sync::WasiCtxBuilder::new()
        .inherit_stdio()
        .build();

    let mut store = Store::new(
        engine,
        WasmState {
            wasi,
            capabilities,
            http_client: HttpClient::new(),
            http_response: Vec::new(),
        },
    );

    let mut linker = Linker::new(engine);
    wasi_common::sync::add_to_linker(&mut linker, |s: &mut WasmState| &mut s.wasi)?;

    linker.func_wrap(
        "env",
        "host_http_request",
        |mut caller: Caller<'_, WasmState>, ptr: i32, len: i32| -> i32 {
            match host_http_request_impl(&mut caller, ptr, len) {
                Ok(status) => status,
                Err(error) => {
                    caller.data_mut().http_response = error.to_string().into_bytes();
                    -1
                }
            }
        },
    )?;
    linker.func_wrap(
        "env",
        "host_http_response_len",
        |caller: Caller<'_, WasmState>| -> i32 { caller.data().http_response.len() as i32 },
    )?;
    linker.func_wrap(
        "env",
        "host_http_read_response",
        |mut caller: Caller<'_, WasmState>, dst_ptr: i32| -> i32 {
            match host_http_read_response_impl(&mut caller, dst_ptr) {
                Ok(len) => len as i32,
                Err(error) => {
                    caller.data_mut().http_response = error.to_string().into_bytes();
                    -1
                }
            }
        },
    )?;
    linker.func_wrap(
        "env",
        "host_http_get",
        |mut caller: Caller<'_, WasmState>, ptr: i32, len: i32| -> i32 {
            match host_http_get_impl(&mut caller, ptr, len) {
                Ok(status) => status,
                Err(error) => {
                    caller.data_mut().http_response = error.to_string().into_bytes();
                    -1
                }
            }
        },
    )?;

    let instance = linker.instantiate(&mut store, module)?;
    Ok((store, instance))
}

fn host_http_request_impl(caller: &mut Caller<'_, WasmState>, ptr: i32, len: i32) -> Result<i32> {
    let request_bytes = read_guest_bytes(caller, ptr, len)?;
    let request = serde_json::from_slice::<HostHttpRequest>(&request_bytes)
        .context("Failed to decode guest host_http_request payload")?;
    execute_host_http_request(caller, request)
}

fn host_http_get_impl(caller: &mut Caller<'_, WasmState>, ptr: i32, len: i32) -> Result<i32> {
    let url = String::from_utf8(read_guest_bytes(caller, ptr, len)?)
        .context("Failed to decode guest host_http_get URL")?;
    execute_host_http_request(
        caller,
        HostHttpRequest {
            method: "GET".to_string(),
            url,
            headers: BTreeMap::new(),
            body: None,
        },
    )
}

fn execute_host_http_request(
    caller: &mut Caller<'_, WasmState>,
    request: HostHttpRequest,
) -> Result<i32> {
    if !is_http_url_allowed(&caller.data().capabilities, &request.url) {
        bail!(
            "Unauthorized HTTP capability access for URL {}",
            request.url
        );
    }

    let method = Method::from_bytes(request.method.as_bytes())
        .with_context(|| format!("Unsupported HTTP method '{}'", request.method))?;
    let client = caller.data().http_client.clone();
    let url = request.url;
    let headers = request.headers;
    let body = request.body;

    let (status, response_body) = tokio::task::block_in_place(|| {
        let handle = tokio::runtime::Handle::try_current()
            .context("host_http_request requires an active Tokio runtime")?;
        handle.block_on(async move {
            let mut builder = client.request(method, &url);
            for (name, value) in &headers {
                builder = builder.header(name, value);
            }
            if let Some(body) = body {
                builder = builder.body(body);
            }

            let response = builder
                .send()
                .await
                .with_context(|| format!("Host HTTP request to {} failed", url))?;
            let status = response.status().as_u16() as i32;
            let body = response
                .bytes()
                .await
                .context("Failed to read host HTTP response body")?;
            Ok::<(i32, Vec<u8>), anyhow::Error>((status, body.to_vec()))
        })
    })?;

    caller.data_mut().http_response = response_body;
    Ok(status)
}

fn host_http_read_response_impl(caller: &mut Caller<'_, WasmState>, dst_ptr: i32) -> Result<usize> {
    let response = caller.data().http_response.clone();
    let memory = guest_memory(caller)?;
    memory.write(caller, dst_ptr as usize, &response)?;
    Ok(response.len())
}

fn read_guest_bytes(caller: &mut Caller<'_, WasmState>, ptr: i32, len: i32) -> Result<Vec<u8>> {
    if ptr < 0 || len < 0 {
        bail!("Guest memory pointer and length must be non-negative");
    }

    let memory = guest_memory(caller)?;
    let mut bytes = vec![0u8; len as usize];
    memory.read(caller, ptr as usize, &mut bytes)?;
    Ok(bytes)
}

fn guest_memory(caller: &mut Caller<'_, WasmState>) -> Result<wasmtime::Memory> {
    caller
        .get_export("memory")
        .and_then(Extern::into_memory)
        .context("WASM module missing 'memory' export")
}

fn build_plugin_request(tool_name: &str, args: &Value) -> Result<Vec<u8>> {
    let mut builder = FlatBufferBuilder::new();
    let tool_name = builder.create_string(tool_name);
    let args_json = builder.create_string(&serde_json::to_string(args)?);
    let request = PluginRequest::create(
        &mut builder,
        &PluginRequestArgs {
            tool_name: Some(tool_name),
            arguments_json: Some(args_json),
            binary_payload: None,
        },
    );
    builder.finish(request, None);
    Ok(builder.finished_data().to_vec())
}

fn is_http_url_allowed(capabilities: &[String], url: &str) -> bool {
    capabilities.iter().any(|capability| {
        if capability == "http:*" {
            return true;
        }

        capability
            .strip_prefix("http:")
            .map(|prefix| url.starts_with(prefix))
            .unwrap_or(false)
    })
}

fn unpack_ptr_len(value: i64) -> (usize, usize) {
    let raw = value as u64;
    let ptr = (raw >> 32) as u32 as usize;
    let len = (raw & 0xffff_ffff) as u32 as usize;
    (ptr, len)
}
