//! Session log wiring (PLAN.md WS4.1): every run writes one JSONL session
//! under the phi data dir, keyed by cwd; `--continue` resumes the most
//! recent for cwd. The loop owns the transcript (`Context.messages`); the
//! store is an append-only sink for the *new* messages each run adds.
//!
//! The store is shared behind an `Arc<Mutex>` so the chat's spawned run
//! task can flush its delta off the UI loop.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result};
use phi_ai::{Context, Message};
use phi_session::{SessionStore, load_messages, most_recent_in};

/// A shared, lockable session store. `None` only when the data dir can't
/// be resolved (the bare core's login/list-models paths never get here);
/// in that degenerate case logging is skipped, not fatal.
pub type SharedStore = Arc<Mutex<SessionStore>>;

/// Open or create the session for this run.
///
/// - `continue_session`: resume the most recent session for `cwd`, seeding
///   `ctx.messages` from it and reusing its file (new messages append).
/// - otherwise: create a fresh session file.
///
/// The session's header model is `provider/id` of the active model.
pub fn open(
    root: &Path,
    cwd: &Path,
    model: &str,
    ctx: &mut Context,
    continue_session: bool,
) -> Result<SharedStore> {
    if continue_session {
        match most_recent_in(root, cwd) {
            Some(path) => {
                let (header, messages) =
                    load_messages(&path).context("loading session for --continue")?;
                if !messages.is_empty() {
                    ctx.messages = messages;
                }
                let store = SessionStore::open(&path)
                    .with_context(|| format!("reopening session {}", path.display()))?;
                // The resumed session keeps its original header model; a
                // mid-session /model switch updates the live model, not the
                // header (matching pi's session_info vs model_change split,
                // which we don't persist yet).
                let _ = header;
                return Ok(Arc::new(Mutex::new(store)));
            }
            None => {
                eprintln!(
                    "phi: no session to continue for {}; starting a new one",
                    cwd.display()
                );
            }
        }
    }
    let store = SessionStore::create_in(root, cwd, model).context("creating session log")?;
    Ok(Arc::new(Mutex::new(store)))
}

/// Flush the messages a run added to the transcript (everything from
/// `start_len` onward) to the store. Called after each run; failures are
/// reported on stderr, never fatal — a missed flush loses the session log
/// for that turn, not the live transcript.
pub fn flush(store: &SharedStore, messages: &[Message], start_len: usize) {
    if start_len >= messages.len() {
        return;
    }
    let mut guard = match store.lock() {
        Ok(g) => g,
        Err(e) => {
            eprintln!("phi: session log poisoned: {e}");
            return;
        }
    };
    for message in &messages[start_len..] {
        if let Err(e) = guard.append(message) {
            eprintln!("phi: session log write failed: {e:#}");
            return;
        }
    }
}

/// The sessions root for the app: the phi data dir's `sessions/`. Returns
/// a path (creating the root dir) or an error when no home dir can be
/// resolved.
pub fn root() -> Result<PathBuf> {
    let root = phi_session::sessions_root()?;
    std::fs::create_dir_all(&root)
        .with_context(|| format!("creating sessions root {}", root.display()))?;
    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use phi_ai::{AssistantContent, AssistantMessage, UserMessage};
    use tempfile::TempDir;

    fn assistant(text: &str) -> Message {
        let mut m = AssistantMessage::empty(&phi_ai::Model {
            id: "fake".into(),
            name: "Fake".into(),
            api: phi_ai::Api::AnthropicMessages,
            provider: "test".into(),
            base_url: String::new(),
            reasoning: false,
            input: Vec::new(),
            cost: Default::default(),
            context_window: 200_000,
            max_tokens: 8192,
            headers: Default::default(),
            thinking_levels: None,
            adaptive_thinking: false,
            reasoning_control: None,
        });
        m.content.push(AssistantContent::Text { text: text.into() });
        Message::Assistant(m)
    }

    /// WS4.1 acceptance (headless): a run writes a session, `--continue`
    /// reopens it with the transcript seeded, a second run's delta flushes
    /// into the same file, and a second `--continue` sees both turns.
    #[test]
    fn continue_seeds_and_appends_across_runs() {
        let root = TempDir::new().unwrap().path().to_owned();
        let cwd = Path::new("/tmp/project");

        // First run: prompt + assistant reply.
        let mut ctx = Context::default();
        let store = open(&root, cwd, "test/fake", &mut ctx, false).unwrap();
        let start = ctx.messages.len();
        ctx.messages.push(Message::User(UserMessage::new("first")));
        ctx.messages.push(assistant("first reply"));
        flush(&store, &ctx.messages, start);

        // Second run with --continue: transcript seeded with both prior
        // messages, new prompt + reply flush into the same file.
        let mut ctx2 = Context::default();
        let store2 = open(&root, cwd, "test/fake", &mut ctx2, true).unwrap();
        assert_eq!(
            ctx2.messages.len(),
            2,
            "--continue seeds the prior transcript"
        );
        assert!(matches!(&ctx2.messages[0], Message::User(u) if u.content == "first"));
        let start2 = ctx2.messages.len();
        ctx2.messages
            .push(Message::User(UserMessage::new("second")));
        ctx2.messages.push(assistant("second reply"));
        flush(&store2, &ctx2.messages, start2);

        // A fresh --continue sees all four messages across both turns.
        let mut ctx3 = Context::default();
        let _ = open(&root, cwd, "test/fake", &mut ctx3, true).unwrap();
        assert_eq!(ctx3.messages.len(), 4);
        let texts: Vec<String> = ctx3
            .messages
            .iter()
            .map(|m| match m {
                Message::User(u) => u.content.clone(),
                Message::Assistant(a) => a.text(),
                _ => String::new(),
            })
            .collect();
        assert_eq!(
            texts,
            vec!["first", "first reply", "second", "second reply"]
        );
    }

    /// `--continue` with no prior session falls back to a fresh session
    /// (and prints a notice, not an error).
    #[test]
    fn continue_with_no_prior_session_starts_fresh() {
        let root = TempDir::new().unwrap().path().to_owned();
        let cwd = Path::new("/tmp/empty");
        let mut ctx = Context::default();
        let store = open(&root, cwd, "test/fake", &mut ctx, true).unwrap();
        assert!(ctx.messages.is_empty());
        // The fresh store is writable.
        ctx.messages.push(Message::User(UserMessage::new("x")));
        flush(&store, &ctx.messages, 0);
        let recent = phi_session::most_recent_in(&root, cwd).unwrap();
        let (_, msgs) = phi_session::load_messages(&recent).unwrap();
        assert_eq!(msgs.len(), 1);
    }

    /// An empty prior session (header only) is skipped by most_recent, so
    /// --continue won't resume a transcript-less file.
    #[test]
    fn continue_skips_empty_prior_sessions() {
        let root = TempDir::new().unwrap().path().to_owned();
        let cwd = Path::new("/tmp/project");
        // Create an empty session (no messages).
        let _ = SessionStore::create_in(&root, cwd, "test/fake").unwrap();
        let mut ctx = Context::default();
        let store = open(&root, cwd, "test/fake", &mut ctx, true).unwrap();
        assert!(ctx.messages.is_empty(), "empty session not resumed");
        // The fallback created a second, writable session.
        ctx.messages.push(Message::User(UserMessage::new("hi")));
        flush(&store, &ctx.messages, 0);
    }
}
