//! Context-pressure management.
//!
//! SwarmClaw agents (especially long-lived chat-gateway agents and cron
//! workers) accumulate conversation history indefinitely. Left unbounded, the
//! history eventually overflows the model's context window and every turn gets
//! slower and more expensive. This module trims the history we send to the
//! model down to an estimated token budget — without mutating the stored
//! history and without producing a transcript any provider adapter would
//! reject.

use crate::core::state::{Message, Role};

/// Rough chars-per-token ratio used for budget estimation when no tokenizer is
/// available. English prose is ~4 chars/token; we keep it simple and slightly
/// conservative.
const CHARS_PER_TOKEN: usize = 4;

/// Fixed per-message overhead (role tags, delimiters, formatting) in estimated
/// tokens. Keeps many tiny messages from being under-counted.
const PER_MESSAGE_OVERHEAD_TOKENS: usize = 4;

/// Default budget, in estimated tokens, for the history sent to the model.
/// Generous enough not to interfere with normal sessions, but bounds the
/// unbounded growth of very long-lived agents.
pub const DEFAULT_CONTEXT_TOKEN_BUDGET: usize = 120_000;

/// Number of most-recent messages always kept verbatim (never summarized) when
/// compacting history. The kept tail may grow beyond this to land on a `User`
/// boundary, but never shrinks below it.
pub const DEFAULT_PROTECT_LAST_N: usize = 6;

/// Maximum estimated token size of a single tool result that may be stored in
/// history. A single giant tool result (a huge file dump, a verbose command
/// output) can otherwise dominate or overflow the entire context window, so we
/// cap each one at ~30% of the default budget (mirroring OpenClaw) before
/// storing it. The rendered/displayed result is unaffected; this only bounds
/// what is persisted into conversation history.
pub const MAX_TOOL_RESULT_TOKENS: usize = DEFAULT_CONTEXT_TOKEN_BUDGET * 30 / 100;

/// Cap a single tool-result string to roughly `max_tokens` worth of content
/// before it is stored in history.
///
/// If the content already fits within `max_tokens` (per [`estimate_tokens`]'s
/// chars-per-token ratio) it is returned unchanged. Otherwise it is truncated
/// to approximately `max_tokens` worth of characters and a clear marker noting
/// how many characters were omitted is appended.
///
/// We keep the HEAD of the result (most tool output is most useful at the
/// start) plus a small TAIL (the end of command output / error footers are
/// often the actionable part), with the marker in between. All slicing is done
/// on UTF-8 char boundaries so multibyte content never causes a panic.
pub fn cap_tool_result(content: &str, max_tokens: usize) -> String {
    // Budget in characters, matching estimate_tokens' ratio. estimate_tokens
    // also adds PER_MESSAGE_OVERHEAD_TOKENS, so a content of exactly
    // max_tokens * CHARS_PER_TOKEN chars estimates slightly over budget; we
    // compare on the raw char-budget here, which is the quantity we truncate
    // to, keeping the helper self-consistent.
    let char_budget = max_tokens.saturating_mul(CHARS_PER_TOKEN);

    // Count chars (not bytes) so multibyte content is measured correctly.
    let total_chars = content.chars().count();
    if total_chars <= char_budget {
        return content.to_string();
    }

    // Reserve ~20% of the budget for a tail, the rest for the head.
    let tail_chars = char_budget / 5;
    let head_chars = char_budget.saturating_sub(tail_chars);
    let omitted = total_chars.saturating_sub(head_chars + tail_chars);

    // Collect char indices (byte offsets) so we slice on boundaries safely.
    let mut char_indices: Vec<usize> = content.char_indices().map(|(i, _)| i).collect();
    char_indices.push(content.len()); // sentinel = end byte offset

    let head_end = char_indices[head_chars];
    let tail_start = char_indices[total_chars - tail_chars];

    let head = &content[..head_end];
    let tail = &content[tail_start..];

    let marker = format!("\n[... tool result truncated: {omitted} characters omitted to fit context ...]\n");

    if tail_chars == 0 {
        format!("{head}{marker}")
    } else {
        format!("{head}{marker}{tail}")
    }
}

