use std::alloc::{alloc, dealloc, Layout};
use std::mem;
use std::slice;
use serde_json::Value;
use anyhow::Result;

// --- Host Imports ---

#[link(wasm_import_module = "env")]
extern "C" {
    /// Allows the WASM skill to request a host capability (like an HTTP GET)
    /// if the host environment permits it.
    pub fn host_http_get(ptr: i32, len: i32) -> i32;
}

// --- Guest Exports (The ABI) ---

/// Allocates memory inside the WASM linear memory so the host can write 
/// the FlatBuffers request payload directly, achieving zero-copy transfer.
#[no_mangle]
pub extern "C" fn claw_malloc(size: usize) -> *mut u8 {
    let align = mem::align_of::<usize>();
    let layout = Layout::from_size_align(size, align).unwrap();
    unsafe {
        let ptr = alloc(layout);
        ptr
    }
}

/// Frees memory allocated by `claw_malloc` (usually called by the host after reading the result).
#[no_mangle]
pub extern "C" fn claw_free(ptr: *mut u8, size: usize) {
    let align = mem::align_of::<usize>();
    let layout = Layout::from_size_align(size, align).unwrap();
    unsafe {
        dealloc(ptr, layout);
    }
}

// --- Developer API (Macros will generate this part eventually) ---

/// A trait that all SwarmClaw Skills must implement.
pub trait SwarmClawSkill {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn execute(&self, args: Value) -> Result<String>;
}
