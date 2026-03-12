# How to Build WASM Plugins for SwarmClaw

SwarmClaw utilizes **WebAssembly (WASM)** and the **Model Context Protocol (MCP)** to execute untrusted community skills with mathematical certainty that they cannot escape their sandbox or access unauthorized secrets. 

To achieve maximum performance (specifically for AI agents handling large media like images or audio), SwarmClaw does **not** use standard JSON-over-memory passing. Instead, it relies on a strict **Zero-Copy FlatBuffers ABI**.

This guide explains how to write a highly optimized WASM plugin for SwarmClaw.

## The Architecture: FlatBuffers over WASM Memory

When SwarmClaw wants to execute your skill, it does not pass strings. It passes a single integer: a pointer to a FlatBuffer in your plugin's memory space.

### The Schema Contract (`plugin.fbs`)
Every plugin must understand this schema:

```flatbuffers
namespace swarmclaw_plugin;

table PluginRequest {
  tool_name: string;
  arguments_json: string;
  binary_payload: [ubyte]; // Zero-copy media
}

table PluginResponse {
  success: bool;
  result_json: string;
  binary_artifact: [ubyte];
  error_message: string;
}

root_type PluginRequest;
```

## Step-by-Step Implementation (Rust Guest)

To write a plugin in Rust, you compile your code to the `wasm32-wasip1` target.

### 1. Project Setup
```bash
cargo new my_skill --lib
cd my_skill
cargo add flatbuffers serde_json
```

Add this to your `Cargo.toml`:
```toml
[lib]
crate-type = ["cdylib"] # Required for WASM plugins

[dependencies]
flatbuffers = "23.5.26"
```

### 2. Export the Memory Allocators
Because the host (SwarmClaw) needs to write the FlatBuffer *into* your plugin's memory before executing it, your plugin must export `claw_malloc` and `claw_free`.

```rust
use std::alloc::{alloc, dealloc, Layout};

#[no_mangle]
pub extern "C" fn claw_malloc(size: usize) -> *mut u8 {
    let layout = Layout::from_size_align(size, 1).unwrap();
    unsafe { alloc(layout) }
}

#[no_mangle]
pub extern "C" fn claw_free(ptr: *mut u8, size: usize) {
    let layout = Layout::from_size_align(size, 1).unwrap();
    unsafe { dealloc(ptr, layout) }
}
```

### 3. Implement the Execute Function
Your plugin must export `claw_execute`. This function receives the pointer and length of the `PluginRequest` FlatBuffer, processes it, and returns a pointer to a `PluginResponse` FlatBuffer.

```rust
// Assume you compiled plugin.fbs into plugin_generated.rs using `flatc`
mod plugin_generated;
use plugin_generated::swarmclaw_plugin::{root_as_plugin_request, PluginResponseBuilder, PluginResponseArgs};
use flatbuffers::FlatBufferBuilder;

#[no_mangle]
pub extern "C" fn claw_execute(req_ptr: *const u8, req_len: usize) -> *const u8 {
    // 1. Read the Zero-Copy FlatBuffer from memory
    let req_bytes = unsafe { std::slice::from_raw_parts(req_ptr, req_len) };
    let request = root_as_plugin_request(req_bytes).unwrap();
    
    // 2. Route the tool call
    let tool = request.tool_name().unwrap_or("");
    let args = request.arguments_json().unwrap_or("{}");
    
    let mut builder = FlatBufferBuilder::new();
    let mut success = false;
    let mut result_json = builder.create_string("{}");
    let mut error_msg = None;

    if tool == "analyze_image" {
        // ZERO-COPY MAGIC: Access the massive image buffer instantly without parsing!
        if let Some(image_data) = request.binary_payload() {
            // ... process image_data.bytes() with OpenCV ...
            result_json = builder.create_string(r#"{"labels": ["dog", "park"]}"#);
            success = true;
        } else {
            error_msg = Some(builder.create_string("No image provided"));
        }
    } else {
        error_msg = Some(builder.create_string("Unknown tool"));
    }

    // 3. Build the Response FlatBuffer
    let response = PluginResponseBuilder::new(&mut builder)
        .add_success(success)
        .add_result_json(result_json);
        
    if let Some(err) = error_msg {
        // ... (builder syntax omitted for brevity)
    }
    
    // 4. Return the pointer to the host (Note: in a real implementation, 
    // you must also return the length, typically by packing it in an i64 or using shared memory)
    // ...
}
```

## Security & Capabilities

Your plugin operates in a strict "Default Deny" sandbox. If you need to make an HTTP request, you **cannot** use `reqwest` directly. 

You must declare your required capabilities in your manifest (e.g., `http:api.github.com`), and then use the SwarmClaw host functions:

```rust
extern "C" {
    fn host_http_get(url_ptr: *const u8, url_len: usize) -> i32;
}
```
If you attempt to call this without the user explicitly granting the capability, `wasmtime` will panic and kill your plugin instance immediately.

## Further Optimizations (Roadmap)

To ensure SwarmClaw remains the fastest agent runtime, we are pursuing the following optimizations:

1. **Shared Memory (Reference Counting):** Currently, FlatBuffers are zero-copy *during execution*, but the host still has to copy the bytes into the WASM Linear Memory initially. Future versions will explore WebAssembly Memory64 and Shared Memory (`SharedArrayBuffer`) to allow the Host and Guest to read the exact same physical RAM address.
2. **WASI-NN (Neural Network) Integration:** For vision tasks, rather than compiling a heavy library like OpenCV directly into the WASM payload, SwarmClaw will expose hardware-accelerated inferencing via the `wasi-nn` standard. This allows a 2MB WASM plugin to instantly utilize the host machine's dedicated GPU/NPU for object detection.
3. **AOT (Ahead-of-Time) Caching via Mothership:** When a SwarmClaw agent boots up, Mothership will use `wasmtime` to AOT compile the `.wasm` into native machine code (e.g., `.cwasm`) on the Carrier daemon, meaning execution starts in microseconds rather than milliseconds.
