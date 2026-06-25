//! `:` command parsing, ported from pi-harness `tui/command.rs`.

mod keymap;

pub use keymap::*;



use std::path::PathBuf;


#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MuxCommand {
    Open(PathBuf),
    Archive,
    Cleanup,
    Refresh,
    Reload,
    Usage,
    Quit,
    Help,
    Action(AppAction),
}

pub fn parse_command(input: &str) -> Result<MuxCommand, String> {
    let input = input
        .trim()
        .strip_prefix(':')
        .unwrap_or(input.trim())
        .trim();
    if input.is_empty() {
        return Err("empty command".to_string());
    }

    let name_end = input.find(char::is_whitespace).unwrap_or(input.len());
    let name = input[..name_end].to_ascii_lowercase();
    let rest = input[name_end..].trim();
    match name.as_str() {
        "open" | "o" => {
            let path = parse_path_argument(rest)?;
            Ok(MuxCommand::Open(expand_home_path(&path)))
        }
        "archive" | "archives" => Ok(MuxCommand::Archive),
        "cleanup" | "clean" => Ok(MuxCommand::Cleanup),
        "usage" => Ok(MuxCommand::Usage),
        "refresh" => Ok(MuxCommand::Refresh),
        "reload" => Ok(MuxCommand::Reload),
        "q" | "quit" => Ok(MuxCommand::Quit),
        "h" | "help" => Ok(MuxCommand::Help),
        "up" | "prevsession" => Ok(MuxCommand::Action(AppAction::PreviousSession)),
        "down" | "nextsession" => Ok(MuxCommand::Action(AppAction::NextSession)),
        "left" | "prevproject" => Ok(MuxCommand::Action(AppAction::PreviousProject)),
        "right" | "nextproject" => Ok(MuxCommand::Action(AppAction::NextProject)),
        _ => Err(format!("unknown command: :{name}")),
    }
}

fn parse_path_argument(input: &str) -> Result<String, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("usage: :open <dir>".to_string());
    }

    let mut chars = input.chars();
    let Some(first) = chars.next() else {
        return Err("usage: :open <dir>".to_string());
    };
    if first != '\'' && first != '"' {
        return Ok(input.to_string());
    }
    if !input.ends_with(first) || input.len() == first.len_utf8() {
        return Err("open: unterminated quoted path".to_string());
    }
    Ok(input[first.len_utf8()..input.len() - first.len_utf8()].to_string())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_open_command_accepts_colon_prefix_and_spaces() {
        assert_eq!(
            parse_command(":open /tmp/project path").unwrap(),
            MuxCommand::Open(PathBuf::from("/tmp/project path"))
        );
    }

    #[test]
    fn parse_open_command_strips_matching_quotes() {
        assert_eq!(
            parse_command("open \"/tmp/project path\"").unwrap(),
            MuxCommand::Open(PathBuf::from("/tmp/project path"))
        );
    }

    #[test]
    fn parse_open_command_expands_home_prefix() {
        let home = home_dir_path();
        assert_eq!(
            parse_command("open ~/project").unwrap(),
            MuxCommand::Open(home.join("project"))
        );
    }

    #[test]
    fn parse_refresh_and_reload_commands() {
        assert_eq!(parse_command("refresh").unwrap(), MuxCommand::Refresh);
        assert_eq!(parse_command(":reload").unwrap(), MuxCommand::Reload);
    }

    #[test]
    fn parse_archive_command() {
        assert_eq!(parse_command("archive").unwrap(), MuxCommand::Archive);
        assert_eq!(parse_command(":archives").unwrap(), MuxCommand::Archive);
    }

    #[test]
    fn parse_usage_command() {
        assert_eq!(parse_command("usage").unwrap(), MuxCommand::Usage);
        assert_eq!(parse_command(":usage").unwrap(), MuxCommand::Usage);
    }

    #[test]
    fn parse_navigation_commands() {
        assert_eq!(
            parse_command(":up").unwrap(),
            MuxCommand::Action(AppAction::PreviousSession)
        );
        assert_eq!(
            parse_command("nextsession").unwrap(),
            MuxCommand::Action(AppAction::NextSession)
        );
        assert_eq!(
            parse_command(":left").unwrap(),
            MuxCommand::Action(AppAction::PreviousProject)
        );
        assert_eq!(
            parse_command("nextproject").unwrap(),
            MuxCommand::Action(AppAction::NextProject)
        );
    }

    #[test]
    fn parse_unknown_command_errors() {
        assert!(parse_command("bogus").is_err());
        assert!(parse_command("   ").is_err());
    }
}
