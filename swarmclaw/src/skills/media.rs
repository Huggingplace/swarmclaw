use async_trait::async_trait;
use crate::tools::Tool;
use crate::skills::Skill;
use std::sync::Arc;
use serde_json::Value;
use anyhow::{Result, Context};

#[cfg(feature = "image")]
use image::{DynamicImage, GenericImageView};

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
            let path = args.get("path").and_then(|v| v.as_str()).context("Missing path")?;
            let width = args.get("width").and_then(|v| v.as_u64()).context("Missing width")? as u32;
            let height = args.get("height").and_then(|v| v.as_u64()).context("Missing height")? as u32;

            let img = image::open(path)?;
            let resized = img.resize_exact(width, height, image::imageops::FilterType::Lanczos3);
            
            let out_path = format!("{}_resized.png", path);
            resized.save(&out_path)?;

            Ok(format!("Successfully resized image and saved to {}", out_path))
        }
        #[cfg(not(feature = "image"))]
        {
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
            tools: vec![Arc::new(ImageResizeTool)],
        }
    }
}

#[async_trait]
impl Skill for MediaSkill {
    fn name(&self) -> &str {
        "media"
    }

    fn description(&self) -> &str {
        "Tools for processing images and media."
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.tools.clone()
    }
}
