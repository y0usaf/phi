//! edit — exact-string replacements, each required to match exactly once.
//! Edits apply in order against the evolving file (simpler than pi's
//! against-the-original model; same effect for non-overlapping edits).

use serde_json::json;

pub fn register(host: &phi_host::Host) -> anyhow::Result<()> {
    host.register_tool(
        "edit",
        "Edit a file by exact string replacement. Each edit's old_text must \
         match the file exactly once (including whitespace); include enough \
         surrounding context to make it unique.",
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File to edit" },
                "edits": {
                    "type": "array",
                    "description": "Replacements applied in order",
                    "items": {
                        "type": "object",
                        "properties": {
                            "old_text": { "type": "string", "description": "Exact text to find (must be unique in the file)" },
                            "new_text": { "type": "string", "description": "Replacement text" }
                        },
                        "required": ["old_text", "new_text"]
                    }
                }
            },
            "required": ["path", "edits"]
        }),
        super::SOURCE,
        Box::new(execute),
    )
}

fn execute(args: &serde_json::Value) -> Result<String, String> {
    let path = super::require_str(args, "path")?;
    let edits = args
        .get("edits")
        .and_then(|v| v.as_array())
        .filter(|a| !a.is_empty())
        .ok_or("'edits' must be a non-empty array of { old_text, new_text }")?;

    let original = std::fs::read_to_string(path).map_err(|e| format!("read {path}: {e}"))?;
    let mut text = original;

    for (i, edit) in edits.iter().enumerate() {
        let n = i + 1;
        let old = edit
            .get("old_text")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| format!("edit {n}: 'old_text' must be a non-empty string"))?;
        let new = edit
            .get("new_text")
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("edit {n}: 'new_text' must be a string"))?;
        if old == new {
            return Err(format!("edit {n}: old_text and new_text are identical"));
        }
        match text.matches(old).count() {
            0 => {
                return Err(format!(
                    "edit {n}: old_text not found in {path} (it must match exactly, including whitespace)"
                ));
            }
            1 => text = text.replacen(old, new, 1),
            n_matches => {
                return Err(format!(
                    "edit {n}: old_text matches {n_matches} locations in {path}; add surrounding context to make it unique"
                ));
            }
        }
    }

    std::fs::write(path, &text).map_err(|e| format!("write {path}: {e}"))?;
    let plural = if edits.len() == 1 { "" } else { "s" };
    Ok(format!(
        "Edited {path} ({} replacement{plural})",
        edits.len()
    ))
}

#[cfg(test)]
mod tests {
    use super::execute;
    use serde_json::json;

    fn file_with(content: &str) -> tempfile::NamedTempFile {
        let f = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(f.path(), content).unwrap();
        f
    }

    #[test]
    fn applies_edits_in_order() {
        let f = file_with("fn a() {}\nfn b() {}\n");
        let out = execute(&json!({
            "path": f.path(),
            "edits": [
                { "old_text": "fn a() {}", "new_text": "fn a() { todo!() }" },
                { "old_text": "fn b() {}", "new_text": "fn c() {}" }
            ]
        }))
        .unwrap();
        assert!(out.contains("2 replacements"), "{out}");
        let text = std::fs::read_to_string(f.path()).unwrap();
        assert_eq!(text, "fn a() { todo!() }\nfn c() {}\n");
    }

    #[test]
    fn ambiguous_match_is_rejected_without_writing() {
        let f = file_with("x = 1\nx = 1\n");
        let err = execute(&json!({
            "path": f.path(),
            "edits": [{ "old_text": "x = 1", "new_text": "x = 2" }]
        }))
        .unwrap_err();
        assert!(err.contains("matches 2 locations"), "{err}");
        assert_eq!(std::fs::read_to_string(f.path()).unwrap(), "x = 1\nx = 1\n");
    }

    #[test]
    fn missing_match_is_rejected() {
        let f = file_with("hello\n");
        let err = execute(&json!({
            "path": f.path(),
            "edits": [{ "old_text": "goodbye", "new_text": "farewell" }]
        }))
        .unwrap_err();
        assert!(err.contains("not found"), "{err}");
    }

    #[test]
    fn identical_old_and_new_is_rejected() {
        let f = file_with("a\n");
        let err = execute(&json!({
            "path": f.path(),
            "edits": [{ "old_text": "a", "new_text": "a" }]
        }))
        .unwrap_err();
        assert!(err.contains("identical"), "{err}");
    }
}
