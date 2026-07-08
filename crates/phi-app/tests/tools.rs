//! Golden port of the read/write/ls sections of the spec's
//! `test/tools.test.ts` (pi v0.79.0), run against the embedded builtin
//! tools pack through the public host path (`load_embedded` +
//! `call_tool`) — the same seam the WS4 agent loop will use.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use phi_app::builtins::TOOLS_PACK;
use phi_host::{Host, HostConfig};
use serde_json::{Value, json};

fn host(cwd: &std::path::Path) -> Host {
    let host = Host::new(HostConfig {
        cwd: Some(cwd.to_string_lossy().into_owned()),
        ..HostConfig::default()
    })
    .expect("host boots");
    let report = host.load_embedded(&[TOOLS_PACK]);
    assert!(report.errors.is_empty(), "load errors: {:?}", report.errors);
    host
}

/// The spec test's `getTextOutput` helper.
fn text_output(result: &Value) -> String {
    result["content"]
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .filter(|c| c["type"] == "text")
                .map(|c| c["text"].as_str().unwrap_or(""))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

fn call(host: &Host, tool: &str, params: Value) -> Result<Value, String> {
    host.call_tool(tool, "test-call", &params)
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// registration
// ---------------------------------------------------------------------------

#[test]
fn pack_registers_the_slice_tools_in_order() {
    let dir = tempfile::tempdir().expect("tempdir");
    let host = host(dir.path());
    let names: Vec<String> = host
        .tools()
        .expect("tools mirror")
        .into_iter()
        .map(|t| t.name)
        .collect();
    assert_eq!(names, ["read", "write", "ls"]);
}

// ---------------------------------------------------------------------------
// read tool
// ---------------------------------------------------------------------------

#[test]
fn read_file_contents_that_fit_within_limits() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("test.txt");
    let content = "Hello, world!\nLine 2\nLine 3";
    std::fs::write(&file, content).unwrap();

    let result = call(&host(dir.path()), "read", json!({ "path": file })).unwrap();
    assert_eq!(text_output(&result), content);
    assert!(!text_output(&result).contains("Use offset="));
    assert!(result.get("details").is_none());
}

#[test]
fn read_handles_non_existent_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("nonexistent.txt");
    let err = call(&host(dir.path()), "read", json!({ "path": file })).unwrap_err();
    assert!(err.contains("ENOENT"), "got: {err}");
}

#[test]
fn read_truncates_files_exceeding_line_limit() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("large.txt");
    let lines: Vec<String> = (1..=2500).map(|i| format!("Line {i}")).collect();
    std::fs::write(&file, lines.join("\n")).unwrap();

    let result = call(&host(dir.path()), "read", json!({ "path": file })).unwrap();
    let output = text_output(&result);
    assert!(output.contains("Line 1"));
    assert!(output.contains("Line 2000"));
    assert!(!output.contains("Line 2001"));
    assert!(output.contains("[Showing lines 1-2000 of 2500. Use offset=2001 to continue.]"));
}

#[test]
fn read_truncates_when_byte_limit_exceeded() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("large-bytes.txt");
    // Exceeds the 50KB byte limit with fewer than 2000 lines.
    let lines: Vec<String> = (1..=500)
        .map(|i| format!("Line {i}: {}", "x".repeat(200)))
        .collect();
    std::fs::write(&file, lines.join("\n")).unwrap();

    let result = call(&host(dir.path()), "read", json!({ "path": file })).unwrap();
    let output = text_output(&result);
    assert!(output.contains("Line 1:"));
    // Spec regex: /\[Showing lines 1-\d+ of 500 \(.* limit\)\. Use offset=\d+ to continue\.\]/
    assert!(output.contains("[Showing lines 1-"), "got tail: {output}");
    assert!(
        output.contains(" of 500 (50.0KB limit). Use offset="),
        "got tail: {output}"
    );
}

#[test]
fn read_handles_offset_parameter() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("offset-test.txt");
    let lines: Vec<String> = (1..=100).map(|i| format!("Line {i}")).collect();
    std::fs::write(&file, lines.join("\n")).unwrap();

    let result = call(
        &host(dir.path()),
        "read",
        json!({ "path": file, "offset": 51 }),
    )
    .unwrap();
    let output = text_output(&result);
    assert!(!output.contains("Line 50\n"));
    assert!(output.contains("Line 51"));
    assert!(output.contains("Line 100"));
    assert!(!output.contains("Use offset="));
}

#[test]
fn read_handles_limit_parameter() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("limit-test.txt");
    let lines: Vec<String> = (1..=100).map(|i| format!("Line {i}")).collect();
    std::fs::write(&file, lines.join("\n")).unwrap();

    let result = call(
        &host(dir.path()),
        "read",
        json!({ "path": file, "limit": 10 }),
    )
    .unwrap();
    let output = text_output(&result);
    assert!(output.contains("Line 1"));
    assert!(output.contains("Line 10"));
    assert!(!output.contains("Line 11"));
    assert!(output.contains("[90 more lines in file. Use offset=11 to continue.]"));
}

#[test]
fn read_handles_offset_and_limit_together() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("offset-limit-test.txt");
    let lines: Vec<String> = (1..=100).map(|i| format!("Line {i}")).collect();
    std::fs::write(&file, lines.join("\n")).unwrap();

    let result = call(
        &host(dir.path()),
        "read",
        json!({ "path": file, "offset": 41, "limit": 20 }),
    )
    .unwrap();
    let output = text_output(&result);
    assert!(!output.contains("Line 40\n"));
    assert!(output.contains("Line 41"));
    assert!(output.contains("Line 60"));
    assert!(!output.contains("Line 61"));
    assert!(output.contains("[40 more lines in file. Use offset=61 to continue.]"));
}

