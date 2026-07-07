//! write — create or overwrite a file, creating parent directories.

use serde_json::json;

pub fn register(host: &phi_host::Host) -> anyhow::Result<()> {
    host.register_tool(
        "write",
        "Write a file, creating it (and any parent directories) or \
         overwriting it entirely. Prefer edit for changes to existing files.",
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File to write" },
                "content": { "type": "string", "description": "Full file contents" }
            },
            "required": ["path", "content"]
        }),
        super::SOURCE,
        Box::new(execute),
    )
}

fn execute(args: &serde_json::Value) -> Result<String, String> {
    let path = super::require_str(args, "path")?;
    let content = args
        .get("content")
        .and_then(|v| v.as_str())
        .ok_or("'content' must be a string")?;
    if let Some(parent) = std::path::Path::new(path).parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    std::fs::write(path, content).map_err(|e| format!("write {path}: {e}"))?;
    Ok(format!("Wrote {path} ({} bytes)", content.len()))
}

#[cfg(test)]
mod tests {
    use super::execute;
    use serde_json::json;

    #[test]
    fn creates_parents_and_writes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a/b/c.txt");
        let out = execute(&json!({ "path": path, "content": "hi" })).unwrap();
        assert!(out.contains("2 bytes"), "{out}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hi");
    }

    #[test]
    fn overwrites_existing() {
        let f = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(f.path(), "old").unwrap();
        execute(&json!({ "path": f.path(), "content": "new" })).unwrap();
        assert_eq!(std::fs::read_to_string(f.path()).unwrap(), "new");
    }
}
