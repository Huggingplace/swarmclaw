//! Programmatic tool pipeline: host-side multi-step tool execution.
//!
//! This module implements a tractable form of Hermes Agent's "code execution /
//! programmatic tool calling". Instead of the model issuing one tool call,
//! observing the (possibly huge) result, then issuing the next call — paying
//! context for every intermediate result — the model submits a whole plan up
//! front. The plan's steps execute SEQUENTIALLY host-side, each step's output
//! can be chained into a later step's arguments via `${step_id}` references, and
//! ONLY the outputs of steps marked `return: true` (or the last step, if none
//! are marked) ever come back into the model's context.
//!
//! The key property: INTERMEDIATE results never enter the model's context. A
//! pipeline that reads a 50k-line file, greps it, and returns three matching
//! lines costs the model only those three lines — the file dump stays host-side.
//!
//! SAFETY: unlike [`crate::core::delegation::DelegateTaskTool`], the pipeline
//! spawns NO sub-agents and makes NO LLM calls — it only invokes tools the
//! agent already has. It is therefore a pure efficiency wrapper and is added
//! UNGATED. The one invariant it must uphold is RECURSION EXCLUSION: the
//! pipeline must never be able to call itself, so the callable-tool map is built
//! excluding any tool named `tool_pipeline` (see [`PipelineTool::new`] and the
//! wiring in `agent::assemble_tools`).

use crate::core::context::{cap_tool_result, MAX_TOOL_RESULT_TOKENS};
use crate::tools::Tool;
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::Arc;

/// A single parsed step of a pipeline plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipelineStep {
    /// Optional id; when present, this step's output is stored under it so later
    /// steps can reference it via `${id}`. When absent, the step still runs but
    /// its output cannot be referenced.
    pub id: Option<String>,
    /// Name of the tool to invoke (required, must resolve in the tool map).
    pub tool: String,
    /// Arguments passed to the tool, after `${ref}` substitution.
    pub args: Value,
    /// Whether this step's output should be included in the returned result.
    pub return_: bool,
}

