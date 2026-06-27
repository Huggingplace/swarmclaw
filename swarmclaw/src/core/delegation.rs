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
//! swapped. [`InProcessExecutor`] (in-process, same binary) is the DEFAULT;
//! [`FleetExecutor`] is a Fleet-backed backend (distributed jobs via the
//! `crate::fleet` provider) that implements the same trait. The Fleet backend
//! is strictly OPT-IN: it is only selected when an `Agent` has a
//! `fleet_provider` configured AND `SWARMCLAW_DELEGATE_BACKEND == "fleet"`, so
//! the default path is unchanged.

use crate::config::AgentConfig;
use crate::core::agent::Agent;
use crate::core::state::{Message, Role};
use crate::fleet::{FleetJobRequest, FleetProvider};
use crate::llm::LLMProvider;
use crate::skills::Skill;
use crate::tools::Tool;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use futures::stream::{self, StreamExt};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
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

/// Maximum number of `get_job_status` polls a [`FleetExecutor`] performs
/// before giving up on a single subtask. Bounds the round-trip so a stuck or
/// never-terminating Fleet job can NEVER hang the delegating turn.
pub const FLEET_POLL_MAX_ATTEMPTS: usize = 60;

/// Default delay between `get_job_status` polls. Kept as a field on
/// [`FleetExecutor`] (defaulting to this) so tests can drive it to zero and
/// avoid real sleeps.
pub const FLEET_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Default container image used when none is configured. Purely a placeholder
/// for the seam; real deployments configure this explicitly.
pub const FLEET_DEFAULT_IMAGE: &str = "swarmclaw/subagent:latest";

/// Case-insensitive check for whether a Fleet job status is TERMINAL (the job
/// has stopped and will not change again). Terminal states are `"completed"`,
/// `"succeeded"` (success) and `"failed"` (failure). Everything else
/// (`"running"`, `"pending"`, `""`, ...) is non-terminal.
///
/// Pure helper, unit-tested independently of any provider.
pub fn is_terminal_status(s: &str) -> bool {
    let s = s.trim().to_ascii_lowercase();
    matches!(s.as_str(), "completed" | "succeeded" | "failed")
}

/// Whether a TERMINAL status represents a FAILURE (as opposed to success).
fn is_failed_status(s: &str) -> bool {
    s.trim().eq_ignore_ascii_case("failed")
}

/// Fleet-backed [`SubAgentExecutor`]: each subtask is dispatched as a
/// distributed Fleet job via an [`Arc<dyn FleetProvider>`] (the leapfrog from
/// in-process to distributed execution).
///
/// This is strictly OPT-IN and never the default. It is only constructed in
/// `agent.rs` when an `Agent` has a `fleet_provider` configured AND the
/// `SWARMCLAW_DELEGATE_BACKEND` env var selects `"fleet"`; otherwise delegation
/// continues to use [`InProcessExecutor`].
///
/// Round-trip (see [`FleetProvider`] for the contract): build a one-shot
/// [`FleetJobRequest`] from the goal/context, `spawn_agents`, then poll
/// `get_job_status` (bounded by [`FLEET_POLL_MAX_ATTEMPTS`]) until terminal,
/// and on success `get_job_result`. Because the reference Mothership provider
/// does not return results yet (inherits the `Ok(None)` default), a real
/// round-trip currently surfaces a clear "provider does not return results yet"
/// error — the seam is ready for future infra that does.
pub struct FleetExecutor {
    provider: Arc<dyn FleetProvider>,
    /// Container image for spawned sub-agent jobs.
    image: String,
    /// Optional command template. When `Some`, occurrences of `{goal}` and
    /// `{context}` are substituted; when `None`, a default command is used and
    /// the goal/context are passed via `env_vars`.
    command_template: Option<String>,
    /// Delay between status polls. Defaults to [`FLEET_POLL_INTERVAL`]; tests
    /// set it to `Duration::ZERO` to avoid real sleeps.
    poll_interval: Duration,
    /// Max number of status polls before timing out. Defaults to
    /// [`FLEET_POLL_MAX_ATTEMPTS`].
    max_attempts: usize,
}

impl FleetExecutor {
    /// Build a Fleet executor from a provider, using default image, command and
    /// poll bounds.
    pub fn new(provider: Arc<dyn FleetProvider>) -> Self {
        Self {
            provider,
            image: FLEET_DEFAULT_IMAGE.to_string(),
            command_template: None,
            poll_interval: FLEET_POLL_INTERVAL,
            max_attempts: FLEET_POLL_MAX_ATTEMPTS,
        }
    }

