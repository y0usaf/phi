//! Mux `:` colon commands, registered through the same `register_command`
//! API as session slash commands (The Rule), tagged `CommandContext::Mux`.
//! Handlers are declarative: they return `UiAction`s that the mux host
//! interprets (`OpenProject`, `OpenOverlay`, `RunHostAction`, `Quit`).

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Result, bail};
use phi_ext::{
    CommandContext, CommandInvocation, CommandOutput, CommandSpec, Extension, ExtensionHost,
    ExtensionManifest, UiAction,
};

pub struct MuxCommandsExtension;

fn mux_command(
    name: &str,
    aliases: &[&str],
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
        context: CommandContext::Mux,
        aliases: aliases.iter().map(|a| (*a).to_owned()).collect(),
    }
}

/// A mux command whose whole effect is a single fixed action.
fn action_command(
    name: &str,
    aliases: &[&str],
    description: &str,
    action: UiAction,
) -> CommandSpec {
    mux_command(name, aliases, "", description, move |_| {
        Ok(CommandOutput::action(action.clone()))
    })
}

/// A mux command that maps onto a named host action.
fn host_action_command(
    name: &str,
    aliases: &[&str],
    description: &str,
    host_action: &str,
) -> CommandSpec {
    action_command(
        name,
        aliases,
        description,
        UiAction::RunHostAction {
            name: host_action.to_owned(),
        },
    )
}

fn overlay_command(name: &str, aliases: &[&str], description: &str, overlay: &str) -> CommandSpec {
    action_command(
        name,
        aliases,
        description,
        UiAction::OpenOverlay {
            name: overlay.to_owned(),
            payload: None,
        },
    )
}

/// Parse the (possibly quoted) path argument of `:open`. Error strings match
/// the pre-registry parser so mux status notes are unchanged.
fn parse_path_argument(input: &str) -> Result<String> {
    let input = input.trim();
    if input.is_empty() {
        bail!("usage: :open <dir>");
    }

    let mut chars = input.chars();
    let Some(first) = chars.next() else {
        bail!("usage: :open <dir>");
    };
    if first != '\'' && first != '"' {
        return Ok(input.to_owned());
    }
    if !input.ends_with(first) || input.len() == first.len_utf8() {
        bail!("open: unterminated quoted path");
    }
    Ok(input[first.len_utf8()..input.len() - first.len_utf8()].to_owned())
}

fn expand_home_path(path: &str) -> PathBuf {
    if path == "~" {
        return home_dir_path();
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return home_dir_path().join(rest);
    }
    PathBuf::from(path)
}

fn home_dir_path() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("~"))
}

impl Extension for MuxCommandsExtension {
    fn manifest(&self) -> ExtensionManifest {
        ExtensionManifest {
            id: "phi:mux-commands".to_owned(),
            name: "Mux commands".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            description: "Default mux colon commands (open, archive, navigation, ...)".to_owned(),
        }
    }

