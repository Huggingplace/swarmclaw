use std::path::Path;
use std::process::Command;

fn main() {
    let schema_path = "schemas/agent.fbs";
    let out_dir = "src";

    if Path::new(schema_path).exists() {
        println!("cargo:rerun-if-changed={}", schema_path);

        let status = Command::new("flatc")
            .args(["--rust", "-o", out_dir, schema_path])
            .status()
            .expect("Failed to execute flatc");

        if !status.success() {
            panic!("flatc failed with status: {}", status);
        }
    }
}
