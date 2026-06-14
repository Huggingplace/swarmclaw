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
}