/// Maximum number of attempts (initial try + retries) for a single provider
/// call when recovering from a context-overflow error by recompacting harder.
pub const MAX_OVERFLOW_RETRIES: usize = 3;

/// Heuristically detect whether a provider error indicates the prompt exceeded
/// the model's context window (as opposed to auth, rate-limit, network, etc.).
///
/// Different providers phrase this differently, so we match a set of common
/// substrings case-insensitively against the error's string form. Used to
/// decide whether a failed provider call is worth retrying after shrinking the
/// effective context budget.
pub fn is_context_overflow_error(err: &anyhow::Error) -> bool {
    let s = err.to_string().to_lowercase();
    const SIGNALS: &[&str] = &[
        "context length",
        "maximum context",
        "context window",
        "context_length_exceeded",
        "too many tokens",
        "maximum_tokens",
        "max_tokens",
        "reduce the length",
        "prompt is too long",
        "prompt too long",
        "input is too long",
        "too large",
        "exceeds the maximum",
        "string too long",
    ];
    SIGNALS.iter().any(|sig| s.contains(sig))
}

/// Estimate the token cost of a single message (content + any tool-call JSON).
pub fn estimate_tokens(message: &Message) -> usize {
    let mut chars = message.content.len();
    if let Some(tool_calls) = &message.tool_calls {
        for tc in tool_calls {
            chars += tc.to_string().len();
        }
    }
    PER_MESSAGE_OVERHEAD_TOKENS + chars / CHARS_PER_TOKEN
}

/// Estimate the total token cost of a history slice.
pub fn estimate_history_tokens(history: &[Message]) -> usize {
    history.iter().map(estimate_tokens).sum()
}

/// Trim conversation history to fit an estimated token budget while keeping it
/// valid for every provider adapter.
///
/// Guarantees:
/// - All `System` messages are preserved (system prompt, capability notices).
/// - The kept tail of non-system messages always begins at a `User` message,
///   so we never send an orphan tool result or lead with an assistant turn
///   (which providers such as Anthropic reject).
/// - The most recent messages (including the current user turn) are always
///   kept, even if a single message alone exceeds the budget — validity is
///   prioritized over the strict budget.
///
/// Returns a possibly-shortened clone; the caller's stored history is untouched.
pub fn trim_to_budget(history: &[Message], max_tokens: usize) -> Vec<Message> {
    if estimate_history_tokens(history) <= max_tokens {
        return history.to_vec();
    }

    // System messages are usually small and always relevant; keep them all.
    let system: Vec<Message> = history
        .iter()
        .filter(|m| m.role == Role::System)
        .cloned()
        .collect();
    let system_tokens: usize = system.iter().map(estimate_tokens).sum();

    let others: Vec<&Message> = history
        .iter()
        .filter(|m| m.role != Role::System)
        .collect();

    let tail_budget = max_tokens.saturating_sub(system_tokens);

    // Walk newest -> oldest, accumulating until the budget is exceeded. `start`
    // is the index (into `others`) of the oldest message we keep. The first
    // iteration always keeps the newest message regardless of budget.
    let mut start = others.len();
    let mut used = 0usize;
    for idx in (0..others.len()).rev() {
        let cost = estimate_tokens(others[idx]);
        if start != others.len() && used + cost > tail_budget {
            break;
        }
        used += cost;
        start = idx;
    }

    // Advance so the tail begins on a User message, dropping any leading orphan
    // tool results or assistant turns left at the cut boundary.
    while start < others.len() && others[start].role != Role::User {
        start += 1;
    }

    // Safety net: if advancing consumed the whole window (no User message in
    // the kept range), fall back to the most recent User message onward so we
    // still send a valid, non-empty conversation.
    if start >= others.len() {
        if let Some(last_user) = others.iter().rposition(|m| m.role == Role::User) {
            start = last_user;
        }
    }

    let mut result = system;
    result.extend(others[start..].iter().map(|m| (*m).clone()));
    result
}