    fn register(&self, host: &mut dyn ExtensionHost) -> Result<()> {
        host.register_command(mux_command(
            "open",
            &["o"],
            "<dir>",
            "open a directory as a project",
            |inv| {
                let path = parse_path_argument(&inv.raw_args)?;
                Ok(CommandOutput::action(UiAction::OpenProject(
                    expand_home_path(&path),
                )))
            },
        ))?;
        host.register_command(overlay_command(
            "archive",
            &["archives"],
            "browse archived sessions",
            phi_ext::OVERLAY_ARCHIVE,
        ))?;
        host.register_command(host_action_command(
            "cleanup",
            &["clean"],
            "remove old archived sessions",
            "cleanup",
        ))?;
        host.register_command(host_action_command(
            "refresh",
            &[],
            "refresh the current session",
            "refresh",
        ))?;
        host.register_command(host_action_command(
            "reload",
            &[],
            "refresh all sessions",
            "reload",
        ))?;
        host.register_command(overlay_command(
            "usage",
            &[],
            "show usage totals",
            phi_ext::OVERLAY_USAGE,
        ))?;
        host.register_command(action_command(
            "quit",
            &["q"],
            "quit the mux",
            UiAction::Quit,
        ))?;
        host.register_command(overlay_command(
            "help",
            &["h"],
            "show commands and keys",
            phi_ext::OVERLAY_HELP,
        ))?;
        host.register_command(host_action_command(
            "up",
            &["prevsession"],
            "previous session",
            "session-prev",
        ))?;
        host.register_command(host_action_command(
            "down",
            &["nextsession"],
            "next session",
            "session-next",
        ))?;
        host.register_command(host_action_command(
            "left",
            &["prevproject"],
            "previous project",
            "project-prev",
        ))?;
        host.register_command(host_action_command(
            "right",
            &["nextproject"],
            "next project",
            "project-next",
        ))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phi_ext::RuntimeBuilder;

    fn runtime() -> std::sync::Arc<phi_ext::AppRuntime> {
        std::sync::Arc::new(
            RuntimeBuilder::new()
                .with_cwd(std::path::Path::new("."))
                .register_extension(MuxCommandsExtension)
                .build()
                .expect("build runtime"),
        )
    }

    fn invoke(rt: &phi_ext::AppRuntime, name: &str, args: &str) -> CommandOutput {
        rt.invoke_command_sync_in(CommandContext::Mux, name, args)
            .expect("invoke")
            .expect("command registered")
    }

    #[test]
    fn open_accepts_paths_with_spaces() {
        let rt = runtime();
        let out = invoke(&rt, "open", "/tmp/project path");
        assert_eq!(
            out.actions,
            vec![UiAction::OpenProject(PathBuf::from("/tmp/project path"))]
        );
    }

    #[test]
    fn open_strips_matching_quotes() {
        let rt = runtime();
        let out = invoke(&rt, "open", "\"/tmp/project path\"");
        assert_eq!(
            out.actions,
            vec![UiAction::OpenProject(PathBuf::from("/tmp/project path"))]
        );
    }

    #[test]
    fn open_expands_home_prefix() {
        let rt = runtime();
        let out = invoke(&rt, "open", "~/project");
        assert_eq!(
            out.actions,
            vec![UiAction::OpenProject(home_dir_path().join("project"))]
        );
    }

    #[test]
    fn open_errors_preserve_legacy_text() {
        let rt = runtime();
        let err = rt
            .invoke_command_sync_in(CommandContext::Mux, "open", "")
            .unwrap_err();
        assert!(err.to_string().contains("usage: :open <dir>"));
        let err = rt
            .invoke_command_sync_in(CommandContext::Mux, "open", "\"/tmp/unterminated")
            .unwrap_err();
        assert!(err.to_string().contains("open: unterminated quoted path"));
    }

    #[test]
    fn aliases_resolve_within_mux_context() {
        let rt = runtime();
        let out = invoke(&rt, "q", "");
        assert_eq!(out.actions, vec![UiAction::Quit]);
        let out = invoke(&rt, "o", "/tmp");
        assert_eq!(out.actions, vec![UiAction::OpenProject(PathBuf::from("/tmp"))]);
        let out = invoke(&rt, "archives", "");
        assert_eq!(
            out.actions,
            vec![UiAction::OpenOverlay {
                name: phi_ext::OVERLAY_ARCHIVE.to_owned(),
                payload: None,
            }]
        );
        let out = invoke(&rt, "prevsession", "");
        assert_eq!(
            out.actions,
            vec![UiAction::RunHostAction {
                name: "session-prev".to_owned(),
            }]
        );
    }

    #[test]
    fn navigation_commands_map_to_host_actions() {
        let rt = runtime();
        for (name, action) in [
            ("up", "session-prev"),
            ("down", "session-next"),
            ("left", "project-prev"),
            ("right", "project-next"),
            ("cleanup", "cleanup"),
            ("refresh", "refresh"),
            ("reload", "reload"),
        ] {
            let out = invoke(&rt, name, "");
            assert_eq!(
                out.actions,
                vec![UiAction::RunHostAction {
                    name: action.to_owned(),
                }]
            );
        }
    }

    #[test]
    fn mux_commands_hidden_from_session_listing() {
        let rt = runtime();
        assert!(rt.list_commands().is_empty());
        let mux: Vec<String> = rt
            .list_commands_in(CommandContext::Mux)
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert!(mux.contains(&"open".to_owned()));
        // Aliases are resolved but not listed.
        assert!(!mux.contains(&"o".to_owned()));
        // Session-context lookup does not see mux commands.
        assert!(rt.invoke_command_sync("quit", "").unwrap().is_none());
    }
}
