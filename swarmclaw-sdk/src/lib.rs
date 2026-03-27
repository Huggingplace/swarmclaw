use anyhow::{bail, Context, Result};
use flatbuffers::FlatBufferBuilder;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use std::alloc::{alloc, dealloc, Layout};
use std::collections::BTreeMap;
use std::mem;
use std::slice;

pub mod plugin_generated;

use plugin_generated::swarmclaw_plugin::{
    root_as_plugin_request, PluginManifest, PluginManifestArgs, PluginResponse, PluginResponseArgs,
};

// --- Host Imports ---

#[link(wasm_import_module = "env")]
extern "C" {
    /// Perform a host-mediated HTTP request described by a JSON payload.
    pub fn host_http_request(ptr: i32, len: i32) -> i32;

    /// Return the byte length of the last host HTTP response body.
    pub fn host_http_response_len() -> i32;

    /// Copy the last host HTTP response body into guest memory.
    pub fn host_http_read_response(dst_ptr: i32) -> i32;

    /// Legacy GET-only helper. Kept for compatibility with earlier experiments.
    pub fn host_http_get(ptr: i32, len: i32) -> i32;
}

// --- Guest Exports (The ABI) ---

/// Allocates memory inside the WASM linear memory so the host can write
/// the FlatBuffers request payload directly, achieving zero-copy transfer.
#[no_mangle]
pub extern "C" fn claw_malloc(size: usize) -> *mut u8 {
    let align = mem::align_of::<usize>();
    let layout = Layout::from_size_align(size, align).unwrap();
    unsafe { alloc(layout) }
}

/// Frees memory allocated by `claw_malloc`.
#[no_mangle]
pub extern "C" fn claw_free(ptr: *mut u8, size: usize) {
    let align = mem::align_of::<usize>();
    let layout = Layout::from_size_align(size, align).unwrap();
    unsafe {
        dealloc(ptr, layout);
    }
}

// --- Developer API ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

impl ToolDefinition {
    pub fn new(name: impl Into<String>, description: impl Into<String>, parameters: Value) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
}

impl HttpRequest {
    pub fn new(method: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            method: method.into(),
            url: url.into(),
            headers: BTreeMap::new(),
            body: None,
        }
    }

    pub fn post_json(url: impl Into<String>, body: &Value) -> Result<Self> {
        let mut request = Self::new("POST", url);
        request
            .headers
            .insert("content-type".to_string(), "application/json".to_string());
        request.body =
            Some(serde_json::to_string(body).context("Failed to serialize HTTP JSON body")?);
        Ok(request)
    }

    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.insert(name.into(), value.into());
        self
    }
}

#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: i32,
    pub body: Vec<u8>,
}

impl HttpResponse {
    pub fn text(&self) -> Result<String> {
        String::from_utf8(self.body.clone()).context("Host HTTP response was not valid UTF-8")
    }

    pub fn json<T: DeserializeOwned>(&self) -> Result<T> {
        serde_json::from_slice(&self.body).context("Failed to decode host HTTP JSON response")
    }
}

#[derive(Debug, Clone)]
pub struct DecodedRequest {
    pub tool_name: String,
    pub arguments: Value,
    pub binary_payload: Option<Vec<u8>>,
}

/// A trait that all SwarmClaw Skills must implement.
pub trait SwarmClawSkill {
    fn name(&self) -> &str;
    fn description(&self) -> &str;

    fn tools(&self) -> Vec<ToolDefinition> {
        vec![ToolDefinition::new(
            self.name(),
            self.description(),
            json!({ "type": "object", "properties": {} }),
        )]
    }

    fn capabilities(&self) -> Vec<String> {
        Vec::new()
    }

    fn execute(&self, args: Value) -> Result<String>;

    fn execute_tool(&self, _tool_name: &str, args: Value) -> Result<String> {
        self.execute(args)
    }
}

pub fn decode_request(req_ptr: *const u8, req_len: usize) -> Result<DecodedRequest> {
    let req_bytes = unsafe { slice::from_raw_parts(req_ptr, req_len) };
    let request = root_as_plugin_request(req_bytes).context("Failed to decode PluginRequest")?;

    let arguments = serde_json::from_str::<Value>(request.arguments_json().unwrap_or("{}"))
        .context("Failed to decode PluginRequest arguments_json")?;
    let binary_payload = request
        .binary_payload()
        .map(|payload| payload.iter().collect::<Vec<_>>());

    Ok(DecodedRequest {
        tool_name: request.tool_name().unwrap_or("").to_string(),
        arguments,
        binary_payload,
    })
}

