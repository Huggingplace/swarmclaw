use crate::skills::Skill;
use crate::tools::Tool;
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;

#[cfg(feature = "image")]
use image::{DynamicImage, GenericImageView};

#[cfg(feature = "image")]
use mothership_media::{capture_screen, synthesize_speech, transcribe_audio};

// --- Image Resize Tool ---

#[derive(Clone)]
pub struct ImageResizeTool;

#[async_trait]
impl Tool for ImageResizeTool {
    fn name(&self) -> &str {
        "image_resize"
    }

    fn description(&self) -> &str {
        "Resize an image to specific dimensions."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "width": { "type": "integer" },
                "height": { "type": "integer" }
            },
            "required": ["path", "width", "height"]
        })
    }

    async fn execute(&self, args: Value) -> Result<String> {
        #[cfg(feature = "image")]
        {
            let path = args
                .get("path")
                .and_then(|v| v.as_str())
                .context("Missing path")?;
            let width = args
                .get("width")
                .and_then(|v| v.as_u64())
                .context("Missing width")? as u32;
            let height = args
                .get("height")
                .and_then(|v| v.as_u64())
                .context("Missing height")? as u32;

            let img = image::open(path)?;
            let resized = img.resize_exact(width, height, image::imageops::FilterType::Lanczos3);

            let out_path = format!("{}_resized.png", path);
            resized.save(&out_path)?;

            Ok(format!(
                "Successfully resized image and saved to {}",
                out_path
            ))
        }
        #[cfg(not(feature = "image"))]
        {
            let _ = args;
            anyhow::bail!("Image feature not enabled.");
        }
    }
}

// --- Vision (Screen Capture) Tool ---

#[derive(Clone)]
pub struct CaptureScreenTool;

#[async_trait]
impl Tool for CaptureScreenTool {
    fn name(&self) -> &str {
        "capture_screen"
    }

    fn description(&self) -> &str {
        "Captures the primary screen as a PNG image to provide visual context to the LLM."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "save_path": { "type": "string", "description": "Optional path to save the PNG image. If omitted, saves to a temporary location." }
            }
        })
    }

    async fn execute(&self, args: Value) -> Result<String> {
        #[cfg(feature = "image")]
        {
            let save_path = args
                .get("save_path")
                .and_then(|v| v.as_str())
                .unwrap_or("screen_capture.png")
                .to_string();

            let img_bytes = capture_screen().await.context("Failed to capture screen")?;

            std::fs::write(&save_path, &img_bytes)?;

            Ok(format!(
                "Screen successfully captured and saved to {}. You can now analyze it.",
                save_path
            ))
        }
        #[cfg(not(feature = "image"))]
        {
            let _ = args;
            anyhow::bail!("Image feature not enabled.");
        }
    }
}

// --- Voice: Text-to-Speech (TTS) Tool ---

#[derive(Clone)]
pub struct SynthesizeSpeechTool;

#[async_trait]
impl Tool for SynthesizeSpeechTool {
    fn name(&self) -> &str {
        "synthesize_speech"
    }

    fn description(&self) -> &str {
        "Synthesize spoken audio from text using Text-to-Speech (TTS)."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "text": { "type": "string", "description": "The text to speak." },
                "output_path": { "type": "string", "description": "Optional path to save the audio file." }
            },
            "required": ["text"]
        })
    }

    async fn execute(&self, args: Value) -> Result<String> {
        #[cfg(feature = "image")]
        {
            let text = args
                .get("text")
                .and_then(|v| v.as_str())
                .context("Missing text")?;
            let save_path = args
                .get("output_path")
                .and_then(|v| v.as_str())
                .unwrap_or("speech_output.wav")
                .to_string();

            let audio_bytes = synthesize_speech(text)
                .await
                .context("Failed to synthesize speech")?;

            std::fs::write(&save_path, &audio_bytes)?;

            Ok(format!(
                "Speech successfully synthesized and saved to {}.",
                save_path
            ))
        }
        #[cfg(not(feature = "image"))]
        {
            let _ = args;
            anyhow::bail!("Image feature not enabled.");
        }
    }
}

// --- Voice: Speech-to-Text (STT) Tool ---

#[derive(Clone)]
pub struct TranscribeAudioTool;

