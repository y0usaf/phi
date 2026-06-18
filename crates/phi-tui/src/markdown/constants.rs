use std::sync::LazyLock;

use syntect::highlighting::{Theme, ThemeSet};
use syntect::parsing::SyntaxSet;

pub(super) const RESET: &str = "\x1b[0m";
pub(super) const BOLD: &str = "\x1b[1m";
pub(super) const DIM: &str = "\x1b[2m";
pub(super) const ITALIC: &str = "\x1b[3m";
pub(super) const UNDERLINE: &str = "\x1b[4m";
pub(super) const CYAN: &str = "\x1b[36m";
pub(super) const REPL_FENCE: &str = "\x1b[38;2;180;140;255m";
pub(super) const REPL_FN: &str = "\x1b[38;2;255;210;120m";
pub(super) const MARKER_ACCENT: &str = "\x1b[38;2;130;160;230m";
pub(super) const TASK_DONE: &str = "\x1b[38;2;130;200;130m";
pub(super) const QUOTE_BAR: &str = "\x1b[38;2;150;130;220m";
pub(super) const SUBHEAD: &str = "\x1b[38;2;190;150;255m";
pub(super) const OUT_BASE: &str = "\x1b[38;2;150;155;165m";
pub(super) const OUT_ERR: &str = "\x1b[38;2;240;120;120m";
pub(super) const OUT_WARN: &str = "\x1b[38;2;235;185;100m";
pub(super) const OUT_OK: &str = "\x1b[38;2;130;200;140m";
pub(super) const OUT_PATH: &str = "\x1b[38;2;130;180;230m";
pub(super) const OUT_NUM: &str = "\x1b[38;2;170;150;230m";
pub(super) const OUT_STR: &str = "\x1b[38;2;150;190;150m";
pub(super) const OUT_URL: &str = "\x1b[38;2;120;200;220m";
pub(super) const DIFF_ADD: &str = "\x1b[38;2;120;200;130m";
pub(super) const DIFF_DEL: &str = "\x1b[38;2;230;110;110m";

pub(super) static SYNTAXES: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);
pub(super) static THEME: LazyLock<Theme> = LazyLock::new(|| {
    ThemeSet::load_defaults()
        .themes
        .remove("base16-ocean.dark")
        .expect("bundled theme")
});