pub fn export_manifest(skill: &dyn SwarmClawSkill) -> i64 {
    build_manifest(skill).unwrap_or(0)
}

pub fn export_execute(skill: &dyn SwarmClawSkill, req_ptr: *const u8, req_len: usize) -> i64 {
    match execute_skill(skill, req_ptr, req_len) {
        Ok(result) => result,
        Err(error) => {
            let message = error.to_string();
            build_response(false, "{}", Some(message.as_str()), None).unwrap_or(0)
        }
    }
}

pub fn host_http(request: &HttpRequest) -> Result<HttpResponse> {
    let payload = serde_json::to_vec(request).context("Failed to encode host HTTP request")?;
    let status = unsafe { host_http_request(payload.as_ptr() as i32, payload.len() as i32) };
    let response_len = unsafe { host_http_response_len() };
    let mut body = vec![0u8; response_len.max(0) as usize];

    if response_len > 0 {
        let copied = unsafe { host_http_read_response(body.as_mut_ptr() as i32) };
        if copied < 0 {
            bail!("Host HTTP response copy failed");
        }
        body.truncate(copied as usize);
    }

    if status < 0 {
        let detail = if body.is_empty() {
            "host HTTP request failed".to_string()
        } else {
            String::from_utf8_lossy(&body).to_string()
        };
        bail!("{}", detail);
    }

    Ok(HttpResponse { status, body })
}

fn execute_skill(skill: &dyn SwarmClawSkill, req_ptr: *const u8, req_len: usize) -> Result<i64> {
    let request = decode_request(req_ptr, req_len)?;
    let result = skill
        .execute_tool(&request.tool_name, request.arguments)
        .with_context(|| format!("Plugin tool '{}' failed", request.tool_name))?;
    build_response(true, &result, None, None)
}

fn build_manifest(skill: &dyn SwarmClawSkill) -> Result<i64> {
    let mut builder = FlatBufferBuilder::new();
    let name = builder.create_string(skill.name());
    let description = builder.create_string(skill.description());
    let tools_json = builder.create_string(
        &serde_json::to_string(&skill.tools()).context("Failed to encode plugin tools JSON")?,
    );

    let capabilities = skill.capabilities();
    let capability_offsets = capabilities
        .iter()
        .map(|capability| builder.create_string(capability))
        .collect::<Vec<_>>();
    let capabilities_offset = if capability_offsets.is_empty() {
        None
    } else {
        Some(builder.create_vector(&capability_offsets))
    };

    let manifest = PluginManifest::create(
        &mut builder,
        &PluginManifestArgs {
            name: Some(name),
            description: Some(description),
            tools_json: Some(tools_json),
            capabilities: capabilities_offset,
        },
    );
    builder.finish(manifest, None);
    finish_builder(builder)
}

fn build_response(
    success: bool,
    result_json: &str,
    error_message: Option<&str>,
    binary_artifact: Option<&[u8]>,
) -> Result<i64> {
    let mut builder = FlatBufferBuilder::new();
    let result_json_offset = builder.create_string(result_json);
    let error_message_offset = error_message.map(|value| builder.create_string(value));
    let binary_artifact_offset = binary_artifact.map(|bytes| builder.create_vector(bytes));

    let response = PluginResponse::create(
        &mut builder,
        &PluginResponseArgs {
            success,
            result_json: Some(result_json_offset),
            binary_artifact: binary_artifact_offset,
            error_message: error_message_offset,
        },
    );
    builder.finish(response, None);
    finish_builder(builder)
}

fn finish_builder(builder: FlatBufferBuilder<'_>) -> Result<i64> {
    let data = builder.finished_data();
    let len = data.len();
    let ptr = claw_malloc(len);
    if ptr.is_null() {
        bail!("Failed to allocate plugin response buffer");
    }

    unsafe {
        std::ptr::copy_nonoverlapping(data.as_ptr(), ptr, len);
    }

    Ok(pack_ptr_len(ptr as usize, len))
}

pub fn pack_ptr_len(ptr: usize, len: usize) -> i64 {
    (((ptr as u64) << 32) | (len as u32 as u64)) as i64
}

pub fn unpack_ptr_len(value: i64) -> (usize, usize) {
    let raw = value as u64;
    let ptr = (raw >> 32) as u32 as usize;
    let len = (raw & 0xffff_ffff) as u32 as usize;
    (ptr, len)
}
