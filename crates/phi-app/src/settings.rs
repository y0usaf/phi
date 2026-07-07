//! Minimal settings: `<config>/phi/settings.json`, all keys optional.
//! CLI flags win over settings; settings win over built-in defaults.

#[derive(Debug, Default, serde::Deserialize)]
pub struct Settings {
    /// Default model id (optionally `provider/model-id`).
    pub model: Option<String>,
    /// Agent loop key; the loop is a pack selected by name, not hardwired
    /// (DESIGN.md WS2). Default: "flat".
    #[serde(rename = "loop")]
    pub loop_name: Option<String>,
    /// Default thinking level (off|minimal|low|medium|high|xhigh).
    pub thinking: Option<String>,
}

pub fn load() -> Settings {
    let Some(dirs) = directories::ProjectDirs::from("", "", "phi") else {
        return Settings::default();
    };
    let path = dirs.config_dir().join("settings.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Settings::default();
    };
    match serde_json::from_str(&text) {
        Ok(settings) => settings,
        Err(e) => {
            eprintln!("phi: ignoring malformed {}: {e}", path.display());
            Settings::default()
        }
    }
}