    /// Override the container image.
    pub fn with_image(mut self, image: impl Into<String>) -> Self {
        self.image = image.into();
        self
    }

    /// Override the command template (`{goal}` / `{context}` are substituted).
    pub fn with_command_template(mut self, template: impl Into<String>) -> Self {
        self.command_template = Some(template.into());
        self
    }

    /// Override the poll interval (used by tests to disable real sleeps).
    pub fn with_poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = interval;
        self
    }

    /// Override the max number of status polls.
    pub fn with_max_attempts(mut self, attempts: usize) -> Self {
        self.max_attempts = attempts;
        self
    }

    /// Build a one-shot [`FleetJobRequest`] for a subtask. Pure helper (no I/O)
    /// so request construction is unit-testable.
    ///
    /// The `job_id` is supplied by the caller (derived from a UUID at call
    /// time) so this stays deterministic given its inputs. The goal and context
    /// are always encoded into `env_vars` (`SWARMCLAW_SUBTASK_GOAL` /
    /// `SWARMCLAW_SUBTASK_CONTEXT`); when a `command_template` is set, `{goal}`
    /// and `{context}` are additionally substituted into the command.
    pub fn build_request(&self, job_id: &str, goal: &str, context: &str) -> FleetJobRequest {
        let command = match &self.command_template {
            Some(t) => t.replace("{goal}", goal).replace("{context}", context),
            None => "run-subtask".to_string(),
        };

        let mut env_vars: HashMap<String, String> = HashMap::new();
        env_vars.insert("SWARMCLAW_SUBTASK_GOAL".to_string(), goal.to_string());
        env_vars.insert(
            "SWARMCLAW_SUBTASK_CONTEXT".to_string(),
            context.to_string(),
        );

        FleetJobRequest {
            job_id: job_id.to_string(),
            image: self.image.clone(),
            command,
            env_vars,
            min_vcpu: 1.0,
            min_memory_gb: 1.0,
            count: 1,
        }
    }
}

#[async_trait]
impl SubAgentExecutor for FleetExecutor {
    async fn run_subtask(&self, goal: &str, context: &str) -> Result<String> {
        let job_id = format!("subtask-{}", Uuid::new_v4());
        let request = self.build_request(&job_id, goal, context);

        // Phase 1: spawn.
        self.provider
            .spawn_agents(request)
            .await
            .map_err(|e| anyhow!("fleet spawn failed for job {job_id}: {e}"))?;

        // Phase 2: poll status until terminal (bounded — never hang).
        let mut last_status = String::new();
        let mut terminal = false;
        for attempt in 0..self.max_attempts {
            let status = self
                .provider
                .get_job_status(&job_id)
                .await
                .map_err(|e| anyhow!("fleet status poll failed for job {job_id}: {e}"))?;
            last_status = status.status.clone();

            if is_terminal_status(&last_status) {
                terminal = true;
                break;
            }

            // Sleep between polls (zero in tests). Skip the final sleep since
            // we're about to exit the loop.
            if attempt + 1 < self.max_attempts && !self.poll_interval.is_zero() {
                tokio::time::sleep(self.poll_interval).await;
            }
        }

        if !terminal {
            return Err(anyhow!(
                "fleet job {job_id} did not reach a terminal status within \
                 {} polls (last status: {:?})",
                self.max_attempts,
                last_status
            ));
        }

        if is_failed_status(&last_status) {
            return Err(anyhow!(
                "fleet job {job_id} reported a failed status ({last_status})"
            ));
        }

        // Phase 3: fetch result on success.
        let result = self
            .provider
            .get_job_result(&job_id)
            .await
            .map_err(|e| anyhow!("fleet result fetch failed for job {job_id}: {e}"))?;

        match result {
            Some(summary) => Ok(summary),
            None => Err(anyhow!(
                "fleet job {job_id} completed but provider '{}' does not return \
                 results yet (get_job_result -> None); result-return is future \
                 infra work",
                self.provider.name()
            )),
        }
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

    // Regression: the delegate tool still works against an in-process-style
    // executor (here the canned EchoExecutor) — no behavior change from the
    // Fleet seam.
    #[tokio::test]
    async fn delegate_tool_regression_with_inprocess_style_executor() {
        let tool = DelegateTaskTool::new(Arc::new(EchoExecutor));
        let out = tool
            .execute(json!({ "tasks": [{ "goal": "only" }] }))
            .await
            .unwrap();
        let parsed: Value = serde_json::from_str(&out).unwrap();
        let results = parsed["results"].as_array().unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["summary"], "done: only");
    }

