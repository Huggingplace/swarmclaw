//! Sub-agent orchestration: decompose -> fan-out -> join.
//!
//! This module implements an opt-in delegate-task capability modeled on
//! Hermes Agent's `delegate_task`. A parent agent (with
//! `Agent::use_orchestrator == true`) can decompose a goal into independent
//! subtasks and run each in a fresh, ephemeral sub-agent. The sub-agents run
//! concurrently (bounded) and only their text summaries are joined back into
//! the parent's turn.
//!
//! SAFETY: this spawns REAL sub-agents (recursive LLM calls = cost), so it is
//! strictly opt-in. The wiring in `agent.rs` only appends [`DelegateTaskTool`]
//! when the parent has `use_orchestrator == true`. Every sub-agent is built
//! with `use_orchestrator = false` (the RECURSION GUARD), so sub-agents never
//! receive the delegate tool and therefore cannot delegate further.
//!
//! The [`SubAgentExecutor`] trait is the seam where execution backends are
//! swapped. Today only [`InProcessExecutor`] (in-process, same binary) exists;
//! a future Fleet-backed executor (K8s/Nomad/Docker, e.g. via the
//! `crate::fleet` provider) can implement the same trait and be dropped in at
//! the wiring sites without touching the tool or the turn loops.

use crate::config::AgentConfig;
use crate::core::agent::Agent;
use crate::core::state::{Message, Role};
use crate::llm::LLMProvider;
use crate::skills::Skill;
use crate::tools::Tool;
use anyhow::Result;
use async_trait::async_trait;
use futures::stream::{self, StreamExt};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

/// Maximum number of subtasks executed concurrently within a single
/// `delegate_task` call. Bounds the fan-out so a single delegation can't
/// stampede the provider with unbounded parallel LLM calls.
pub const MAX_DELEGATES: usize = 3;

/// Execution backend for a single delegated subtask.
///
/// This is the seam for swapping execution strategies. [`InProcessExecutor`]
/// runs the subtask in-process; a future Fleet-backed executor
/// (K8s/Nomad/Docker via `crate::fleet`) can implement this same trait and be
/// substituted at the wiring sites with no other changes.
#[async_trait]
pub trait SubAgentExecutor: Send + Sync {
    /// Run a single subtask to completion and return its text summary.
    async fn run_subtask(&self, goal: &str, context: &str) -> Result<String>;
}

/// In-process [`SubAgentExecutor`]: each subtask runs a fresh, ephemeral
/// [`Agent`] in the same process, reusing the parent's LLM provider, config,
/// and skills.
pub struct InProcessExecutor {
    llm: Arc<dyn LLMProvider>,
    config: AgentConfig,
    skills: Vec<Arc<dyn Skill>>,
}

impl InProcessExecutor {
    /// Build an executor from the parent's LLM provider, config, and skills.
    /// The skills are shared so sub-agents can use the same tools as the
    /// parent (file system, shell, web, etc.).
    pub fn new(
        llm: Arc<dyn LLMProvider>,
        config: AgentConfig,
        skills: Vec<Arc<dyn Skill>>,
    ) -> Self {
        Self {
            llm,
            config,
            skills,
        }
    }

    /// Construct (but do not run) the fresh, ephemeral child [`Agent`] that
    /// would execute `goal`/`context`.
    ///
    /// Extracted as a pure-ish helper so the RECURSION GUARD and ephemerality
    /// invariants are unit-testable without an LLM round-trip:
    ///
    /// * `use_orchestrator = false` — the child never receives the delegate
    ///   tool, so it cannot delegate further (recursion guard).
    /// * `use_multithread = false` — safe, deterministic sub-agent execution.
    /// * no `state_path`/`state_store_path` is ever set — the child is
    ///   EPHEMERAL and performs no disk writes (`Agent::new` leaves both
    ///   `None`, and we deliberately never call `with_state_path`).
    pub fn build_child_agent(&self, goal: &str, context: &str) -> Agent {
        let child_id = format!("subagent-{}", Uuid::new_v4());
        let mut child = Agent::new(child_id, self.config.clone(), self.llm.clone());
        child.skills = self.skills.clone();

        // RECURSION GUARD: sub-agents must never delegate further.
        child.use_orchestrator = false;
        // Safe, deterministic sub-agent execution.
        child.use_multithread = false;

        // Seed history with a single user message: the goal followed by a
        // clearly delimited Context section. `Agent::new` already seeded any
        // system instructions from the config.
        let seed = if context.trim().is_empty() {
            goal.to_string()
        } else {
            format!("{goal}\n\nContext:\n{context}")
        };
        child.state.history.push(Message {
            role: Role::User,
            content: seed,
            timestamp: now_secs(),
            tool_calls: None,
            tool_call_id: None,
        });

        child
    }
}

