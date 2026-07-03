//! WS7.2: tool approval gate as an optional builtin extension.
//!
//! Core keeps pi's stance — no permission theater in the engine. This
//! extension is the reference consumer of the `ToolCall` interception event
//! plus the `UiHandle` ask primitive: enabled via config.lua
//! (`approvals = { enabled = true }`), it evaluates every intercepted tool
//! call against an ordered ruleset and blocks, allows, or asks the user.
//!
//! Ruleset order (opencode's last-match-wins semantics): builtin defaults
//! (ask on `repl`/`bash`/`write_file`/`edit_file`), then config.lua rules
//! (project layer trust-gated — [`phi_session::config::Config::load_approvals`]),
//! then rules persisted from "always allow" answers, then session-scoped
//! answers. A call no rule matches proceeds — the defaults cover the
//! surfaces that mutate the machine, and `repl` subsumes everything the
//! model does in REPL-only mode.
//!
//! Fail direction: asks fail **closed**. No interactive UI (one-shot mode),
//! a dismissed prompt, or an unanswered prompt all deny the call with an
//! explanatory tool error; the dispatch watchdog is raised per-registration
//! so the extension, not the fail-open dispatch timeout, decides.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Result, anyhow};
use phi_ext::{
    CommandInvocation, CommandOutput, CommandSpec, EventHandlerRegistration, EventKind,
    EventPayload, EventReturn, Extension, ExtensionHost, ExtensionManifest, UiAskOutcome, UiHandle,
    UiPrompt, UiPromptOption,
};
use phi_session::config::ApprovalsConfig;
use serde::{Deserialize, Serialize};

/// How long an `ask` waits for the user before failing closed.
const ASK_WAIT: Duration = Duration::from_secs(300);
/// Per-registration dispatch watchdog: comfortably above [`ASK_WAIT`] so the
/// extension decides the outcome. A timed-out interception handler
/// contributes no return and the call would proceed — fail open — which is
/// exactly what an approval gate must never do.
const HANDLER_TIMEOUT: Duration = Duration::from_secs(310);
/// Longest subject preview shown in the prompt body.
const SUBJECT_PREVIEW_CHARS: usize = 2000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Action {
    Allow,
    Deny,
    Ask,
}

