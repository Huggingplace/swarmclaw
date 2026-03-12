use anyhow::Result;

/// The SafetyLayer acts as an AI firewall, scrubbing user inputs
/// before they reach the LLM to prevent prompt injection attacks.
pub struct SafetyLayer;

impl SafetyLayer {
    pub fn scrub_prompt(prompt: &str) -> Result<String> {
        let lower = prompt.to_lowercase();
        
        // Fast heuristic checks for common prompt injection vectors
        if lower.contains("ignore all previous") 
            || lower.contains("system prompt") 
            || lower.contains("disregard previous instructions") {
            anyhow::bail!("SECURITY ALERT: Prompt injection detected. Request rejected.");
        }
        
        // In a production system, this could also call a fast, local lightweight
        // classifier model (e.g. ONNX) to determine intent.

        Ok(prompt.to_string())
    }
}