#[async_trait]
impl SubAgentExecutor for InProcessExecutor {
    async fn run_subtask(&self, goal: &str, context: &str) -> Result<String> {
        let mut child = self.build_child_agent(goal, context);

        // Run the child to completion. The shared loop_guard caps its own
        // reasoning loop, so this terminates.
        child.think().await?;

        // Extract the last assistant message as the summary.
        let summary = child
            .state
            .history
            .iter()
            .rev()
            .find(|m| m.role == Role::Assistant && !m.content.trim().is_empty())
            .map(|m| m.content.clone())
            .unwrap_or_else(|| "[no output]".to_string());

        Ok(summary)
    }
}

/// A single delegated subtask parsed from the tool arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DelegateTask {
    goal: String,
    context: String,
}

/// Parse the `tasks` array from the tool arguments into a list of subtasks.
///
/// Returns an empty `Vec` when `tasks` is missing, not an array, or empty —
/// the caller surfaces that as a graceful "no tasks" result rather than an
/// error. Each entry must have a non-empty string `goal`; `context` defaults
/// to an empty string when absent.
fn parse_tasks(args: &Value) -> Vec<DelegateTask> {
    let Some(arr) = args.get("tasks").and_then(|t| t.as_array()) else {
        return Vec::new();
    };

    arr.iter()
        .filter_map(|t| {
            let goal = t.get("goal").and_then(|g| g.as_str())?.trim().to_string();
            if goal.is_empty() {
                return None;
            }
            let context = t
                .get("context")
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_string();
            Some(DelegateTask { goal, context })
        })
        .collect()
}

/// Restore the original task order from `(index, summary)` pairs produced by
/// the unordered concurrent run. Mirrors `agent::order_results`.
fn order_results(mut completed: Vec<(usize, String)>) -> Vec<String> {
    completed.sort_by_key(|(idx, _)| *idx);
    completed.into_iter().map(|(_, result)| result).collect()
}

/// Tool that decomposes work into independent subtasks and runs them in
/// parallel ephemeral sub-agents, returning only their summaries.
///
/// Only added to an agent's tool set when `Agent::use_orchestrator == true`
/// (opt-in via `/orchestrator on`). See the module docs for the recursion
/// guard.
pub struct DelegateTaskTool {
    executor: Arc<dyn SubAgentExecutor>,
}

impl DelegateTaskTool {
    pub fn new(executor: Arc<dyn SubAgentExecutor>) -> Self {
        Self { executor }
    }
}

#[async_trait]
impl Tool for DelegateTaskTool {
    fn name(&self) -> &str {
        "delegate_task"
    }

