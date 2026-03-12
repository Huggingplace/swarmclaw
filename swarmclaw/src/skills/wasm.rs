use async_trait::async_trait;
use crate::tools::Tool;
use crate::skills::Skill;
use std::sync::Arc;
use std::path::PathBuf;
use anyhow::{Result, Context};
use wasmtime::{Engine, Module, Store, Linker, Caller, Memory};
use wasi_common::WasiCtx;
use serde_json::Value;

// Import the generated FlatBuffers code
#[allow(dead_code, unused_imports)]
#[path = "../plugin_generated.rs"]
mod plugin_generated;
use plugin_generated::swarmclaw_plugin::{PluginRequestArgs, PluginRequest, root_as_plugin_request};
use flatbuffers::FlatBufferBuilder;

// Host State for WASM Instance
struct WasmState {
    wasi: WasiCtx,
    name: String,
    capabilities: Vec<String>,
}

// Wrapper for a specific tool inside a WASM module
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
        let wasi = wasi_common::sync::WasiCtxBuilder::new()
            .inherit_stdio()
            .build();
            
        let mut store = Store::new(&self.engine, WasmState { 
            wasi,
            name: self.name.clone(),
            capabilities: self.capabilities.clone(),
        });
        
        let mut linker = Linker::new(&self.engine);
        wasi_common::sync::add_to_linker(&mut linker, |s: &mut WasmState| &mut s.wasi)?;

        // Enforce capabilities at the host level
        linker.func_wrap("env", "host_http_get", |caller: Caller<'_, WasmState>, _ptr: i32, _len: i32| -> i32 {
            let caps = &caller.data().capabilities;
            if !caps.iter().any(|c| c.starts_with("http:")) {
                tracing::error!("SECURITY ALERT: WASM module tried to use HTTP without the required capability!");
                panic!("Unauthorized Capability Access: HTTP");
            }
            0
        })?;

        let instance = linker.instantiate(&mut store, &self.module)?;
        
        let memory = instance.get_memory(&mut store, "memory")
            .context("WASM module missing 'memory' export")?;
            
        let malloc = instance.get_typed_func::<i32, i32>(&mut store, "claw_malloc")
            .context("WASM module missing 'claw_malloc' export")?;
            
        let execute = instance.get_typed_func::<(i32, i32), i32>(&mut store, "claw_execute")
            .context("WASM module missing 'claw_execute' export")?;

        // 1. Build the FlatBuffer Request
        let mut builder = FlatBufferBuilder::new();
        let tool_name = builder.create_string(&self.name);
        let args_json = builder.create_string(&serde_json::to_string(&args)?);
        
        let args_offset = PluginRequest::create(&mut builder, &PluginRequestArgs {
            tool_name: Some(tool_name),
            arguments_json: Some(args_json),
            binary_payload: None,
        });
            
        builder.finish(args_offset, None);
        let buf = builder.finished_data();
        let buf_len = buf.len() as i32;
        
        // 2. Allocate memory in guest
        let ptr = malloc.call(&mut store, buf_len)?;
        
        // 3. Write FlatBuffer directly into WASM memory
        memory.write(&mut store, ptr as usize, buf)?;
        
        // 4. Execute tool
        let res_ptr = execute.call(&mut store, (ptr, buf_len))?;
        
        // TODO: In a real implementation, we need the guest to return both the pointer AND the length of the result buffer.
        // For now, we simulate reading a fixed length or finding a terminator to parse the PluginResponse FlatBuffer.
        
        Ok(format!("WASM Tool Executed via FlatBuffers (Result Ptr: {})", res_ptr))
    }
}

pub struct WasmSkill {
    name: String,
    tools: Vec<Arc<dyn Tool>>,
}

impl WasmSkill {
    pub fn new(path: PathBuf) -> Result<Self> {
        let engine = Engine::default();
        let module = Module::from_file(&engine, &path)
            .context("Failed to load WASM module")?;
        
        let name = path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();

        // Discovery: Instantiate momentarily to call `claw_get_manifest`
        // For MVP, we'll just mock a single tool based on filename
        let mock_tool = WasmTool {
            name: format!("{}_tool", name),
            description: format!("Dynamically loaded tool from {}", name),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "input": { "type": "string" }
                }
            }),
            engine: engine.clone(),
            module: module.clone(),
            capabilities: vec!["fs:read:/tmp".into()], // Mock capability
        };

        Ok(Self {
            name,
            tools: vec![Arc::new(mock_tool)],
        })
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
