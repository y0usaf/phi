//! Built-in slash commands, registered through the same `register_command`
//! API user extensions use (The Rule). Handlers return declarative
//! `UiAction`s that the frontend interprets; none of them touch UI state.

use std::sync::Arc;

use anyhow::Result;
use phi_ext::{
    ArgCompletionFn, CommandInvocation, CommandOutput, CommandSpec, Extension, ExtensionHost,
    ExtensionManifest, PickerKind, UiAction,
};

/// OAuth providers offered by `/login` and `/logout` completion. Must match
/// what `OAuthProviderId::parse` accepts.
const OAUTH_PROVIDERS: &[&str] = &["anthropic", "openai"];

pub struct BuiltinCommandsExtension {
    /// Qualified `provider/id` names offered by `/model` and `/submodel`
    /// argument completion.
    model_names: Vec<String>,
}

impl BuiltinCommandsExtension {
    pub fn new(model_names: Vec<String>) -> Self {
        Self { model_names }
    }
}

fn command(
    name: &str,
    args_hint: &str,
    description: &str,
    handler: impl Fn(CommandInvocation) -> Result<CommandOutput> + Send + Sync + 'static,
) -> CommandSpec {
    CommandSpec {
        name: name.to_owned(),
        args_hint: args_hint.to_owned(),
        description: description.to_owned(),
        handler: Arc::new(handler),
        arg_completions: None,
    }
}

/// A command that either switches to the named model or opens the picker
/// when invoked without an argument.
fn model_command(
    name: &str,
    description: &str,
    root: bool,
    model_names: Vec<String>,
) -> CommandSpec {
    let mut spec = command(name, "[model]", description, move |inv| {
        let query = inv.raw_args.trim();
        Ok(if query.is_empty() {
            CommandOutput::action(UiAction::OpenOverlay {
                name: phi_ext::OVERLAY_MODEL.to_owned(),
                payload: Some(serde_json::json!({ "root": root })),
            })
        } else {
            CommandOutput::action(UiAction::SwitchModel {
                root,
                query: query.to_owned(),
            })
        })
    });
    spec.arg_completions = Some(Arc::new(move || model_names.clone()));
    spec
}

fn provider_completions() -> ArgCompletionFn {
    Arc::new(|| OAUTH_PROVIDERS.iter().map(|p| (*p).to_owned()).collect())
}

impl Extension for BuiltinCommandsExtension {
    fn manifest(&self) -> ExtensionManifest {
        ExtensionManifest {
            id: "phi.commands".to_owned(),
            name: "Built-in commands".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            description: "Default slash commands (session, model, auth, help)".to_owned(),
        }
    }

