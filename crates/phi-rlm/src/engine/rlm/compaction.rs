//! In-loop context compaction (WS5): when the live history approaches the
//! root model's context window, fold it into a structured summary and keep
//! going — long runs must not die on provider overflow.
//!
//! Design (upstream rlm `_compact_history` + claude-code compact service +
//! VCC's view-over-lossless-log framing):
//! - Trigger: estimated context tokens ≥ 85% of the window minus reserved
//!   headroom for the summary call itself (upstream: 0.85 · window,
//!   `ref/rlm/rlm/core/rlm.py`; claude-code reserves summary + max-output).
//! - The compacted message span is never lost: it is archived verbatim into
//!   a REPL variable (`compacted_{exchange}_{n}`) before the in-band history
//!   is rebuilt, so the model can search or slice it programmatically —
//!   compaction is a cheaper *view* of the trajectory, not deletion.
//! - The rebuilt history keeps the system prompt, the initial user prompt,
//!   the most recent complete iteration verbatim, and every mid-run user
//!   interjection verbatim (claude-code: user messages survive compaction).
//! - Circuit breaker: after `MAX_COMPACTION_FAILURES` consecutive failed
//!   summary calls, compaction disarms for the rest of the run (claude-code
//!   added this after runaway retry loops burned API quota).

use std::sync::Arc;

use base64::Engine as _;
use phi_ai::Usage;
use phi_repl::PythonRepl;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::config::RlmEvent;
use crate::lm_core::{ChatMessage, LmHandler, Role};
use crate::prompts::{COMPACTION_SUMMARY_PROMPT, compaction_continue_message};

use super::Rlm;

/// Fraction of the context window at which compaction triggers.
pub(crate) const COMPACTION_THRESHOLD: f64 = 0.85;

/// Tokens reserved below the threshold for the summary completion itself
/// (its output plus the continue message). Claude-code reserves ~13K plus
/// the model's max-output; a flat constant keeps this independent of
/// per-model output caps.
pub(crate) const COMPACTION_HEADROOM_TOKENS: u64 = 20_000;

/// Consecutive summary-call failures after which compaction disarms.
pub(crate) const MAX_COMPACTION_FAILURES: usize = 3;

/// Prefix `RlmSession` puts on mid-run steered user messages; these survive
/// compaction verbatim.
const INTERJECTION_PREFIX: &str = "[user interjection]";

/// Per-run compaction state, owned by `run_loop`.
#[derive(Default)]
pub(crate) struct CompactionState {
    /// Successful compactions this run (names the archive variables).
    pub count: usize,
    /// Consecutive failures; at `MAX_COMPACTION_FAILURES` the breaker trips.
    pub consecutive_failures: usize,
}

/// Estimated size of the *next* prompt, from the usage of the completion
/// that just finished: everything that was sent (input + cache tiers) plus
/// what was generated (it becomes history). Zero when the provider reported
/// no usage (aborted stream) — treated as "unknown, don't compact".
fn context_tokens(usage: &Usage) -> u64 {
    usage.input + usage.cache_read + usage.cache_write + usage.output
}

/// Index of the last `Role::Assistant` message that still has messages after
/// it (its tool results), past the preserved head (`system` + initial user
/// prompt) — i.e. the start of the most recent complete iteration, which is
/// kept verbatim. Ordinary iterations carry no user messages anymore (WS9.2),
/// so the assistant tool-call message is the iteration boundary. Cutting at
/// it can never orphan a tool result from its assistant tool call (results
/// always directly follow their assistant message, so they land in the tail
/// together). `history.len()` when there is no safe boundary (keep no tail).
fn recent_tail_boundary(history: &[ChatMessage]) -> usize {
    history
        .iter()
        .enumerate()
        .skip(2)
        .rev()
        .skip(1) // the boundary must keep at least one message after it
        .find(|(_, m)| m.role == Role::Assistant)
        .map(|(i, _)| i)
        .unwrap_or(history.len())
}