/// Tool that runs a sequence of tool steps host-side, chaining outputs via
/// `${step_id}` references, and returns only the selected (or last) outputs so
/// intermediate results never consume the model's context.
pub struct PipelineTool {
    /// name -> tool, built from the agent's OTHER tools (excluding the pipeline
    /// itself, see RECURSION EXCLUSION in the module docs).
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl PipelineTool {
    /// Build a pipeline from a name->tool map. Any entry named `tool_pipeline`
    /// is defensively dropped so the pipeline can never call itself.
    pub fn new(mut tools: HashMap<String, Arc<dyn Tool>>) -> Self {
        tools.remove("tool_pipeline");
        Self { tools }
    }

    /// Build a pipeline from a slice of tools (the agent's collected tools),
    /// excluding any tool named `tool_pipeline` (RECURSION EXCLUSION). Later
    /// duplicates of the same name overwrite earlier ones.
    pub fn from_tools(tools: &[Arc<dyn Tool>]) -> Self {
        let map: HashMap<String, Arc<dyn Tool>> = tools
            .iter()
            .filter(|t| t.name() != "tool_pipeline")
            .map(|t| (t.name().to_string(), t.clone()))
            .collect();
        Self::new(map)
    }
}

/// Recursively replace `${id}` tokens inside string values of `args` with the
/// corresponding prior step's output from `outputs`.
///
/// Only string scalars are scanned; objects and arrays are walked recursively;
/// all other JSON value kinds (numbers, bools, null) are returned untouched.
///
/// UNKNOWN REFERENCE POLICY: a `${id}` whose `id` is not present in `outputs` is
/// left verbatim in the string (it is NOT replaced with empty). This makes a
/// typo'd reference visible to the tool/model rather than silently vanishing.
pub fn substitute_refs(args: &Value, outputs: &HashMap<String, String>) -> Value {
    match args {
        Value::String(s) => Value::String(substitute_in_str(s, outputs)),
        Value::Array(arr) => {
            Value::Array(arr.iter().map(|v| substitute_refs(v, outputs)).collect())
        }
        Value::Object(map) => {
            let mut out = Map::with_capacity(map.len());
            for (k, v) in map {
                out.insert(k.clone(), substitute_refs(v, outputs));
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

/// Replace every `${id}` occurrence in a single string. Manual scan (no regex
/// dependency assumptions): finds `${`, reads up to the matching `}`, and if the
/// enclosed name is a known output substitutes it; otherwise leaves the token
/// (including the braces) verbatim.
fn substitute_in_str(s: &str, outputs: &HashMap<String, String>) -> String {
    let bytes = s.as_bytes();
    let mut result = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' && i + 1 < bytes.len() && bytes[i + 1] == b'{' {
            // Find the closing brace.
            if let Some(close_rel) = s[i + 2..].find('}') {
                let close = i + 2 + close_rel;
                let name = &s[i + 2..close];
                if let Some(val) = outputs.get(name) {
                    result.push_str(val);
                    i = close + 1;
                    continue;
                }
                // Unknown ref: leave the whole `${name}` token verbatim.
                result.push_str(&s[i..close + 1]);
                i = close + 1;
                continue;
            }
            // No closing brace: emit the rest verbatim.
            result.push_str(&s[i..]);
            break;
        }
        // Push this whole char (handle multibyte safely).
        let ch_len = utf8_char_len(bytes[i]);
        result.push_str(&s[i..i + ch_len]);
        i += ch_len;
    }
    result
}

/// Length in bytes of a UTF-8 char given its leading byte.
fn utf8_char_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b >> 5 == 0b110 {
        2
    } else if b >> 4 == 0b1110 {
        3
    } else if b >> 3 == 0b11110 {
        4
    } else {
        1
    }
}

/// Parse and validate the `steps` array from the tool arguments.
///
/// Errors (returned as `Err(String)`, surfaced gracefully by `execute`):
/// - `steps` missing or not an array,
/// - `steps` empty,
/// - any step that is not an object, lacks a non-empty string `tool`, or whose
///   `args` (when present) is not an object.
///
/// `id` is optional (string); `args` defaults to an empty object when absent;
/// `return` defaults to false.
pub fn parse_steps(args: &Value) -> Result<Vec<PipelineStep>, String> {
    let Some(arr) = args.get("steps").and_then(|s| s.as_array()) else {
        return Err("missing or invalid `steps`: expected a non-empty array of steps".to_string());
    };
    if arr.is_empty() {
        return Err("`steps` is empty: provide at least one step".to_string());
    }

    let mut steps = Vec::with_capacity(arr.len());
    for (idx, raw) in arr.iter().enumerate() {
        let Some(obj) = raw.as_object() else {
            return Err(format!("step {idx} is not an object"));
        };

        let tool = obj
            .get("tool")
            .and_then(|t| t.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| format!("step {idx} is missing a non-empty string `tool`"))?;

        let id = obj
            .get("id")
            .and_then(|i| i.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let args = match obj.get("args") {
            None | Some(Value::Null) => json!({}),
            Some(v @ Value::Object(_)) => v.clone(),
            Some(_) => {
                return Err(format!("step {idx} has a non-object `args`"));
            }
        };

        let return_ = obj.get("return").and_then(|r| r.as_bool()).unwrap_or(false);

        steps.push(PipelineStep {
            id,
            tool,
            args,
            return_,
        });
    }

    Ok(steps)
}

/// Select the outputs to return from the per-step outputs collected in plan
/// order.
///
/// `step_outputs` is one `(id, output)` pair per executed step, in order, where
/// `id` is the step's id (empty string for an id-less step). Selection:
/// - the outputs of all `return: true` steps, in plan order;
/// - if none are marked, the LAST step's output (its id, or empty string).
///
/// Returns `(id, output)` pairs. `steps` and `step_outputs` are positionally
/// aligned (same length, same order), so an id-less step's output is still
/// returned correctly via the last-step fallback.
pub fn select_returns(
    steps: &[PipelineStep],
    step_outputs: &[(String, String)],
) -> Vec<(String, String)> {
    let marked: Vec<(String, String)> = steps
        .iter()
        .zip(step_outputs.iter())
        .filter(|(s, _)| s.return_)
        .map(|(_, out)| out.clone())
        .collect();

    if !marked.is_empty() {
        return marked;
    }

    // Fallback: last step's output (regardless of whether it had an id).
    step_outputs.last().cloned().into_iter().collect()
}

#[async_trait]
impl Tool for PipelineTool {
    fn name(&self) -> &str {
        "tool_pipeline"
    }

    fn description(&self) -> &str {
        "Run a fixed SEQUENCE of tool calls host-side in one shot, chaining each \
         step's output into later steps, and get back ONLY the steps you mark \
         (or the last step). Use this for KNOWN multi-step pipelines where the \
         intermediate results are large or uninteresting and you don't need to \
         see them — e.g. read a file, transform it, then return just the final \
         summary. Provide a `steps` array; each step has an optional `id` (so \
         later steps can reference its output as `${id}` inside their `args`), a \
         required `tool` (the name of another tool you have), a required `args` \
         object, and an optional `return` (true to include this step's output in \
         the result). If no step sets `return`, only the last step's output is \
         returned. Steps run in order; later steps may use `${earlier_id}` in \
         their string arguments. Intermediate outputs NEVER enter your context, \
         which saves tokens. Cannot call itself."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "steps": {
                    "type": "array",
                    "description": "Ordered steps to execute host-side.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": {
                                "type": "string",
                                "description": "Optional id; later steps reference this step's output as ${id}."
                            },
                            "tool": {
                                "type": "string",
                                "description": "Name of the tool to invoke (must be a tool you have)."
                            },
                            "args": {
                                "type": "object",
                                "description": "Arguments for the tool. String values may contain ${id} references to earlier steps' outputs."
                            },
                            "return": {
                                "type": "boolean",
                                "description": "If true, include this step's output in the returned result. Defaults to false."
                            }
                        },
                        "required": ["tool", "args"]
                    }
                }
            },
            "required": ["steps"]
        })
    }

    async fn execute(&self, args: Value) -> Result<String> {
        let steps = match parse_steps(&args) {
            Ok(s) => s,
            Err(e) => return Ok(format!("[tool_pipeline] error: {e}")),
        };

        // Outputs accumulated by step id, available for ${ref} substitution.
        let mut outputs: HashMap<String, String> = HashMap::new();
        // Per-step outputs in plan order (id-or-empty, output), for selection.
        let mut step_outputs: Vec<(String, String)> = Vec::with_capacity(steps.len());

        for (idx, step) in steps.iter().enumerate() {
            // Resolve the tool first so an unknown tool fails before any work.
            let Some(tool) = self.tools.get(&step.tool) else {
                return Ok(format!(
                    "[tool_pipeline] error at step {idx}: unknown tool {tool:?}. Available tools: {available}",
                    tool = step.tool,
                    available = available_tools(&self.tools)
                ));
            };

            // Substitute ${id} references using outputs collected so far.
            let resolved_args = substitute_refs(&step.args, &outputs);

            // Execute; on error, stop and report which step failed.
            let output = match tool.execute(resolved_args).await {
                Ok(o) => o,
                Err(e) => {
                    return Ok(format!(
                        "[tool_pipeline] error at step {idx} (tool {tool:?}): {e}",
                        tool = step.tool,
                    ));
                }
            };

            // Store output under the step id (if any) for later ${ref}s, and
            // always record it positionally for return selection.
            if let Some(id) = &step.id {
                outputs.insert(id.clone(), output.clone());
            }
            step_outputs.push((step.id.clone().unwrap_or_default(), output));
        }

        // Collect the selected (or last) outputs. INTERMEDIATE outputs that are
        // not selected never leave this function.
        let selected = select_returns(&steps, &step_outputs);
        let results: Vec<Value> = selected
            .into_iter()
            .map(|(id, output)| json!({ "id": id, "output": output }))
            .collect();

        let payload = json!({ "results": results });
        let rendered = serde_json::to_string(&payload)
            .unwrap_or_else(|_| "[tool_pipeline] error: failed to serialize results".to_string());

        // Bound the RETURNED output so the pipeline can't blow context either.
        Ok(cap_tool_result(&rendered, MAX_TOOL_RESULT_TOKENS))
    }
}

