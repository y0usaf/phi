//! read — file contents with offset/limit paging and head truncation.

use serde_json::json;

use crate::truncate;

pub fn register(host: &phi_host::Host) -> anyhow::Result<()> {
    host.register_tool(
        "read",
        "Read a file from the filesystem. Returns at most 2000 lines / 50KB \
         per call; use offset and limit to page through larger files.",
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path (absolute, or relative to the working directory)" },
                "offset": { "type": "integer", "description": "1-indexed line to start reading from" },
                "limit": { "type": "integer", "description": "Maximum number of lines to return" }
            },
            "required": ["path"]
        }),
        super::SOURCE,
        Box::new(execute),
    )
}

fn execute(args: &serde_json::Value) -> Result<String, String> {
    let path = super::require_str(args, "path")?;
    let offset = args
        .get("offset")
        .and_then(|v| v.as_u64())
        .map(|v| v.max(1) as usize)
        .unwrap_or(1);
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize);

    let bytes = std::fs::read(path).map_err(|e| format!("read {path}: {e}"))?;
    let text = String::from_utf8(bytes)
        .map_err(|_| format!("{path} is not valid UTF-8 (binary files are not supported)"))?;

    let total = text.lines().count();
    if total == 0 {
        return Ok("[file is empty]".to_owned());
    }
    if offset > total {
        return Err(format!(
            "offset {offset} is past the end of {path} ({total} lines)"
        ));
    }

    let selected: Vec<&str> = text
        .lines()
        .skip(offset - 1)
        .take(limit.unwrap_or(usize::MAX))
        .collect();
    let selected = selected.join("\n");
    let t = truncate::head(&selected, truncate::MAX_LINES, truncate::MAX_BYTES);

    let last = offset - 1 + t.kept;
    let mut out = t.text;
    if last < total {
        out.push_str(&format!(
            "\n[Showing lines {offset}-{last} of {total}. Use offset={} to continue.]",
            last + 1
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::execute;
    use serde_json::json;

    fn write_tmp(content: &str) -> tempfile::NamedTempFile {
        let f = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(f.path(), content).unwrap();
        f
    }

    #[test]
    fn reads_whole_file() {
        let f = write_tmp("one\ntwo\n");
        let out = execute(&json!({ "path": f.path() })).unwrap();
        assert_eq!(out, "one\ntwo\n");
    }

    #[test]
    fn pages_with_offset_and_limit() {
        let f = write_tmp("1\n2\n3\n4\n5\n");
        let out = execute(&json!({ "path": f.path(), "offset": 2, "limit": 2 })).unwrap();
        assert!(out.starts_with("2\n3\n"), "{out}");
        assert!(out.contains("[Showing lines 2-3 of 5. Use offset=4 to continue.]"));
    }

    #[test]
    fn missing_file_is_an_error() {
        let err = execute(&json!({ "path": "/nonexistent/x" })).unwrap_err();
        assert!(err.contains("/nonexistent/x"), "{err}");
    }

    #[test]
    fn offset_past_end_is_an_error() {
        let f = write_tmp("a\n");
        let err = execute(&json!({ "path": f.path(), "offset": 5 })).unwrap_err();
        assert!(err.contains("past the end"), "{err}");
    }

    #[test]
    fn empty_file_is_reported() {
        let f = write_tmp("");
        assert_eq!(
            execute(&json!({ "path": f.path() })).unwrap(),
            "[file is empty]"
        );
    }
}