    // A mock executor that returns a per-goal summary, or an error for any goal
    // containing "fail" — used to drive DelegateTaskTool::execute() fan-out/join
    // and per-task error handling without any LLM round-trip.
    struct MockExecutor;
    #[async_trait]
    impl SubAgentExecutor for MockExecutor {
        async fn run_subtask(&self, goal: &str, _context: &str) -> Result<String> {
            if goal.contains("fail") {
                return Err(anyhow!("subtask blew up: {goal}"));
            }
            Ok(format!("summary: {goal}"))
        }
    }

    #[tokio::test]
    async fn execute_fanout_returns_summaries_in_original_order() {
        // Distinguishable goals; results must come back in submission order even
        // though they run concurrently (bounded by MAX_DELEGATES).
        let tool = DelegateTaskTool::new(Arc::new(MockExecutor));
        let out = tool
            .execute(json!({
                "tasks": [
                    { "goal": "alpha" },
                    { "goal": "bravo" },
                    { "goal": "charlie" },
                    { "goal": "delta" }
                ]
            }))
            .await
            .unwrap();
        let parsed: Value = serde_json::from_str(&out).unwrap();
        let results = parsed["results"].as_array().unwrap();
        assert_eq!(results.len(), 4);
        assert_eq!(results[0]["goal"], "alpha");
        assert_eq!(results[0]["summary"], "summary: alpha");
        assert_eq!(results[1]["summary"], "summary: bravo");
        assert_eq!(results[2]["summary"], "summary: charlie");
        assert_eq!(results[3]["summary"], "summary: delta");
    }

    #[tokio::test]
    async fn execute_failing_subtask_becomes_error_entry_not_whole_failure() {
        // The middle task fails; the call still succeeds and the failing task
        // surfaces as an [error] summary while the others return normally.
        let tool = DelegateTaskTool::new(Arc::new(MockExecutor));
        let out = tool
            .execute(json!({
                "tasks": [
                    { "goal": "ok one" },
                    { "goal": "please fail here" },
                    { "goal": "ok two" }
                ]
            }))
            .await
            .unwrap();
        let parsed: Value = serde_json::from_str(&out).unwrap();
        let results = parsed["results"].as_array().unwrap();
        assert_eq!(results.len(), 3, "all tasks produce an entry");
        assert_eq!(results[0]["summary"], "summary: ok one");
        let failed = results[1]["summary"].as_str().unwrap();
        assert!(failed.starts_with("[error]"), "got: {failed}");
        assert!(failed.contains("subtask blew up"), "got: {failed}");
        assert_eq!(results[2]["summary"], "summary: ok two");
    }

    #[tokio::test]
    async fn execute_missing_tasks_is_graceful_with_mock_executor() {
        let tool = DelegateTaskTool::new(Arc::new(MockExecutor));
        let out = tool.execute(json!({})).await.unwrap();
        assert!(out.contains("No tasks"), "got: {out}");
        // Empty array is equally graceful.
        let out = tool.execute(json!({ "tasks": [] })).await.unwrap();
        assert!(out.contains("No tasks"), "got: {out}");
    }

    #[tokio::test]
    async fn execute_more_tasks_than_max_delegates_still_completes_in_order() {
        // Submit more tasks than the concurrency bound (MAX_DELEGATES) to prove
        // the bounded buffer_unordered still drains every task and order is
        // restored. We assert values/order, not timing.
        let n = MAX_DELEGATES * 2 + 1;
        let tasks: Vec<Value> = (0..n)
            .map(|i| json!({ "goal": format!("task-{i}") }))
            .collect();
        let tool = DelegateTaskTool::new(Arc::new(MockExecutor));
        let out = tool.execute(json!({ "tasks": tasks })).await.unwrap();
        let parsed: Value = serde_json::from_str(&out).unwrap();
        let results = parsed["results"].as_array().unwrap();
        assert_eq!(results.len(), n);
        for (i, r) in results.iter().enumerate() {
            assert_eq!(r["goal"], format!("task-{i}"));
            assert_eq!(r["summary"], format!("summary: task-{i}"));
        }
    }

    // ---- Fleet executor tests (mock provider, no real infra, no real sleeps) ----

    use crate::fleet::{FleetError, FleetJobStatus};

    /// Configurable mock Fleet provider. `status` is returned by every
    /// `get_job_status` call (so it's terminal on the FIRST poll -> tests never
    /// sleep meaningfully), and `result` is returned by `get_job_result`.
    struct MockFleetProvider {
        status: String,
        result: Option<String>,
    }

