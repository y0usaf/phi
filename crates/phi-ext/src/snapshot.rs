use crate::{
    CommandSpec, EventKind, ExtensionManifest, FlagSpec, OverlaySpec, ShortcutSpec, ToolSpec,
};
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
pub struct RuntimeSnapshot {
    pub cwd: String,
    pub session_id: String,
    pub extensions: Vec<ExtensionManifest>,
    pub tools: Vec<ToolSummary>,
    pub commands: Vec<CommandSummary>,
    pub shortcuts: Vec<ShortcutSummary>,
    pub flags: Vec<FlagSummary>,
    pub event_handlers: Vec<EventHandlerSummary>,
    pub themes: Vec<ThemeSummary>,
    pub spinners: Vec<SpinnerSummary>,
    pub thinking_expressions: Vec<ThinkingExpressionSummary>,
    pub text_effects: Vec<TextEffectSummary>,
    pub overlays: Vec<OverlaySummary>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ToolSummary {
    pub name: String,
    pub description: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct CommandSummary {
    pub name: String,
    pub args_hint: String,
    pub description: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ShortcutSummary {
    pub key: String,
    pub description: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct FlagSummary {
    pub name: String,
    pub description: String,
    pub flag_type: String,
    pub default: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct EventHandlerSummary {
    pub event: EventKind,
    pub label: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ThemeSummary {
    pub name: String,
    pub role_count: usize,
    pub has_resolver: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct SpinnerSummary {
    pub name: String,
    pub frame_count: usize,
    pub frame_interval_ms: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ThinkingExpressionSummary {
    pub name: String,
    pub expression_count: usize,
    pub expression_interval_ms: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct TextEffectSummary {
    pub name: String,
    pub kind: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct OverlaySummary {
    pub name: String,
    pub description: String,
    pub toggle_key: Option<String>,
}

pub(crate) fn tool_summary(tool: &ToolSpec) -> ToolSummary {
    ToolSummary {
        name: tool.name.clone(),
        description: tool.description.clone(),
    }
}

pub(crate) fn command_summary(command: &CommandSpec) -> CommandSummary {
    CommandSummary {
        name: command.name.clone(),
        args_hint: command.args_hint.clone(),
        description: command.description.clone(),
    }
}

pub(crate) fn shortcut_summary(shortcut: &ShortcutSpec) -> ShortcutSummary {
    ShortcutSummary {
        key: shortcut.key.clone(),
        description: shortcut.description.clone(),
    }
}

pub(crate) fn flag_summary(flag: &FlagSpec) -> FlagSummary {
    FlagSummary {
        name: flag.name.clone(),
        description: flag.description.clone(),
        flag_type: match flag.flag_type {
            crate::FlagType::Boolean => "boolean".to_owned(),
            crate::FlagType::String => "string".to_owned(),
        },
        default: flag.default.clone(),
    }
}

pub(crate) fn spinner_summary(spinner: &crate::SpinnerSpec) -> SpinnerSummary {
    SpinnerSummary {
        name: spinner.name.clone(),
        frame_count: spinner.frames.len(),
        frame_interval_ms: spinner.frame_interval_ms,
    }
}

pub(crate) fn thinking_expression_summary(
    expr: &crate::ThinkingExpressionSpec,
) -> ThinkingExpressionSummary {
    ThinkingExpressionSummary {
        name: expr.name.clone(),
        expression_count: expr.expressions.len(),
        expression_interval_ms: expr.expression_interval_ms,
    }
}

pub(crate) fn text_effect_summary(effect: &crate::TextEffectSpec) -> TextEffectSummary {
    TextEffectSummary {
        name: effect.name.clone(),
        kind: effect.kind.clone(),
    }
}

pub(crate) fn overlay_summary(overlay: &OverlaySpec) -> OverlaySummary {
    OverlaySummary {
        name: overlay.name.clone(),
        description: overlay.description.clone(),
        toggle_key: overlay.toggle_key.clone(),
    }
}