/// A plan for compacting an over-budget history by summarizing its middle.
///
/// The original history is conceptually split into three contiguous, ordered
/// segments:
/// - `head`: messages kept verbatim at the front. Always contains every
///   `System` message (system prompt, capability notices); these are never
///   summarized.
/// - `middle`: the contiguous slice to replace with a single summary message.
///   May be empty when no compaction is needed.
/// - `tail`: the most-recent messages kept verbatim. Always begins at a `User`
///   boundary so a tool-call assistant turn is never split from its `Tool`
///   result, and always includes the newest message.
///
/// `head ++ middle ++ tail` reproduces the original history exactly, so the plan
/// is fully reconstructable and unit-testable.
#[derive(Debug, Clone)]
pub struct CompactionPlan {
    pub head: Vec<Message>,
    pub middle: Vec<Message>,
    pub tail: Vec<Message>,
}

impl CompactionPlan {
    /// Whether this plan actually requires summarizing anything.
    pub fn needs_compaction(&self) -> bool {
        !self.middle.is_empty()
    }
}

/// Partition `history` into head / middle / tail for summarization-based
/// compaction (see [`CompactionPlan`]).
///
/// Rules:
/// - If the whole history already fits `max_tokens`, returns a no-compaction
///   plan (`head` = full history, empty `middle`/`tail`).
/// - All `System` messages stay in `head` and are never summarized.
/// - The last `protect_last_n` messages are always kept verbatim, the newest
///   message is always kept, and the kept tail is extended (toward older
///   messages) until it begins at a `User` boundary so tool-call/tool-result
///   pairs are never split across the middle/tail seam.
/// - The middle is the contiguous run of non-system messages between the
///   leading system messages and the kept tail. If that run is empty there is
///   nothing to summarize and the plan reports no compaction.
pub fn partition_for_compaction(
    history: &[Message],
    max_tokens: usize,
    protect_last_n: usize,
) -> CompactionPlan {
    // Fast path: already within budget -> nothing to compact.
    if estimate_history_tokens(history) <= max_tokens {
        return CompactionPlan {
            head: history.to_vec(),
            middle: Vec::new(),
            tail: Vec::new(),
        };
    }

    let n = history.len();

    // Leading System messages form the protected head. We only summarize the
    // contiguous body after them; any System message interleaved later still
    // lands in the middle and gets folded into the summary, but the canonical
    // system prompt(s) at the front are preserved verbatim.
    let mut head_end = 0usize;
    while head_end < n && history[head_end].role == Role::System {
        head_end += 1;
    }

    // Determine where the kept tail begins. Start by protecting the last
    // `protect_last_n` messages (and always at least the newest one), then walk
    // backward to the nearest `User` boundary so we never lead the tail with an
    // orphan Tool result or a bare Assistant turn.
    let protect = protect_last_n.max(1).min(n);
    let mut tail_start = n - protect;

    // Snap the tail start to a User boundary by extending toward older messages.
    while tail_start > head_end && history[tail_start].role != Role::User {
        tail_start -= 1;
    }

    // If we walked all the way back into the head without finding a User
    // boundary, fall back to the most recent User message in the body so the
    // tail is still valid and non-empty.
    if tail_start <= head_end || history[tail_start].role != Role::User {
        if let Some(last_user) = history[head_end..]
            .iter()
            .rposition(|m| m.role == Role::User)
        {
            tail_start = head_end + last_user;
        } else {
            // No User message anywhere in the body: there is no safe summarize
            // boundary, so keep the whole body as tail (no compaction).
            tail_start = head_end;
        }
    }

    let head: Vec<Message> = history[..head_end].to_vec();
    let middle: Vec<Message> = history[head_end..tail_start].to_vec();
    let tail: Vec<Message> = history[tail_start..].to_vec();

    CompactionPlan { head, middle, tail }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(role: Role, content: &str) -> Message {
        Message {
            role,
            content: content.to_string(),
            timestamp: 0,
            tool_calls: None,
            tool_call_id: None,
        }
    }

    #[test]
    fn under_budget_is_unchanged() {
        let history = vec![
            m(Role::System, "sys"),
            m(Role::User, "hi"),
            m(Role::Assistant, "hello"),
        ];
        let out = trim_to_budget(&history, 10_000);
        assert_eq!(out.len(), 3);
    }

    #[test]
    fn keeps_system_and_starts_tail_at_user() {
        // Each content is ~40 chars (~10 tokens + overhead). Budget forces a trim.
        let big = "x".repeat(40);
        let history = vec![
            m(Role::System, "system prompt"),
            m(Role::User, &big),      // oldest turn
            m(Role::Assistant, &big),
            m(Role::User, &big),      // newest turn
            m(Role::Assistant, &big),
        ];
        // Budget that fits roughly the last two messages plus system.
        let out = trim_to_budget(&history, 40);
        assert_eq!(out[0].role, Role::System);
        // First non-system message must be a User.
        assert_eq!(out[1].role, Role::User);
        // Newest message always retained.
        assert_eq!(out.last().unwrap().role, Role::Assistant);
    }

    #[test]
    fn drops_orphan_tool_result_at_cut_boundary() {
        let big = "y".repeat(80);
        let mut assistant = m(Role::Assistant, "");
        assistant.tool_calls = Some(vec![serde_json::json!({
            "id": "t1", "type": "function",
            "function": {"name": "f", "arguments": "{}"}
        })]);
        let mut tool_result = m(Role::Tool, &big);
        tool_result.tool_call_id = Some("t1".to_string());

        let history = vec![
            m(Role::System, "sys"),
            m(Role::User, &big),
            assistant,
            tool_result, // if the cut lands here, it would be an orphan
            m(Role::User, "current"),
        ];
        let out = trim_to_budget(&history, 20);
        // No kept non-system message may be an orphan Tool result at the front.
        let first_non_system = out.iter().find(|x| x.role != Role::System).unwrap();
        assert_eq!(first_non_system.role, Role::User);
        // Never drop the latest user turn.
        assert!(out.iter().any(|x| x.content == "current"));
    }

    #[test]
    fn always_keeps_latest_user_even_if_oversized() {
        let huge = "z".repeat(10_000);
        let history = vec![m(Role::System, "sys"), m(Role::User, &huge)];
        let out = trim_to_budget(&history, 5);
        assert_eq!(out.len(), 2);
        assert_eq!(out[1].role, Role::User);
    }

    #[test]
    fn partition_under_budget_needs_no_compaction() {
        let history = vec![
            m(Role::System, "sys"),
            m(Role::User, "hi"),
            m(Role::Assistant, "hello"),
        ];
        let plan = partition_for_compaction(&history, 10_000, DEFAULT_PROTECT_LAST_N);
        assert!(!plan.needs_compaction());
        assert!(plan.middle.is_empty());
        assert!(plan.tail.is_empty());
        // head holds the full history so head++middle++tail reconstructs it.
        assert_eq!(plan.head.len(), history.len());
    }

    #[test]
    fn partition_over_budget_identifies_middle_and_keeps_system_and_user_boundary() {
        let big = "x".repeat(400); // ~100 tokens each -> forces compaction
        let history = vec![
            m(Role::System, "system prompt"),
            m(Role::User, &big),       // oldest turn -> middle
            m(Role::Assistant, &big),  // middle
            m(Role::User, &big),       // middle
            m(Role::Assistant, &big),  // middle
            m(Role::User, "recent-1"),
            m(Role::Assistant, "recent-2"),
            m(Role::User, "recent-3"),
            m(Role::Assistant, "newest"),
        ];
        // Protect last 4 -> boundary snaps to a User message.
        let plan = partition_for_compaction(&history, 200, 4);
        assert!(plan.needs_compaction());

        // Head keeps all leading System messages.
        assert_eq!(plan.head.len(), 1);
        assert_eq!(plan.head[0].role, Role::System);
        // No System message ends up in the middle for this layout.
        assert!(plan.middle.iter().all(|x| x.role != Role::System));
        // Tail begins at a User boundary.
        assert_eq!(plan.tail.first().unwrap().role, Role::User);
        // Newest message always kept verbatim in the tail.
        assert_eq!(plan.tail.last().unwrap().content, "newest");

        // head ++ middle ++ tail reconstructs the original history.
        let mut recon = plan.head.clone();
        recon.extend(plan.middle.clone());
        recon.extend(plan.tail.clone());
        assert_eq!(recon.len(), history.len());
        assert_eq!(recon[0].content, history[0].content);
        assert_eq!(recon.last().unwrap().content, "newest");
    }

    #[test]
    fn partition_does_not_split_tool_call_pair_across_middle_tail_boundary() {
        let big = "w".repeat(400);
        let mut assistant = m(Role::Assistant, "");
        assistant.tool_calls = Some(vec![serde_json::json!({
            "id": "t1", "type": "function",
            "function": {"name": "f", "arguments": "{}"}
        })]);
        let mut tool_result = m(Role::Tool, "tool output");
        tool_result.tool_call_id = Some("t1".to_string());

        let history = vec![
            m(Role::System, "sys"),
            m(Role::User, &big),
            m(Role::Assistant, &big),
            m(Role::User, "kicks-off-tool"),
            assistant,            // tool_call
            tool_result,          // tool_result -- must stay with its assistant
            m(Role::Assistant, "final answer"),
        ];
        // protect_last_n=3 would land the boundary on the Tool result; the
        // snap-to-User logic must back it up to "kicks-off-tool".
        let plan = partition_for_compaction(&history, 50, 3);
        assert!(plan.needs_compaction());

        // Tail must start on a User message, not an orphan Tool/Assistant.
        assert_eq!(plan.tail.first().unwrap().role, Role::User);
        assert_eq!(plan.tail.first().unwrap().content, "kicks-off-tool");

        // The tool_call assistant and its Tool result are together in the tail,
        // never split across the seam (the Tool result is not in the middle).
        assert!(plan
            .middle
            .iter()
            .all(|x| x.role != Role::Tool && x.tool_calls.is_none()));
        assert!(plan.tail.iter().any(|x| x.role == Role::Tool));

        // Newest message kept.
        assert_eq!(plan.tail.last().unwrap().content, "final answer");
    }

    #[test]
    fn cap_tool_result_under_cap_is_unchanged() {
        let content = "small tool output";
        let out = cap_tool_result(content, 1000);
        assert_eq!(out, content);
    }

    #[test]
    fn cap_tool_result_over_cap_truncates_with_marker_and_bounds_length() {
        // 10_000 chars, cap at 10 tokens => ~40 char budget. Use 'Z' as the
        // content marker char since it never appears in the truncation notice,
        // so counting 'Z's measures exactly the kept original content.
        let content = "Z".repeat(10_000);
        let max_tokens = 10usize;
        let out = cap_tool_result(&content, max_tokens);

        assert_ne!(out, content);
        assert!(out.contains("tool result truncated"));
        assert!(out.contains("characters omitted"));

        // The kept content (head + tail, excluding the marker) is bounded by the
        // char budget. Estimate via the same ratio used internally.
        let char_budget = max_tokens * CHARS_PER_TOKEN;
        let kept = out.chars().filter(|c| *c == 'Z').count();
        assert!(
            kept <= char_budget,
            "kept {kept} content chars exceeds budget {char_budget}"
        );
        assert!(out.len() < content.len());
    }

    #[test]
    fn cap_tool_result_is_multibyte_safe() {
        // Each emoji/char is multibyte; truncation must never split a char.
        let content = "héllo🌍".repeat(2_000); // well over a tiny budget
        let out = cap_tool_result(&content, 5);
        // Must be valid UTF-8 (guaranteed by &str) and contain the marker.
        assert!(out.contains("tool result truncated"));
        // Round-trip through chars to confirm no broken boundaries / panics.
        assert!(out.chars().count() > 0);
    }

    #[test]
    fn is_context_overflow_error_positive_matches() {
        let cases = [
            "This model's maximum context length is 8192 tokens",
            "Error: context window exceeded",
            "Please reduce the length of the messages",
            "prompt is too long: 200000 tokens > 100000",
            "too many tokens in request",
            "context_length_exceeded",
            "input is too long for requested model",
        ];
        for c in cases {
            let err = anyhow::anyhow!("{c}");
            assert!(
                is_context_overflow_error(&err),
                "expected overflow match for: {c}"
            );
        }
    }

    #[test]
    fn is_context_overflow_error_negatives() {
        let cases = [
            "401 Unauthorized: invalid api_key",
            "429 rate limit exceeded",
            "connection reset by peer",
            "the model returned an empty response",
            "tool 'foo' not found",
        ];
        for c in cases {
            let err = anyhow::anyhow!("{c}");
            assert!(
                !is_context_overflow_error(&err),
                "did not expect overflow match for: {c}"
            );
        }
    }
}
