use anyhow::{Result, Context};
use std::path::PathBuf;
use std::fs;
use reqwest::Client;
use futures::StreamExt;
use std::io::Write;

pub struct ModelFetcher {
    models_dir: PathBuf,
    client: Client,
}

impl ModelFetcher {
    pub fn new(workspace_path: &PathBuf) -> Self {
        let models_dir = workspace_path.join("models");
        fs::create_dir_all(&models_dir).unwrap_or_default(); // Ensure dir exists
        Self {
            models_dir,
            client: Client::new(),
        }
    }

    pub async fn ensure_model(&self, model_identifier: &str) -> Result<PathBuf> {
        // 1. Sanitize model name for filename
        let filename = model_identifier.replace("/", "_") + ".gguf";
        let model_path = self.models_dir.join(&filename);

        if model_path.exists() {
            println!("✅ Model found locally: {:?}", model_path);
            return Ok(model_path);
        }

        println!("⬇️ Model not found. Fetching '{}'...", model_identifier);

        // 2. Resolve URL
        // For MVP, we assume model_identifier is either a direct URL or a HuggingFace repo/file ID.
        // If it starts with http, use it. Otherwise, assume HuggingFace GGUF default convention or HuggingPlace.
        
        let url = if model_identifier.starts_with("http") {
            model_identifier.to_string()
        } else {
            // Default to a known quantization from HuggingFace if just a name is given
            // Example: "TheBloke/Llama-2-7B-Chat-GGUF" -> assumes main file. 
            // Real implementation needs more robust resolving logic.
            format!("https://huggingface.co/{}/resolve/main/model.gguf", model_identifier)
        };

        // 3. Download
        let res = self.client.get(&url).send().await
            .context(format!("Failed to fetch model from {}", url))?;

        if !res.status().is_success() {
            anyhow::bail!("Failed to download model: HTTP {}", res.status());
        }

        let total_size = res.content_length().unwrap_or(0);
        let mut stream = res.bytes_stream();
        let mut file = fs::File::create(&model_path)?;
        let mut downloaded: u64 = 0;

        while let Some(item) = stream.next().await {
            let chunk = item?;
            file.write_all(&chunk)?;
            downloaded += chunk.len() as u64;
            
            if total_size > 0 {
                let percent = (downloaded as f64 / total_size as f64) * 100.0;
                 // Simple progress logging (can be noisy, maybe limit freq)
                if downloaded % (10 * 1024 * 1024) == 0 { // Log every 10MB
                    print!("\rDownloading... {:.1}% ({}/{} bytes)", percent, downloaded, total_size);
                    std::io::stdout().flush()?;
                }
            }
        }
        
        println!("
✅ Download complete: {:?}", model_path);
        Ok(model_path)
    }
}