/// Python snippet archiving the compacted span verbatim into the REPL as
/// `{var} = [{'role': ..., 'content': ...}, ...]`, base64-shielded like
/// `history_snippet` so arbitrary content can't break out of the literal.
fn archive_snippet(var: &str, span: &[ChatMessage]) -> String {
    let messages: Vec<Value> = span
        .iter()
        .map(|m| serde_json::json!({ "role": m.role, "content": m.content }))
        .collect();
    let encoded =
        base64::engine::general_purpose::STANDARD.encode(Value::Array(messages).to_string());
    format!(
        "import base64 as _b64, json as _json\n{var} = _json.loads(_b64.b64decode(\"{encoded}\").decode(\"utf-8\"))\ndel _b64, _json"
    )
}

impl Rlm {
    /// Compact `history` in place when it approaches the context window.
    /// Called at the end of every iteration; a no-op unless the threshold
    /// is crossed. Never fails the run: on any error the history is left
    /// untouched and the failure counter advances toward the breaker.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn maybe_compact(
        &self,
        env: &mut PythonRepl,
        handler: &Arc<LmHandler>,
        history: &mut Vec<ChatMessage>,
        last_usage: &Usage,
        state: &mut CompactionState,
        iteration: usize,
        exchange: usize,
        cancel: &CancellationToken,
    ) {
        if state.consecutive_failures >= MAX_COMPACTION_FAILURES {
            return;
        }
        let Some(window) = handler.root_context_window() else {
            return;
        };
        let used = context_tokens(last_usage);
        if used == 0 || history.len() < 4 {
            return;
        }
        let limit = ((window as f64 * COMPACTION_THRESHOLD) as u64)
            .saturating_sub(COMPACTION_HEADROOM_TOKENS);
        if used < limit {
            return;
        }

        // Nothing between the preserved head and the recent tail: rebuilding
        // would only grow the history. Bail before paying for a summary call.
        let boundary = recent_tail_boundary(history);
        if boundary <= 2 {
            return;
        }

        // ── Extension gate: SessionBeforeCompact ─────────────────────────
        // `Ok(None)` = an extension cancelled compaction; a non-empty return
        // customizes the summary instructions. Raced against cancellation so
        // a blocked handler can't strand the turn.
        let mut custom: Option<String> = None;
        let mut from_extension = false;
        if let Some(rt) = &self.runtime {
            let gate = tokio::select! {
                r = rt.before_compact(None) => r,
                () = cancel.cancelled() => return,
            };
            match gate {
                Ok(Some(instructions)) => {
                    if !instructions.is_empty() {
                        from_extension = true;
                        custom = Some(instructions);
                    }
                }
                Ok(None) => return,
                Err(_) => {}
            }
        }

        // ── Summary completion over the live history ─────────────────────
        let mut request = history.clone();
        let mut prompt = COMPACTION_SUMMARY_PROMPT.to_owned();
        if let Some(instructions) = custom {
            prompt.push_str("\n\nAdditional instructions for this summary:\n");
            prompt.push_str(&instructions);
        }
        request.push(ChatMessage {
            role: Role::User,
            content: prompt,
            ..Default::default()
        });
        let summary = match handler
            .complete_root(&request, None, None, None, cancel)
            .await
        {
            Ok(completion) if !completion.text.trim().is_empty() => completion.text,
            Ok(_) => {
                // Tool calls or empty text instead of a summary.
                state.consecutive_failures += 1;
                self.emit_compaction(iteration, used, None, Some("empty summary".to_owned()));
                return;
            }
            Err(e) => {
                if cancel.is_cancelled() {
                    return;
                }
                state.consecutive_failures += 1;
                self.emit_compaction(iteration, used, None, Some(e));
                return;
            }
        };

        // ── Rebuild history around the summary ───────────────────────────
        let span = &history[2..boundary];
        let interjections: Vec<String> = span
            .iter()
            .filter(|m| m.role == Role::User && m.content.starts_with(INTERJECTION_PREFIX))
            .map(|m| m.content.clone())
            .collect();

        // Archive the compacted span into the REPL before dropping it from
        // the in-band history (best-effort: a failed exec loses the archive
        // variable, not the run — the durable JSONL still has everything).
        state.count += 1;
        let archive_var = format!("compacted_{exchange}_{}", state.count - 1);
        let _ = env
            .exec(
                &archive_snippet(&archive_var, span),
                self.config.exec_timeout,
            )
            .await;

        let mut rebuilt = Vec::with_capacity(4 + history.len() - boundary);
        rebuilt.extend_from_slice(&history[..2]);
        rebuilt.push(ChatMessage {
            role: Role::Assistant,
            content: summary.clone(),
            ..Default::default()
        });
        rebuilt.push(ChatMessage {
            role: Role::User,
            content: compaction_continue_message(state.count, &archive_var, &interjections),
            ..Default::default()
        });
        rebuilt.extend_from_slice(&history[boundary..]);
        *history = rebuilt;

        state.consecutive_failures = 0;
        self.emit_compaction(iteration, used, Some(summary), None);

        // ── Extension notification: SessionCompact ───────────────────────
        // Fire-and-forget, detached so a slow observer can't delay the loop.
        if let Some(rt) = &self.runtime {
            let rt = Arc::clone(rt);
            tokio::spawn(async move {
                let _ = rt.on_session_compact(from_extension).await;
            });
        }
    }

    fn emit_compaction(
        &self,
        iteration: usize,
        tokens_before: u64,
        summary: Option<String>,
        error: Option<String>,
    ) {
        self.events.emit(RlmEvent::Compaction {
            depth: self.depth,
            iteration,
            tokens_before,
            summary,
            error,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: Role, content: &str) -> ChatMessage {
        ChatMessage {
            role,
            content: content.to_owned(),
            ..Default::default()
        }
    }

    #[test]
    fn tail_boundary_cuts_at_last_assistant_with_following_messages() {
        // Two iterations, no per-turn user messages (WS9.2 history shape):
        // the boundary is the last assistant tool-call message, keeping the
        // most recent iteration (a1 + t1) verbatim.
        let history = vec![
            msg(Role::System, "sys"),
            msg(Role::User, "initial"),
            msg(Role::Assistant, "a0"),
            msg(Role::Tool, "t0"),
            msg(Role::Assistant, "a1"),
            msg(Role::Tool, "t1"),
        ];
        assert_eq!(recent_tail_boundary(&history), 4);
    }

    #[test]
    fn tail_boundary_keeps_trailing_nudge_with_last_iteration() {
        // A trailing user message (e.g. a loop nudge) stays in the tail with
        // the iteration it follows.
        let history = vec![
            msg(Role::System, "sys"),
            msg(Role::User, "initial"),
            msg(Role::Assistant, "a0"),
            msg(Role::Tool, "t0"),
            msg(Role::Assistant, "a1"),
            msg(Role::Tool, "t1"),
            msg(Role::User, "nudge"),
        ];
        assert_eq!(recent_tail_boundary(&history), 4);
    }

    #[test]
    fn tail_boundary_bails_on_single_iteration() {
        // One iteration only: the boundary lands on the head edge (index 2),
        // which maybe_compact treats as "no compactable span" — rebuilding
        // would only grow the history.
        let history = vec![
            msg(Role::System, "sys"),
            msg(Role::User, "initial"),
            msg(Role::Assistant, "a0"),
            msg(Role::Tool, "t0"),
        ];
        assert_eq!(recent_tail_boundary(&history), 2);
    }

    #[test]
    fn tail_boundary_handles_no_safe_cut() {
        let history = vec![
            msg(Role::System, "sys"),
            msg(Role::User, "initial"),
            msg(Role::Assistant, "a0"),
        ];
        assert_eq!(recent_tail_boundary(&history), history.len());
    }

    #[test]
    fn archive_snippet_is_base64_shielded() {
        let span = vec![msg(Role::User, "line with \"quotes\" and \\ backslashes")];
        let snippet = archive_snippet("compacted_0_0", &span);
        assert!(snippet.contains("compacted_0_0 = _json.loads"));
        assert!(!snippet.contains("quotes"), "content must be encoded");
    }

    #[test]
    fn context_tokens_sums_all_tiers() {
        let usage = Usage {
            input: 100,
            output: 10,
            cache_read: 50,
            cache_write: 5,
            ..Default::default()
        };
        assert_eq!(context_tokens(&usage), 165);
    }
}