    #[async_trait]
    impl FleetProvider for MockFleetProvider {
        fn name(&self) -> &str {
            "MockFleet"
        }
        async fn spawn_agents(&self, _request: FleetJobRequest) -> Result<(), FleetError> {
            Ok(())
        }
        async fn terminate_job(&self, _job_id: &str) -> Result<(), FleetError> {
            Ok(())
        }
        async fn get_job_status(&self, job_id: &str) -> Result<FleetJobStatus, FleetError> {
            Ok(FleetJobStatus {
                job_id: job_id.to_string(),
                status: self.status.clone(),
                active_nodes: 1,
            })
        }
        async fn get_job_result(&self, _job_id: &str) -> Result<Option<String>, FleetError> {
            Ok(self.result.clone())
        }
    }

    fn fleet_executor(provider: Arc<dyn FleetProvider>) -> FleetExecutor {
        // Zero poll interval so the bounded poll loop never actually sleeps.
        FleetExecutor::new(provider).with_poll_interval(Duration::ZERO)
    }

    #[test]
    fn is_terminal_status_classifies_states() {
        for ok in ["completed", "COMPLETED", "Completed", "succeeded", "FAILED", "failed"] {
            assert!(is_terminal_status(ok), "{ok} should be terminal");
        }
        for non in ["running", "PENDING", "pending", "", "   ", "queued"] {
            assert!(!is_terminal_status(non), "{non:?} should NOT be terminal");
        }
    }

    #[test]
    fn build_request_encodes_goal_and_context() {
        let exec = FleetExecutor::new(Arc::new(MockFleetProvider {
            status: "completed".into(),
            result: None,
        }))
        .with_command_template("do {goal} :: {context}");
        let req = exec.build_request("job-1", "the goal", "the ctx");
        assert_eq!(req.job_id, "job-1");
        assert_eq!(req.count, 1);
        assert_eq!(req.command, "do the goal :: the ctx");
        assert_eq!(
            req.env_vars.get("SWARMCLAW_SUBTASK_GOAL").map(|s| s.as_str()),
            Some("the goal")
        );
        assert_eq!(
            req.env_vars
                .get("SWARMCLAW_SUBTASK_CONTEXT")
                .map(|s| s.as_str()),
            Some("the ctx")
        );
    }

    #[tokio::test]
    async fn fleet_run_subtask_returns_summary_on_completed_with_result() {
        let exec = fleet_executor(Arc::new(MockFleetProvider {
            status: "completed".into(),
            result: Some("the summary".into()),
        }));
        let out = exec.run_subtask("goal", "ctx").await.unwrap();
        assert_eq!(out, "the summary");
    }

    #[tokio::test]
    async fn fleet_run_subtask_errors_when_provider_returns_no_result() {
        // Completed, but provider returns None (the Mothership default style).
        let exec = fleet_executor(Arc::new(MockFleetProvider {
            status: "completed".into(),
            result: None,
        }));
        let err = exec.run_subtask("goal", "ctx").await.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("does not return results yet"),
            "expected a clear no-result error, got: {msg}"
        );
    }

    #[tokio::test]
    async fn fleet_run_subtask_errors_on_failed_status() {
        let exec = fleet_executor(Arc::new(MockFleetProvider {
            status: "failed".into(),
            result: Some("ignored".into()),
        }));
        let err = exec.run_subtask("goal", "ctx").await.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("failed status"),
            "expected a descriptive failure error, got: {msg}"
        );
    }

    #[tokio::test]
    async fn fleet_run_subtask_times_out_when_never_terminal() {
        // Never-terminal status + tiny bound -> bounded timeout, never hangs.
        let exec = FleetExecutor::new(Arc::new(MockFleetProvider {
            status: "running".into(),
            result: None,
        }))
        .with_poll_interval(Duration::ZERO)
        .with_max_attempts(3);
        let err = exec.run_subtask("goal", "ctx").await.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("did not reach a terminal status"),
            "expected a timeout error, got: {msg}"
        );
    }

    #[tokio::test]
    async fn mothership_inherits_none_default_so_executor_reports_no_result() {
        // The reference Mothership provider returns status "running" forever and
        // inherits get_job_result -> None. With a tiny bound it times out (it
        // never completes), which still proves the default contract: no panic,
        // no empty success.
        use crate::fleet::MothershipFleetProvider;
        let exec = FleetExecutor::new(Arc::new(MothershipFleetProvider::new(
            "http://localhost".into(),
        )))
        .with_poll_interval(Duration::ZERO)
        .with_max_attempts(2);
        let err = exec.run_subtask("goal", "ctx").await.unwrap_err();
        assert!(err.to_string().contains("did not reach a terminal status"));
    }
}