    fn register(&self, host: &mut dyn ExtensionHost) -> Result<()> {
        let mut login = command(
            "login",
            "[provider]",
            "authenticate via OAuth (anthropic | openai)",
            |inv| {
                Ok(CommandOutput::action(UiAction::StartLogin {
                    provider: inv.raw_args.trim().to_owned(),
                }))
            },
        );
        login.arg_completions = Some(provider_completions());
        host.register_command(login)?;

        let mut logout = command("logout", "[provider]", "remove stored credentials", |inv| {
            Ok(CommandOutput::action(UiAction::Logout {
                provider: inv.raw_args.trim().to_owned(),
            }))
        });
        logout.arg_completions = Some(provider_completions());
        host.register_command(logout)?;

        host.register_command(model_command(
            "model",
            "switch the root model (no arg: open the selector)",
            true,
            self.model_names.clone(),
        ))?;
        host.register_command(model_command(
            "submodel",
            "switch the sub model used by llm_query",
            false,
            self.model_names.clone(),
        ))?;

        host.register_command(command(
            "resume",
            "",
            "pick a past session for this directory to resume",
            |_| {
                Ok(CommandOutput::action(UiAction::OpenPicker(
                    PickerKind::Resume,
                )))
            },
        ))?;
        host.register_command(command(
            "tree",
            "",
            "navigate the session tree (also esc esc)",
            |_| {
                Ok(CommandOutput::action(UiAction::OpenPicker(
                    PickerKind::Tree,
                )))
            },
        ))?;
        host.register_command(command(
            "fork",
            "",
            "fork a new session from a prior user message",
            |_| {
                Ok(CommandOutput::action(UiAction::OpenPicker(
                    PickerKind::Fork,
                )))
            },
        ))?;
        host.register_command(command(
            "new",
            "",
            "reset the session (fresh REPL, history cleared)",
            |_| Ok(CommandOutput::action(UiAction::NewSession)),
        ))?;
        host.register_command(command("usage", "", "show usage totals", |_| {
            Ok(CommandOutput::action(UiAction::OpenOverlay {
                name: phi_ext::OVERLAY_USAGE.to_owned(),
                payload: None,
            }))
        }))?;
        host.register_command(command("help", "", "show commands and keys", |_| {
            Ok(CommandOutput::action(UiAction::OpenOverlay {
                name: phi_ext::OVERLAY_HELP.to_owned(),
                payload: None,
            }))
        }))?;
        host.register_command(command(
            "quit",
            "",
            "exit (also ctrl-d on an empty line)",
            |_| Ok(CommandOutput::action(UiAction::Quit)),
        ))?;
        host.register_command(command("exit", "", "alias for /quit", |_| {
            Ok(CommandOutput::action(UiAction::Quit))
        }))?;

        let mut trust = command(
            "trust",
            "[revoke]",
            "trust this project's .phi/ extensions (revoke: untrust)",
            |inv| {
                let trusted = !inv.raw_args.trim().eq_ignore_ascii_case("revoke");
                Ok(CommandOutput::action(UiAction::TrustProject { trusted }))
            },
        );
        trust.arg_completions = Some(Arc::new(|| vec!["revoke".to_owned()]));
        host.register_command(trust)?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phi_ext::RuntimeBuilder;

    fn runtime() -> phi_ext::AppRuntime {
        RuntimeBuilder::new()
            .with_cwd(std::path::Path::new("."))
            .register_extension(BuiltinCommandsExtension::new(vec![
                "anthropic/claude-opus-4-7".to_owned(),
            ]))
            .build()
            .unwrap()
    }

    #[test]
    fn registers_all_builtin_commands() {
        let rt = runtime();
        let names: Vec<String> = rt.list_commands().into_iter().map(|c| c.name).collect();
        for expected in [
            "exit", "fork", "help", "login", "logout", "model", "new", "quit", "resume",
            "submodel", "tree", "trust", "usage",
        ] {
            assert!(names.contains(&expected.to_owned()), "missing {expected}");
        }
    }

    #[test]
    fn model_with_query_switches_and_without_opens_picker() {
        let rt = runtime();
        let out = rt.invoke_command_sync("model", "opus").unwrap().unwrap();
        assert_eq!(
            out.actions,
            vec![UiAction::SwitchModel {
                root: true,
                query: "opus".to_owned()
            }]
        );
        let out = rt.invoke_command_sync("model", "").unwrap().unwrap();
        assert_eq!(
            out.actions,
            vec![UiAction::OpenOverlay {
                name: phi_ext::OVERLAY_MODEL.to_owned(),
                payload: Some(serde_json::json!({ "root": true })),
            }]
        );
        let out = rt.invoke_command_sync("submodel", "gpt").unwrap().unwrap();
        assert_eq!(
            out.actions,
            vec![UiAction::SwitchModel {
                root: false,
                query: "gpt".to_owned()
            }]
        );
    }

    #[test]
    fn argument_completions_cover_models_and_providers() {
        let rt = runtime();
        assert_eq!(
            rt.complete_command_arg("model").unwrap(),
            vec!["anthropic/claude-opus-4-7".to_owned()]
        );
        assert_eq!(
            rt.complete_command_arg("login").unwrap(),
            vec!["anthropic".to_owned(), "openai".to_owned()]
        );
        assert!(rt.complete_command_arg("help").is_none());
        assert!(rt.complete_command_arg("nonexistent").is_none());
    }

    #[test]
    fn quit_and_exit_both_quit() {
        let rt = runtime();
        for name in ["quit", "exit"] {
            let out = rt.invoke_command_sync(name, "").unwrap().unwrap();
            assert_eq!(out.actions, vec![UiAction::Quit]);
        }
    }
}