#[test]
fn read_errors_when_offset_is_beyond_file_length() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("short.txt");
    std::fs::write(&file, "Line 1\nLine 2\nLine 3").unwrap();

    let err = call(
        &host(dir.path()),
        "read",
        json!({ "path": file, "offset": 100 }),
    )
    .unwrap_err();
    assert!(
        err.contains("Offset 100 is beyond end of file (3 lines total)"),
        "got: {err}"
    );
}

#[test]
fn read_includes_truncation_details_when_truncated() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("large-file.txt");
    let lines: Vec<String> = (1..=2500).map(|i| format!("Line {i}")).collect();
    std::fs::write(&file, lines.join("\n")).unwrap();

    let result = call(&host(dir.path()), "read", json!({ "path": file })).unwrap();
    let truncation = &result["details"]["truncation"];
    assert_eq!(truncation["truncated"], true);
    assert_eq!(truncation["truncatedBy"], "lines");
    assert_eq!(truncation["totalLines"], 2500);
    assert_eq!(truncation["outputLines"], 2000);
}

/// The spec test's 1x1 transparent PNG fixture, decoded from its base64
/// literal.
fn png_1x1() -> Vec<u8> {
    const B64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGNgYGD4DwABBAEAX+XDSwAAAABJRU5ErkJggg==";
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::new();
    let vals: Vec<u32> = B64
        .bytes()
        .filter(|b| *b != b'=')
        .map(|b| CHARS.iter().position(|c| *c == b).unwrap() as u32)
        .collect();
    for chunk in vals.chunks(4) {
        let mut v: u32 = 0;
        for (i, c) in chunk.iter().enumerate() {
            v |= c << (18 - 6 * i);
        }
        let bytes = [(v >> 16) as u8, (v >> 8) as u8, v as u8];
        out.extend_from_slice(&bytes[..chunk.len() - 1]);
    }
    out
}

#[test]
fn read_detects_image_mime_type_from_file_magic_not_extension() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("image.txt");
    std::fs::write(&file, png_1x1()).unwrap();

    let result = call(&host(dir.path()), "read", json!({ "path": file })).unwrap();
    assert_eq!(result["content"][0]["type"], "text");
    assert!(text_output(&result).contains("Read image file [image/png]"));

    let image = result["content"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["type"] == "image")
        .expect("image block");
    assert_eq!(image["mimeType"], "image/png");
    assert!(!image["data"].as_str().unwrap().is_empty());
}

#[test]
fn read_treats_image_extension_with_non_image_content_as_text() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("not-an-image.png");
    std::fs::write(&file, "definitely not a png").unwrap();

    let result = call(&host(dir.path()), "read", json!({ "path": file })).unwrap();
    let output = text_output(&result);
    assert!(output.contains("definitely not a png"));
    assert!(
        !result["content"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["type"] == "image")
    );
}

// ---------------------------------------------------------------------------
// write tool
// ---------------------------------------------------------------------------

#[test]
fn write_writes_file_contents() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("write-test.txt");

    let result = call(
        &host(dir.path()),
        "write",
        json!({ "path": file, "content": "Test content" }),
    )
    .unwrap();
    let output = text_output(&result);
    assert!(output.contains("Successfully wrote"));
    assert!(output.contains(file.to_str().unwrap()));
    assert!(result.get("details").is_none());
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "Test content");
}

#[test]
fn write_creates_parent_directories() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("nested").join("dir").join("test.txt");

    let result = call(
        &host(dir.path()),
        "write",
        json!({ "path": file, "content": "Nested content" }),
    )
    .unwrap();
    assert!(text_output(&result).contains("Successfully wrote"));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "Nested content");
}

// ---------------------------------------------------------------------------
// ls tool
// ---------------------------------------------------------------------------

#[test]
fn ls_lists_dotfiles_and_directories() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join(".hidden-file"), "secret").unwrap();
    std::fs::create_dir(dir.path().join(".hidden-dir")).unwrap();

    let result = call(&host(dir.path()), "ls", json!({ "path": dir.path() })).unwrap();
    let output = text_output(&result);
    assert!(output.contains(".hidden-file"));
    assert!(output.contains(".hidden-dir/"));
}

#[test]
fn ls_reports_missing_and_non_directory_paths() {
    let dir = tempfile::tempdir().expect("tempdir");
    let host = host(dir.path());

    let err = call(&host, "ls", json!({ "path": dir.path().join("missing") })).unwrap_err();
    assert!(err.contains("Path not found:"), "got: {err}");

    let file = dir.path().join("plain.txt");
    std::fs::write(&file, "x").unwrap();
    let err = call(&host, "ls", json!({ "path": file })).unwrap_err();
    assert!(err.contains("Not a directory:"), "got: {err}");
}

#[test]
fn ls_caps_entries_and_reports_the_limit() {
    let dir = tempfile::tempdir().expect("tempdir");
    for i in 0..10 {
        std::fs::write(dir.path().join(format!("f{i:02}")), "x").unwrap();
    }

    let result = call(
        &host(dir.path()),
        "ls",
        json!({ "path": dir.path(), "limit": 4 }),
    )
    .unwrap();
    let output = text_output(&result);
    assert!(output.contains("f00"));
    assert!(output.contains("f03"));
    assert!(!output.contains("f04"));
    assert!(output.contains("[4 entries limit reached. Use limit=8 for more]"));
    assert_eq!(result["details"]["entryLimitReached"], 4);
}

#[test]
fn ls_defaults_to_the_tool_cwd() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("here.txt"), "x").unwrap();

    let result = call(&host(dir.path()), "ls", json!({})).unwrap();
    assert!(text_output(&result).contains("here.txt"));
}