/// Comma-separated, sorted list of available tool names, for error messages.
fn available_tools(tools: &HashMap<String, Arc<dyn Tool>>) -> String {
    let mut names: Vec<&str> = tools.keys().map(|s| s.as_str()).collect();
    names.sort_unstable();
    names.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- substitute_refs ---------------------------------------------------

    fn outputs(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn substitute_refs_replaces_in_nested_strings() {
        let out = outputs(&[("a", "ALPHA")]);
        let args = json!({
            "outer": {
                "inner": "value is ${a}",
                "list": ["${a}!", "no ref"]
            }
        });
        let res = substitute_refs(&args, &out);
        assert_eq!(res["outer"]["inner"], "value is ALPHA");
        assert_eq!(res["outer"]["list"][0], "ALPHA!");
        assert_eq!(res["outer"]["list"][1], "no ref");
    }

    #[test]
    fn substitute_refs_multiple_refs_in_one_string() {
        let out = outputs(&[("a", "X"), ("b", "Y")]);
        let args = json!({ "k": "${a}-${b}-${a}" });
        let res = substitute_refs(&args, &out);
        assert_eq!(res["k"], "X-Y-X");
    }

    #[test]
    fn substitute_refs_unknown_ref_left_verbatim() {
        let out = outputs(&[("a", "X")]);
        let args = json!({ "k": "${a} and ${missing}" });
        let res = substitute_refs(&args, &out);
        assert_eq!(res["k"], "X and ${missing}");
    }

    #[test]
    fn substitute_refs_non_string_values_untouched() {
        let out = outputs(&[("a", "X")]);
        let args = json!({ "n": 42, "b": true, "nil": null, "arr": [1, 2] });
        let res = substitute_refs(&args, &out);
        assert_eq!(res["n"], 42);
        assert_eq!(res["b"], true);
        assert!(res["nil"].is_null());
        assert_eq!(res["arr"], json!([1, 2]));
    }

    #[test]
    fn substitute_refs_multibyte_safe() {
        let out = outputs(&[("a", "X")]);
        let args = json!({ "k": "héllo 🌍 ${a}" });
        let res = substitute_refs(&args, &out);
        assert_eq!(res["k"], "héllo 🌍 X");
    }

    // --- parse_steps -------------------------------------------------------

    #[test]
    fn parse_steps_valid_multi_step() {
        let args = json!({
            "steps": [
                { "id": "s1", "tool": "echo", "args": { "text": "hi" } },
                { "tool": "upper", "args": { "text": "${s1}" }, "return": true }
            ]
        });
        let steps = parse_steps(&args).unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].id.as_deref(), Some("s1"));
        assert_eq!(steps[0].tool, "echo");
        assert!(!steps[0].return_);
        assert_eq!(steps[1].id, None);
        assert_eq!(steps[1].tool, "upper");
        assert!(steps[1].return_);
    }

    #[test]
    fn parse_steps_args_defaults_to_empty_object() {
        let args = json!({ "steps": [ { "tool": "noargs" } ] });
        let steps = parse_steps(&args).unwrap();
        assert_eq!(steps[0].args, json!({}));
    }

    #[test]
    fn parse_steps_missing_tool_errors() {
        let args = json!({ "steps": [ { "id": "s1", "args": {} } ] });
        let err = parse_steps(&args).unwrap_err();
        assert!(err.contains("tool"), "unexpected error: {err}");
    }

    #[test]
    fn parse_steps_empty_errors() {
        assert!(parse_steps(&json!({ "steps": [] })).is_err());
    }

    #[test]
    fn parse_steps_missing_steps_errors() {
        assert!(parse_steps(&json!({})).is_err());
        assert!(parse_steps(&json!({ "steps": "nope" })).is_err());
    }

    #[test]
    fn parse_steps_non_object_args_errors() {
        let args = json!({ "steps": [ { "tool": "t", "args": "nope" } ] });
        assert!(parse_steps(&args).is_err());
    }

    // --- select_returns ----------------------------------------------------

    fn step(id: Option<&str>, return_: bool) -> PipelineStep {
        PipelineStep {
            id: id.map(|s| s.to_string()),
            tool: "t".to_string(),
            args: json!({}),
            return_,
        }
    }

    fn so(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn select_returns_explicit_marked() {
        let steps = vec![
            step(Some("a"), false),
            step(Some("b"), true),
            step(Some("c"), true),
        ];
        let out = so(&[("a", "AA"), ("b", "BB"), ("c", "CC")]);
        let selected = select_returns(&steps, &out);
        assert_eq!(
            selected,
            vec![
                ("b".to_string(), "BB".to_string()),
                ("c".to_string(), "CC".to_string())
            ]
        );
    }

    #[test]
    fn select_returns_last_step_fallback() {
        let steps = vec![step(Some("a"), false), step(Some("b"), false)];
        let out = so(&[("a", "AA"), ("b", "BB")]);
        let selected = select_returns(&steps, &out);
        assert_eq!(selected, vec![("b".to_string(), "BB".to_string())]);
    }

    #[test]
    fn select_returns_last_step_without_id_returns_its_output() {
        // Last step has no id but DID produce output -> fallback returns it.
        let steps = vec![step(Some("a"), false), step(None, false)];
        let out = so(&[("a", "AA"), ("", "LAST")]);
        let selected = select_returns(&steps, &out);
        assert_eq!(selected, vec![("".to_string(), "LAST".to_string())]);
    }

    // --- execute (with mock tools) ----------------------------------------

    struct EchoTool;
    #[async_trait]
    impl Tool for EchoTool {
        fn name(&self) -> &str {
            "echo"
        }
        fn description(&self) -> &str {
            "echo"
        }
        fn parameters(&self) -> Value {
            json!({})
        }
        async fn execute(&self, args: Value) -> Result<String> {
            Ok(args
                .get("text")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string())
        }
    }

    struct UpperTool;
    #[async_trait]
    impl Tool for UpperTool {
        fn name(&self) -> &str {
            "upper"
        }
        fn description(&self) -> &str {
            "upper"
        }
        fn parameters(&self) -> Value {
            json!({})
        }
        async fn execute(&self, args: Value) -> Result<String> {
            Ok(args
                .get("text")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_uppercase())
        }
    }

    struct FailTool;
    #[async_trait]
    impl Tool for FailTool {
        fn name(&self) -> &str {
            "fail"
        }
        fn description(&self) -> &str {
            "fail"
        }
        fn parameters(&self) -> Value {
            json!({})
        }
        async fn execute(&self, _args: Value) -> Result<String> {
            Err(anyhow::anyhow!("boom"))
        }
    }

    fn pipeline_with(tools: Vec<Arc<dyn Tool>>) -> PipelineTool {
        PipelineTool::from_tools(&tools)
    }

    #[tokio::test]
    async fn execute_chains_and_returns_only_last() {
        let tool = pipeline_with(vec![Arc::new(EchoTool), Arc::new(UpperTool)]);
        let out = tool
            .execute(json!({
                "steps": [
                    { "id": "step1", "tool": "echo", "args": { "text": "hello" } },
                    { "tool": "upper", "args": { "text": "${step1}" } }
                ]
            }))
            .await
            .unwrap();
        let parsed: Value = serde_json::from_str(&out).unwrap();
        let results = parsed["results"].as_array().unwrap();
        // Only the last step's output is returned (no step marked return).
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["output"], "HELLO");
        // Intermediate "hello" output is NOT present in the returned payload
        // other than as transformed into the final result.
        assert!(!out.contains("\"output\":\"hello\""));
    }

    #[tokio::test]
    async fn execute_returns_only_marked_steps() {
        let tool = pipeline_with(vec![Arc::new(EchoTool), Arc::new(UpperTool)]);
        let out = tool
            .execute(json!({
                "steps": [
                    { "id": "a", "tool": "echo", "args": { "text": "keep" }, "return": true },
                    { "id": "b", "tool": "echo", "args": { "text": "drop" } },
                    { "id": "c", "tool": "upper", "args": { "text": "${a}" }, "return": true }
                ]
            }))
            .await
            .unwrap();
        let parsed: Value = serde_json::from_str(&out).unwrap();
        let results = parsed["results"].as_array().unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0]["id"], "a");
        assert_eq!(results[0]["output"], "keep");
        assert_eq!(results[1]["id"], "c");
        assert_eq!(results[1]["output"], "KEEP");
        // The unmarked intermediate "drop" output never leaves execute().
        assert!(!out.contains("drop"));
    }

    #[tokio::test]
    async fn execute_unknown_tool_stops_with_error() {
        let tool = pipeline_with(vec![Arc::new(EchoTool)]);
        let out = tool
            .execute(json!({
                "steps": [
                    { "tool": "echo", "args": { "text": "ok" } },
                    { "tool": "nonexistent", "args": {} }
                ]
            }))
            .await
            .unwrap();
        assert!(out.contains("unknown tool"), "got: {out}");
        assert!(out.contains("step 1"), "got: {out}");
    }

    #[tokio::test]
    async fn execute_step_error_stops_and_reports() {
        let tool = pipeline_with(vec![Arc::new(EchoTool), Arc::new(FailTool)]);
        let out = tool
            .execute(json!({
                "steps": [
                    { "tool": "echo", "args": { "text": "ok" } },
                    { "tool": "fail", "args": {} }
                ]
            }))
            .await
            .unwrap();
        assert!(out.contains("error at step 1"), "got: {out}");
        assert!(out.contains("boom"), "got: {out}");
    }

    #[tokio::test]
    async fn execute_malformed_steps_is_graceful() {
        let tool = pipeline_with(vec![Arc::new(EchoTool)]);
        let out = tool.execute(json!({})).await.unwrap();
        assert!(out.contains("[tool_pipeline] error"), "got: {out}");
    }

    #[test]
    fn pipeline_excludes_itself_recursion_guard() {
        // A tool literally named "tool_pipeline" must be dropped from the map.
        struct FakePipeline;
        #[async_trait]
        impl Tool for FakePipeline {
            fn name(&self) -> &str {
                "tool_pipeline"
            }
            fn description(&self) -> &str {
                ""
            }
            fn parameters(&self) -> Value {
                json!({})
            }
            async fn execute(&self, _args: Value) -> Result<String> {
                Ok(String::new())
            }
        }
        let pipe = PipelineTool::from_tools(&[Arc::new(EchoTool), Arc::new(FakePipeline)]);
        assert!(pipe.tools.contains_key("echo"));
        assert!(!pipe.tools.contains_key("tool_pipeline"));
    }
}