impl Action {
    fn parse(s: &str) -> Result<Self> {
        match s {
            "allow" => Ok(Self::Allow),
            "deny" => Ok(Self::Deny),
            "ask" => Ok(Self::Ask),
            other => Err(anyhow!(
                "approvals rule action '{other}' is not one of allow/deny/ask"
            )),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Rule {
    tool: String,
    pattern: String,
    action: Action,
}

impl Rule {
    fn ask(tool: &str) -> Self {
        Self {
            tool: tool.to_owned(),
            pattern: "*".to_owned(),
            action: Action::Ask,
        }
    }

    fn matches(&self, tool: &str, subject: &str) -> bool {
        (self.tool == "*" || self.tool == tool) && wildcard_match(&self.pattern, subject)
    }
}

/// Builtin defaults: gate the surfaces that mutate the machine. `repl`
/// subsumes everything in REPL-only mode; the named helpers cover the
/// hybrid surface (`coding_tools_native`) where they arrive as their own
/// tool calls.
fn base_rules() -> Vec<Rule> {
    ["repl", "bash", "write_file", "edit_file"]
        .into_iter()
        .map(Rule::ask)
        .collect()
}

/// Simple wildcard match: `*` any run (including empty), `?` one char.
fn wildcard_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (None::<usize>, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            // Backtrack: let the last `*` swallow one more char.
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// What a rule's pattern is matched against, per tool: the thing the user
/// would recognize in a prompt. Falls back to the compact JSON args when the
/// canonical field is absent or not a string.
fn subject_for(name: &str, args: &serde_json::Value) -> String {
    let key = match name {
        "bash" => "cmd",
        "repl" => "code",
        "write_file" | "edit_file" => "path",
        _ => "",
    };
    if !key.is_empty()
        && let Some(s) = args.get(key).and_then(|v| v.as_str())
    {
        return s.to_owned();
    }
    serde_json::to_string(args).unwrap_or_default()
}

// ── Persistence ─────────────────────────────────────────────────────────────

/// "Always allow" answers, persisted per canonical project path
/// (`TrustStore`'s layout precedent): one JSON object at
/// `<data-dir>/phi/approvals.json`.
struct ApprovalStore {
    path: PathBuf,
    entries: BTreeMap<String, Vec<Rule>>,
}

impl ApprovalStore {
    fn default_path() -> PathBuf {
        directories::ProjectDirs::from("", "", "phi")
            .map(|dirs| dirs.data_dir().join("approvals.json"))
            .unwrap_or_else(|| PathBuf::from(".phi-approvals.json"))
    }

    /// Missing or unreadable file loads as an empty store, never an error.
    fn load(path: PathBuf) -> Self {
        let entries = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        Self { path, entries }
    }

    fn key(cwd: &Path) -> String {
        cwd.canonicalize()
            .unwrap_or_else(|_| cwd.to_path_buf())
            .display()
            .to_string()
    }

    fn rules_for(&self, cwd: &Path) -> Vec<Rule> {
        self.entries
            .get(&Self::key(cwd))
            .cloned()
            .unwrap_or_default()
    }

    fn append(&mut self, cwd: &Path, rule: Rule) -> Result<()> {
        self.entries.entry(Self::key(cwd)).or_default().push(rule);
        self.save()
    }

    fn clear(&mut self, cwd: &Path) -> Result<()> {
        self.entries.remove(&Self::key(cwd));
        self.save()
    }

    fn save(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&self.path, serde_json::to_string_pretty(&self.entries)?)?;
        Ok(())
    }
}

// ── Gate state ──────────────────────────────────────────────────────────────

struct GateState {
    cwd: PathBuf,
    base: Vec<Rule>,
    config: Vec<Rule>,
    /// Persisted "always allow" answers for this project.
    persisted: Vec<Rule>,
    /// Answers scoped to this session only (currently unused by the prompt
    /// options but part of the evaluation order for `/approvals` clarity).
    session: Vec<Rule>,
    store: ApprovalStore,
}

impl GateState {
    /// Evaluation order; later entries win.
    fn rules(&self) -> impl Iterator<Item = (&'static str, &Rule)> {
        (self.base.iter().map(|r| ("default", r)))
            .chain(self.config.iter().map(|r| ("config", r)))
            .chain(self.persisted.iter().map(|r| ("saved", r)))
            .chain(self.session.iter().map(|r| ("session", r)))
    }

    /// Last matching rule wins; unmatched calls proceed.
    fn evaluate(&self, tool: &str, subject: &str) -> Action {
        self.rules()
            .filter(|(_, r)| r.matches(tool, subject))
            .last()
            .map(|(_, r)| r.action)
            .unwrap_or(Action::Allow)
    }

    fn always_allow(&mut self, tool: &str) -> Result<()> {
        let rule = Rule {
            tool: tool.to_owned(),
            pattern: "*".to_owned(),
            action: Action::Allow,
        };
        self.persisted.push(rule.clone());
        self.store.append(&self.cwd, rule)
    }

    fn reset(&mut self) -> Result<()> {
        self.persisted.clear();
        self.session.clear();
        self.store.clear(&self.cwd)
    }
}

// ── Extension ───────────────────────────────────────────────────────────────

pub struct ApprovalsExtension {
    cwd: PathBuf,
    config: ApprovalsConfig,
    /// Test override for the persisted-allowlist file.
    store_path: Option<PathBuf>,
}

impl ApprovalsExtension {
    pub fn new(cwd: &Path, config: ApprovalsConfig) -> Self {
        Self {
            cwd: cwd.to_owned(),
            config,
            store_path: None,
        }
    }

    pub fn with_store_path(mut self, path: PathBuf) -> Self {
        self.store_path = Some(path);
        self
    }
}

fn config_rules(config: &ApprovalsConfig) -> Result<Vec<Rule>> {
    config
        .rules
        .clone()
        .unwrap_or_default()
        .into_iter()
        .map(|r| {
            Ok(Rule {
                tool: r.tool.unwrap_or_else(|| "*".to_owned()),
                pattern: r.pattern.unwrap_or_else(|| "*".to_owned()),
                action: Action::parse(r.action.as_deref().unwrap_or("ask"))?,
            })
        })
        .collect()
}

fn block(reason: impl Into<String>) -> Result<Option<EventReturn>> {
    Ok(Some(EventReturn::ToolBlock {
        reason: reason.into(),
    }))
}

fn subject_preview(subject: &str) -> String {
    if subject.chars().count() <= SUBJECT_PREVIEW_CHARS {
        return subject.to_owned();
    }
    let cut: String = subject.chars().take(SUBJECT_PREVIEW_CHARS).collect();
    format!("{cut}\n… (truncated)")
}

/// The interception handler: evaluate, then allow / deny / ask.
fn gate(
    state: &Arc<Mutex<GateState>>,
    ui: &UiHandle,
    name: &str,
    args: &serde_json::Value,
) -> Result<Option<EventReturn>> {
    let subject = subject_for(name, args);
    // Evaluate without holding the lock across the (long) ask below.
    let action = state.lock().unwrap().evaluate(name, &subject);
    match action {
        Action::Allow => Ok(None),
        Action::Deny => block(format!(
            "'{name}' is denied by an approvals rule. Do not retry the same call; \
             adjust your approach or ask the user."
        )),
        Action::Ask => {
            if !ui.has_ui() {
                return block(format!(
                    "'{name}' requires user approval but no interactive UI is attached \
                     — denied. Do not retry; the user must run this interactively or \
                     add an allow rule to `approvals` in config.lua."
                ));
            }
            let prompt = UiPrompt {
                title: format!("Allow {name}?"),
                body: subject_preview(&subject),
                options: vec![
                    UiPromptOption::new("Allow once"),
                    UiPromptOption::new(format!("Always allow {name}"))
                        .with_detail("persisted for this project"),
                    UiPromptOption::new("Deny"),
                ],
            };
            match ui.ask(prompt, ASK_WAIT) {
                UiAskOutcome::Answer(0) => Ok(None),
                UiAskOutcome::Answer(1) => {
                    // A failed save still leaves the rule in memory, so the
                    // answer holds for this session; persistence degrades
                    // silently rather than denying an explicitly allowed call.
                    let _ = state.lock().unwrap().always_allow(name);
                    Ok(None)
                }
                UiAskOutcome::Answer(_) | UiAskOutcome::Dismissed => block(format!(
                    "The user denied this '{name}' call. Do not retry the same call; \
                     ask the user how to proceed."
                )),
                UiAskOutcome::TimedOut => block(format!(
                    "Approval prompt for '{name}' timed out with no answer — denied."
                )),
                UiAskOutcome::NoUi => block(format!(
                    "'{name}' requires user approval but the interactive UI went away \
                     — denied."
                )),
            }
        }
    }
}

impl Extension for ApprovalsExtension {
    fn manifest(&self) -> ExtensionManifest {
        ExtensionManifest {
            id: "phi:approvals".to_owned(),
            name: "Phi Approvals".to_owned(),
            version: "0.1.0".to_owned(),
            description: "Gate tool calls behind user confirmation with a persistable \
                          allowlist (WS7.2)"
                .to_owned(),
        }
    }

    fn register(&self, host: &mut dyn ExtensionHost) -> Result<()> {
        let store = ApprovalStore::load(
            self.store_path
                .clone()
                .unwrap_or_else(ApprovalStore::default_path),
        );
        let persisted = store.rules_for(&self.cwd);
        let state = Arc::new(Mutex::new(GateState {
            cwd: self.cwd.clone(),
            base: base_rules(),
            config: config_rules(&self.config)?,
            persisted,
            session: Vec::new(),
            store,
        }));
        let ui = host.ui_handle();

        let gate_state = Arc::clone(&state);
        host.subscribe(EventHandlerRegistration {
            event: EventKind::ToolCall,
            label: "phi:approvals".to_owned(),
            handler: Arc::new(move |event| {
                let EventPayload::ToolCall { name, args } = &event.payload else {
                    return Ok(None);
                };
                gate(&gate_state, &ui, name, args)
            }),
            timeout: Some(HANDLER_TIMEOUT),
        })?;

        let cmd_state = Arc::clone(&state);
        host.register_command(CommandSpec {
            name: "approvals".to_owned(),
            description: "list tool approval rules; `reset` clears saved answers".to_owned(),
            args_hint: "[reset]".to_owned(),
            handler: Arc::new(move |inv: CommandInvocation| {
                let mut st = cmd_state.lock().unwrap();
                match inv.raw_args.trim() {
                    "" => {
                        let mut lines = vec![
                            "approval rules (last match wins; unmatched calls proceed):".to_owned(),
                        ];
                        for (provenance, rule) in st.rules() {
                            lines.push(format!(
                                "  [{provenance}] {} {} {}",
                                match rule.action {
                                    Action::Allow => "allow",
                                    Action::Deny => "deny",
                                    Action::Ask => "ask",
                                },
                                rule.tool,
                                rule.pattern,
                            ));
                        }
                        Ok(CommandOutput::message(lines.join("\n")))
                    }
                    "reset" => {
                        st.reset()?;
                        Ok(CommandOutput::message(
                            "cleared saved approval answers for this project",
                        ))
                    }
                    other => Err(anyhow!(
                        "unknown argument '{other}' (usage: /approvals [reset])"
                    )),
                }
            }),
            arg_completions: Some(Arc::new(|| vec!["reset".to_owned()])),
            context: phi_ext::CommandContext::Session,
            aliases: Vec::new(),
        })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phi_ext::{LifecycleEvent, RuntimeBuilder};

    #[test]
    fn wildcard_matching() {
        assert!(wildcard_match("*", ""));
        assert!(wildcard_match("*", "anything at all"));
        assert!(wildcard_match("git *", "git status"));
        assert!(!wildcard_match("git *", "cargo build"));
        assert!(wildcard_match("*.rs", "src/main.rs"));
        assert!(!wildcard_match("*.rs", "src/main.py"));
        assert!(wildcard_match("a?c", "abc"));
        assert!(!wildcard_match("a?c", "ac"));
        assert!(wildcard_match(
            "*cargo*test*",
            "RUST_LOG=1 cargo nextest run"
        ));
    }

    #[test]
    fn subjects_pick_the_recognizable_field() {
        assert_eq!(
            subject_for("bash", &serde_json::json!({"cmd": "ls -la"})),
            "ls -la"
        );
        assert_eq!(
            subject_for("repl", &serde_json::json!({"code": "print(1)"})),
            "print(1)"
        );
        assert_eq!(
            subject_for(
                "edit_file",
                &serde_json::json!({"path": "a.rs", "old": "x"})
            ),
            "a.rs"
        );
        // Absent canonical field → compact JSON args.
        assert_eq!(
            subject_for("edit_file", &serde_json::json!({"patch": "p"})),
            r#"{"patch":"p"}"#
        );
    }

    fn state_with(config: Vec<Rule>, dir: &Path) -> GateState {
        GateState {
            cwd: dir.to_owned(),
            base: base_rules(),
            config,
            persisted: Vec::new(),
            session: Vec::new(),
            store: ApprovalStore::load(dir.join("approvals.json")),
        }
    }

    #[test]
    fn last_match_wins_and_unmatched_proceeds() {
        let dir = tempfile::tempdir().unwrap();
        let mut st = state_with(
            vec![
                Rule {
                    tool: "bash".to_owned(),
                    pattern: "git *".to_owned(),
                    action: Action::Allow,
                },
                Rule {
                    tool: "bash".to_owned(),
                    pattern: "git push*".to_owned(),
                    action: Action::Deny,
                },
            ],
            dir.path(),
        );
        // Base default: ask.
        assert_eq!(st.evaluate("bash", "rm -rf /"), Action::Ask);
        // Config allow overrides the base ask.
        assert_eq!(st.evaluate("bash", "git status"), Action::Allow);
        // Later, more specific deny overrides the allow.
        assert_eq!(st.evaluate("bash", "git push origin main"), Action::Deny);
        // Unmatched tool proceeds.
        assert_eq!(st.evaluate("read_file", "src/main.rs"), Action::Allow);
        // A persisted always-allow lands after config and wins.
        st.always_allow("bash").unwrap();
        assert_eq!(st.evaluate("bash", "git push origin main"), Action::Allow);
    }

    #[test]
    fn store_round_trips_per_project() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("approvals.json");
        let cwd = dir.path().join("proj");
        std::fs::create_dir_all(&cwd).unwrap();

        let mut store = ApprovalStore::load(path.clone());
        store
            .append(
                &cwd,
                Rule {
                    tool: "bash".to_owned(),
                    pattern: "*".to_owned(),
                    action: Action::Allow,
                },
            )
            .unwrap();

        let reloaded = ApprovalStore::load(path.clone());
        assert_eq!(reloaded.rules_for(&cwd).len(), 1);
        assert!(reloaded.rules_for(&dir.path().join("other")).is_empty());

        let mut reloaded = reloaded;
        reloaded.clear(&cwd).unwrap();
        assert!(ApprovalStore::load(path).rules_for(&cwd).is_empty());
    }

    #[test]
    fn invalid_config_action_fails_registration() {
        let config = ApprovalsConfig {
            enabled: Some(true),
            rules: Some(vec![phi_session::config::ApprovalRuleConfig {
                tool: Some("bash".to_owned()),
                pattern: None,
                action: Some("alow".to_owned()),
            }]),
        };
        let dir = tempfile::tempdir().unwrap();
        let result = RuntimeBuilder::new()
            .with_cwd(dir.path())
            .register_extension(
                ApprovalsExtension::new(dir.path(), config)
                    .with_store_path(dir.path().join("approvals.json")),
            )
            .build();
        assert!(result.is_err(), "typo'd action must not silently gate");
    }

    fn tool_call(name: &str, args: serde_json::Value) -> LifecycleEvent {
        LifecycleEvent {
            kind: EventKind::ToolCall,
            payload: EventPayload::ToolCall {
                name: name.to_owned(),
                args,
            },
        }
    }

    fn runtime_with(config: ApprovalsConfig, dir: &Path) -> phi_ext::AppRuntime {
        RuntimeBuilder::new()
            .with_cwd(dir)
            .register_extension(
                ApprovalsExtension::new(dir, config).with_store_path(dir.join("approvals.json")),
            )
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn ask_without_ui_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let rt = runtime_with(ApprovalsConfig::default(), dir.path());
        let returns = rt
            .dispatch(tool_call("bash", serde_json::json!({"cmd": "rm -rf /"})))
            .await
            .unwrap();
        assert!(
            matches!(&returns[..], [EventReturn::ToolBlock { reason }]
                if reason.contains("no interactive UI")),
            "expected fail-closed block, got {returns:?}"
        );
        // Reads are not gated by default.
        let returns = rt
            .dispatch(tool_call("read_file", serde_json::json!({"path": "x"})))
            .await
            .unwrap();
        assert!(returns.is_empty());
    }

    #[tokio::test]
    async fn config_deny_blocks_without_prompting() {
        let dir = tempfile::tempdir().unwrap();
        let config = ApprovalsConfig {
            enabled: Some(true),
            rules: Some(vec![phi_session::config::ApprovalRuleConfig {
                tool: Some("bash".to_owned()),
                pattern: Some("git push*".to_owned()),
                action: Some("deny".to_owned()),
            }]),
        };
        let rt = runtime_with(config, dir.path());
        let returns = rt
            .dispatch(tool_call("bash", serde_json::json!({"cmd": "git push"})))
            .await
            .unwrap();
        assert!(matches!(&returns[..], [EventReturn::ToolBlock { reason }]
                if reason.contains("denied by an approvals rule")));
    }

    #[tokio::test]
    async fn prompt_flow_allow_once_and_always() {
        let dir = tempfile::tempdir().unwrap();
        let rt = runtime_with(ApprovalsConfig::default(), dir.path());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        rt.connect_ui(tx);

        // Round 1: user picks "Allow once" (index 0).
        let host = tokio::spawn(async move {
            let req = rx.recv().await.expect("prompt 1");
            assert_eq!(req.prompt.title, "Allow bash?");
            assert_eq!(req.prompt.body, "cargo test");
            assert_eq!(req.prompt.options.len(), 3);
            req.reply.send(Some(0)).unwrap();

            // Round 2: user picks "Always allow bash" (index 1).
            let req = rx.recv().await.expect("prompt 2");
            req.reply.send(Some(1)).unwrap();

            // Round 4 below dismisses (reply None).
            let req = rx.recv().await.expect("prompt 4");
            req.reply.send(None).unwrap();
            rx
        });

        let call = || tool_call("bash", serde_json::json!({"cmd": "cargo test"}));
        // 1: allowed once — prompts.
        assert!(rt.dispatch(call()).await.unwrap().is_empty());
        // 2: "always allow" — prompts, persists.
        assert!(rt.dispatch(call()).await.unwrap().is_empty());
        // 3: covered by the persisted rule — no prompt consumed.
        assert!(rt.dispatch(call()).await.unwrap().is_empty());
        // The rule survives a store reload under the project key.
        let store = ApprovalStore::load(dir.path().join("approvals.json"));
        assert_eq!(store.rules_for(dir.path()).len(), 1);

        // 4: a differently-gated tool still prompts; dismissal denies.
        let returns = rt
            .dispatch(tool_call("edit_file", serde_json::json!({"path": "a.rs"})))
            .await
            .unwrap();
        assert!(matches!(&returns[..], [EventReturn::ToolBlock { reason }]
                if reason.contains("denied")));
        host.await.unwrap();
    }

    #[tokio::test]
    async fn approvals_command_lists_and_resets() {
        let dir = tempfile::tempdir().unwrap();
        let rt = runtime_with(ApprovalsConfig::default(), dir.path());
        let out = rt
            .invoke_command_sync("approvals", "")
            .unwrap()
            .expect("registered");
        let listing = out.message.unwrap();
        assert!(listing.contains("[default] ask repl *"));
        assert!(listing.contains("[default] ask bash *"));

        // Persist an answer, then reset clears it.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        rt.connect_ui(tx);
        let host = tokio::spawn(async move {
            let req = rx.recv().await.expect("prompt");
            req.reply.send(Some(1)).unwrap();
            rx
        });
        assert!(
            rt.dispatch(tool_call("bash", serde_json::json!({"cmd": "ls"})))
                .await
                .unwrap()
                .is_empty()
        );
        host.await.unwrap();
        let listing = rt
            .invoke_command_sync("approvals", "")
            .unwrap()
            .unwrap()
            .message
            .unwrap();
        assert!(listing.contains("[saved] allow bash *"));

        rt.invoke_command_sync("approvals", "reset").unwrap();
        let listing = rt
            .invoke_command_sync("approvals", "")
            .unwrap()
            .unwrap()
            .message
            .unwrap();
        assert!(!listing.contains("[saved]"));
        assert!(
            ApprovalStore::load(dir.path().join("approvals.json"))
                .rules_for(dir.path())
                .is_empty()
        );
    }
}
