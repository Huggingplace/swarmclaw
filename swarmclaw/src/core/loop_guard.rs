//! Loop guards for the agent reasoning turn loop.
//!
//! Both agent turn loops (`Agent::stream_think` and `Agent::run_fullscreen_turn`)
//! re-call the LLM after executing tool calls and only stop when the model
//! returns no tool calls. With no upper bound and no loop detection, a model
//! that keeps calling tools — or repeats the exact same call — runs forever and
//! burns unbounded cost.
//!
//! This module centralizes the *decision* logic so both loops behave
//! identically and the guard isn't duplicated or allowed to diverge. The
//! helpers here are pure and unit-tested; the loops keep only thin wiring.

/// Maximum number of LLM<->tools reasoning steps (outer-loop iterations) a
/// single turn may take before the loop stops gracefully. Each outer-loop
/// iteration is one provider request followed by optional tool execution.
pub const DEFAULT_MAX_REASONING_STEPS: usize = 25;

/// Number of times an identical tool-call signature may occur within a single
/// turn before the repeat detector trips. At/above this count we treat the
/// model as stuck repeating the same call and stop the loop.
///
/// This is a *total within the turn* threshold (not strictly consecutive):
/// every occurrence of a given signature in the turn is counted, which is the
/// simplest robust definition and catches A,B,A,B,A style oscillation as well
/// as A,A,A.
pub const REPEAT_THRESHOLD: usize = 3;

/// Build a stable string key for a tool call from its name and raw argument
/// string. Identical (name, args) pairs always produce the same signature;
/// differing names or args produce different signatures.
///
/// The arguments are used verbatim (the raw string the provider streamed); we
/// deliberately do not normalize/parse JSON here so the helper stays pure,
/// cheap, and dependency-free. Distinct serializations of equivalent JSON are
/// treated as distinct calls, which only makes the guard more conservative.
pub fn tool_call_signature(name: &str, args: &str) -> String {
    // A NUL separator can't appear in a tool name and is vanishingly unlikely
    // in argument JSON, so it gives an unambiguous join without escaping.
    format!("{name}\u{0}{args}")
}

/// Given the signatures of the tool calls already seen this turn (including the
/// just-computed `next` signature, or passed separately), report whether
/// `next` has now occurred `>= REPEAT_THRESHOLD` times within the turn.
///
/// `history` is the slice of all signatures recorded so far in the turn that do
/// NOT yet include `next`. Returns `true` when, counting `next`, the signature
/// reaches the threshold — i.e. the model is stuck repeating an identical call.
pub fn is_repeated_call(history: &[String], next: &str) -> bool {
    // +1 for the `next` occurrence itself.
    let count = 1 + history.iter().filter(|s| s.as_str() == next).count();
    count >= REPEAT_THRESHOLD
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_is_stable_for_same_inputs() {
        let a = tool_call_signature("search", "{\"q\":\"rust\"}");
        let b = tool_call_signature("search", "{\"q\":\"rust\"}");
        assert_eq!(a, b);
    }

    #[test]
    fn signature_differs_on_args() {
        let a = tool_call_signature("search", "{\"q\":\"rust\"}");
        let b = tool_call_signature("search", "{\"q\":\"go\"}");
        assert_ne!(a, b);
    }

    #[test]
    fn signature_differs_on_name() {
        let a = tool_call_signature("search", "{}");
        let b = tool_call_signature("fetch", "{}");
        assert_ne!(a, b);
    }

    #[test]
    fn signature_no_collision_across_name_arg_boundary() {
        // Without a separator, ("ab", "c") and ("a", "bc") could collide.
        let a = tool_call_signature("ab", "c");
        let b = tool_call_signature("a", "bc");
        assert_ne!(a, b);
    }

    #[test]
    fn repeat_detector_false_below_threshold() {
        let sig = tool_call_signature("t", "{}");
        // First occurrence: count == 1.
        assert!(!is_repeated_call(&[], &sig));
        // Second occurrence: count == 2, still below threshold of 3.
        assert!(!is_repeated_call(&[sig.clone()], &sig));
    }

    #[test]
    fn repeat_detector_true_at_threshold() {
        let sig = tool_call_signature("t", "{}");
        // Third occurrence reaches the threshold of 3.
        let history = vec![sig.clone(), sig.clone()];
        assert!(is_repeated_call(&history, &sig));
    }

    #[test]
    fn repeat_detector_true_after_threshold() {
        let sig = tool_call_signature("t", "{}");
        let history = vec![sig.clone(), sig.clone(), sig.clone()];
        assert!(is_repeated_call(&history, &sig));
    }

    #[test]
    fn repeat_detector_counts_total_not_consecutive() {
        // Oscillating A,B,A,B,A should trip on the 3rd A even though they are
        // not consecutive.
        let a = tool_call_signature("a", "{}");
        let b = tool_call_signature("b", "{}");
        let history = vec![a.clone(), b.clone(), a.clone(), b.clone()];
        assert!(is_repeated_call(&history, &a));
    }

    #[test]
    fn repeat_detector_distinct_calls_do_not_trip() {
        let a = tool_call_signature("a", "{}");
        let b = tool_call_signature("b", "{}");
        let c = tool_call_signature("c", "{}");
        let history = vec![a, b];
        assert!(!is_repeated_call(&history, &c));
    }

    #[test]
    fn max_step_constant_is_positive() {
        assert!(DEFAULT_MAX_REASONING_STEPS > 0);
        assert!(REPEAT_THRESHOLD > 0);
    }
}