#[async_trait]
impl Tool for TranscribeAudioTool {
    fn name(&self) -> &str {
        "transcribe_audio"
    }

    fn description(&self) -> &str {
        "Transcribe spoken audio from a file to text using Speech-to-Text (STT)."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "audio_path": { "type": "string", "description": "Path to the audio file." }
            },
            "required": ["audio_path"]
        })
    }

    async fn execute(&self, args: Value) -> Result<String> {
        #[cfg(feature = "image")]
        {
            let path = args
                .get("audio_path")
                .and_then(|v| v.as_str())
                .context("Missing audio_path")?;

            let audio_bytes = std::fs::read(path).context("Failed to read audio file")?;
            let transcript = transcribe_audio(&audio_bytes)
                .await
                .context("Failed to transcribe audio")?;

            Ok(format!("Transcription result:\n\n{}", transcript))
        }
        #[cfg(not(feature = "image"))]
        {
            let _ = args;
            anyhow::bail!("Image feature not enabled.");
        }
    }
}

// --- Media Skill ---


// --- Vision (Analyze Image) Tool ---

#[derive(Clone)]
pub struct AnalyzeImageTool;

#[async_trait]
impl Tool for AnalyzeImageTool {
    fn name(&self) -> &str {
        "analyze_image"
    }

    fn description(&self) -> &str {
        "Analyzes an image file on disk using a Vision model and returns a detailed textual description of its contents. Useful after capture_screen."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "The absolute or relative path to the image file (e.g. PNG or JPG)." },
                "prompt": { "type": "string", "description": "Specific questions to ask about the image, e.g., 'What city is shown on the map?'" }
            },
            "required": ["path", "prompt"]
        })
    }

    async fn execute(&self, args: Value) -> Result<String> {
        let path = args.get("path").and_then(|v| v.as_str()).context("Missing path")?;
        let prompt = args.get("prompt").and_then(|v| v.as_str()).context("Missing prompt")?;
        
        let api_key = std::env::var("GEMINI_API_KEY").unwrap_or_else(|_| std::env::var("API_KEY").unwrap_or_default());
        if api_key.is_empty() {
            anyhow::bail!("GEMINI_API_KEY environment variable is required to analyze images.");
        }

        let img_bytes = std::fs::read(path).context("Failed to read image file")?;
        let base64_img = base64::encode(&img_bytes);
        
        let mime_type = if path.to_lowercase().ends_with(".png") { "image/png" } else { "image/jpeg" };

        let payload = serde_json::json!({
            "contents": [{
                "parts": [
                    { "text": prompt },
                    {
                        "inline_data": {
                            "mime_type": mime_type,
                            "data": base64_img
                        }
                    }
                ]
            }]
        });

        let client = reqwest::Client::new();
        let url = format!("https://generativelanguage.googleapis.com/v1beta/models/gemini-1.5-flash:generateContent?key={}", api_key);
        
        let res = client.post(&url)
            .header("Content-Type", "application/json")
            .json(&payload)
            .send()
            .await?;

        if !res.status().is_success() {
            let status = res.status();
            let text = res.text().await.unwrap_or_default();
            anyhow::bail!("Vision API failed with status {}: {}", status, text);
        }

        let json: Value = res.json().await?;
        
        if let Some(text) = json.pointer("/candidates/0/content/parts/0/text").and_then(|v| v.as_str()) {
            Ok(text.to_string())
        } else {
            Ok(format!("Received response, but could not parse text: {}", json))
        }
    }
}

pub struct MediaSkill {
    tools: Vec<Arc<dyn Tool>>,
}

impl MediaSkill {
    pub fn new() -> Self {
        Self {
            tools: vec![
                Arc::new(ImageResizeTool),
                Arc::new(CaptureScreenTool),
                Arc::new(SynthesizeSpeechTool),
                Arc::new(TranscribeAudioTool),
                Arc::new(AnalyzeImageTool),
            ],
        }
    }
}

#[async_trait]
impl Skill for MediaSkill {
    fn name(&self) -> &str {
        "media"
    }

    fn description(&self) -> &str {
        "Tools for processing images, screen capture, and audio (TTS/STT)."
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.tools.clone()
    }
}
