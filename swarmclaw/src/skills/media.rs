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