    fn description(&self) -> &str {
        "Decompose a goal into independent subtasks and run each one in a \
         separate sub-agent IN PARALLEL, then return only their text \
         summaries. Use this for decomposable, independent work (e.g. \
         researching several topics at once, processing multiple files) where \
         the subtasks do not depend on each other's output. Provide a `tasks` \
         array; each task has a `goal` (what the sub-agent should accomplish) \
         and optional `context` (background it needs). Sub-agents cannot \
         delegate further, so make each goal self-contained."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "tasks": {
                    "type": "array",
                    "description": "Independent subtasks to run in parallel sub-agents.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "goal": {
                                "type": "string",
                                "description": "What the sub-agent should accomplish. Self-contained."
                            },
                            "context": {
                                "type": "string",
                                "description": "Optional background information the sub-agent needs."
                            }
                        },
                        "required": ["goal"]
                    }
                }
            },
            "required": ["tasks"]
        })
    }

    async fn execute(&self, args: Value) -> Result<String> {
        let tasks = parse_tasks(&args);
        if tasks.is_empty() {
            return Ok(
                "[delegate_task] No tasks provided. Supply a non-empty `tasks` array, \
                 each with a `goal`."
                    .to_string(),
            );
        }

        let executor = self.executor.clone();

        // Fan-out: run subtasks concurrently, bounded by MAX_DELEGATES.
        // Tag each with its original index so order can be restored after the
        // unordered join. A failing subtask yields an error entry rather than
        // failing the whole call.
        let completed: Vec<(usize, String)> = stream::iter(tasks.into_iter().enumerate())
            .map(|(idx, task)| {
                let executor = executor.clone();
                async move {
                    let summary = match executor.run_subtask(&task.goal, &task.context).await {
                        Ok(s) => s,
                        Err(e) => format!("[error] {e}"),
                    };
                    (idx, json!({ "goal": task.goal, "summary": summary }).to_string())
                }
            })
            .buffer_unordered(MAX_DELEGATES)
            .collect()
            .await;

        // Join: restore original order and emit a structured JSON array.
        let ordered = order_results(completed);
        let entries: Vec<Value> = ordered
            .iter()
            .filter_map(|s| serde_json::from_str::<Value>(s).ok())
            .collect();

        Ok(serde_json::to_string_pretty(&json!({ "results": entries }))
            .unwrap_or_else(|_| ordered.join("\n\n")))
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{
        ChatChunk, CompletionOptions, CompletionResponse, LLMProvider, ProviderCapabilities,
    };
    use std::pin::Pin;

    // Minimal LLM provider stub so we can construct an InProcessExecutor and
    // its child Agent without any network/LLM round-trip.
    struct StubLlm;

    #[async_trait]
    impl LLMProvider for StubLlm {
        fn provider_name(&self) -> &str {
            "stub"
        }
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities::openai_compatible()
        }
        async fn complete_with_tools(
            &self,
            _messages: &[Message],
            _options: &CompletionOptions,
            _tools: &[Arc<dyn Tool>],
        ) -> Result<CompletionResponse> {
            Ok(CompletionResponse {
                content: Some("stub".to_string()),
                tool_calls: None,
                finish_reason: Some("stop".to_string()),
            })
        }
        async fn stream(
            &self,
            _messages: &[Message],
            _options: &CompletionOptions,
            _tools: &[Arc<dyn Tool>],
        ) -> Result<Pin<Box<dyn futures::Stream<Item = Result<ChatChunk>> + Send>>> {
            Ok(Box::pin(stream::empty()))
        }
    }

    fn executor() -> InProcessExecutor {
        InProcessExecutor::new(Arc::new(StubLlm), AgentConfig::default(), Vec::new())
    }

    #[test]
    fn parse_tasks_valid_array() {
        let args = json!({
            "tasks": [
                { "goal": "research A", "context": "ctx A" },
                { "goal": "research B" }
            ]
        });
        let tasks = parse_tasks(&args);
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].goal, "research A");
        assert_eq!(tasks[0].context, "ctx A");
        assert_eq!(tasks[1].goal, "research B");
        assert_eq!(tasks[1].context, "");
    }

    #[test]
    fn parse_tasks_missing_or_empty() {
        assert!(parse_tasks(&json!({})).is_empty());
        assert!(parse_tasks(&json!({ "tasks": [] })).is_empty());
        assert!(parse_tasks(&json!({ "tasks": "nope" })).is_empty());
        // Entry with blank goal is dropped.
        assert!(parse_tasks(&json!({ "tasks": [{ "goal": "  " }] })).is_empty());
    }

    #[test]
    fn order_results_restores_original_order() {
        let completed = vec![
            (2, "c".to_string()),
            (0, "a".to_string()),
            (1, "b".to_string()),
        ];
        assert_eq!(order_results(completed), vec!["a", "b", "c"]);
    }

    #[test]
    fn max_delegates_is_positive() {
        assert!(MAX_DELEGATES > 0);
    }

    #[test]
    fn child_agent_has_recursion_guard_and_is_ephemeral() {
        let child = executor().build_child_agent("do the thing", "some context");

        // RECURSION GUARD: child can never receive the delegate tool.
        assert!(!child.use_orchestrator);
        assert!(!child.use_multithread);

        // EPHEMERAL: no state-persistence path is configured -> no disk writes.
        assert!(child.state_path_for_test().is_none());

        // The delegate tool is gated on use_orchestrator, so with it false the
        // child's assembled tools never contain "delegate_task". With no skills
        // the child has no tools at all, which trivially excludes it.
        let tool_names: Vec<String> = child
            .skills
            .iter()
            .flat_map(|s| s.tools())
            .map(|t| t.name().to_string())
            .collect();
        assert!(!tool_names.iter().any(|n| n == "delegate_task"));

        // History: seeded system instruction (if any) + our user message,
        // with the goal and a Context section.
        let last = child.state.history.last().unwrap();
        assert_eq!(last.role, Role::User);
        assert!(last.content.contains("do the thing"));
        assert!(last.content.contains("Context:"));
        assert!(last.content.contains("some context"));
    }

    #[tokio::test]
    async fn execute_with_empty_tasks_is_graceful() {
        let tool = DelegateTaskTool::new(Arc::new(executor()));
        let out = tool.execute(json!({})).await.unwrap();
        assert!(out.contains("No tasks"));
    }

    // A canned executor that echoes the goal, so we can test fan-out/join and
    // ordering without an LLM.
    struct EchoExecutor;
    #[async_trait]
    impl SubAgentExecutor for EchoExecutor {
        async fn run_subtask(&self, goal: &str, _context: &str) -> Result<String> {
            Ok(format!("done: {goal}"))
        }
    }

    #[tokio::test]
    async fn execute_fans_out_and_preserves_order() {
        let tool = DelegateTaskTool::new(Arc::new(EchoExecutor));
        let out = tool
            .execute(json!({
                "tasks": [
                    { "goal": "first" },
                    { "goal": "second" },
                    { "goal": "third" }
                ]
            }))
            .await
            .unwrap();
        let parsed: Value = serde_json::from_str(&out).unwrap();
        let results = parsed["results"].as_array().unwrap();
        assert_eq!(results.len(), 3);
        assert_eq!(results[0]["goal"], "first");
        assert_eq!(results[0]["summary"], "done: first");
        assert_eq!(results[1]["goal"], "second");
        assert_eq!(results[2]["goal"], "third");
    }
}
