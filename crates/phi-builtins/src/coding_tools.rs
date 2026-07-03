//! WS9.3(b): the hybrid tool surface as a default-off builtin. Registers the
//! prelude's coding helpers (bash, read_file, write_file, edit_file, grep,
//! find, ls) with `expose: both` + `dispatch: Python`: the provider sees
//! native tool definitions, and every native call maps onto the *same
//! prelude functions in the same REPL env* (`__phi_native_call__`), so the
//! hashline cache, read-before-edit enforcement, and output spill are shared
//! regardless of surface — one implementation, two exposures.
//!
//! Enabled with `coding_tools_native = true` in config.lua. Enabling it *by
//! default* is gated on measurement (PLAN.md WS9.3b): REPL-only vs hybrid on
//! a fixed coding-task suite, compared on success rate, turns, and
//! wall-clock. No data, no default change.

use anyhow::Result;
use phi_ext::{
    Extension, ExtensionHost, ExtensionManifest, ToolDispatch, ToolExposure, ToolParam, ToolSpec,
};
use serde_json::json;

pub struct CodingToolsExtension;

impl Extension for CodingToolsExtension {
    fn manifest(&self) -> ExtensionManifest {
        ExtensionManifest {
            id: "phi:coding-tools-native".to_string(),
            name: "Phi Coding Tools (native exposure)".to_string(),
            version: "0.1.0".to_string(),
            description: "Prelude coding helpers as provider-level tool definitions \
                          (hybrid surface, WS9.3b)"
                .to_string(),
        }
    }

    fn register(&self, host: &mut dyn ExtensionHost) -> Result<()> {
        for spec in coding_tool_specs() {
            host.register_tool(spec)?;
        }
        Ok(())
    }
}

/// One spec per prelude coding helper. Params mirror the Python signatures
/// (defaults are Python literals); the explicit schemas add the per-field
/// descriptions the derived schema can't carry.
pub fn coding_tool_specs() -> Vec<ToolSpec> {
    fn param(name: &str, ty: &str, default: Option<&str>) -> ToolParam {
        ToolParam {
            name: name.to_owned(),
            ty: ty.to_owned(),
            default: default.map(str::to_owned),
        }
    }
    fn spec(
        name: &str,
        description: &str,
        params: Vec<ToolParam>,
        input_schema: serde_json::Value,
    ) -> ToolSpec {
        ToolSpec {
            name: name.to_owned(),
            description: description.to_owned(),
            params,
            expose: ToolExposure::Both,
            input_schema: Some(input_schema),
            dispatch: ToolDispatch::Python,
        }
    }

    vec![
        spec(
            "bash",
            "Run a shell command in the working directory; returns combined stdout/stderr \
             plus a note on non-zero exit. Output over 50KB spills to a tmp file with a \
             [full output: path] annotation. For long-running work (builds, test suites, \
             servers) prefer the repl tool with a background job: \
             jid = bg(\"out = bash('cargo build', timeout=3600)\").",
            vec![
                param("cmd", "str", None),
                param("timeout", "int", Some("120")),
            ],
            json!({
                "type": "object",
                "properties": {
                    "cmd": { "type": "string", "description": "Shell command to run." },
                    "timeout": { "type": "integer", "description": "Seconds before the command is killed (default 120)." },
                },
                "required": ["cmd"],
            }),
        ),
        spec(
            "read_file",
            "Read a file as `[path#TAG]` + `LINE:TEXT` hashline rows (default) or plain text \
             (raw=true). Repeated reads of an unchanged file return a one-line stub instead of \
             re-emitting content — pass fresh=true to force a full re-read.",
            vec![
                param("path", "str", None),
                param("offset", "int", Some("0")),
                param("limit", "int", Some("2000")),
                param("raw", "bool", Some("False")),
                param("fresh", "bool", Some("False")),
            ],
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File to read." },
                    "offset": { "type": "integer", "description": "Line to start from (default 0)." },
                    "limit": { "type": "integer", "description": "Max lines to read (default 2000)." },
                    "raw": { "type": "boolean", "description": "Plain text without line numbers or hashline tags (default false)." },
                    "fresh": { "type": "boolean", "description": "Force a full re-read of an unchanged file (default false)." },
                },
                "required": ["path"],
            }),
        ),
        spec(
            "write_file",
            "Write a file, creating parent directories. Refuses to overwrite an existing file \
             this session hasn't read — call read_file(path) first, then retry.",
            vec![param("path", "str", None), param("content", "str", None)],
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File to write." },
                    "content": { "type": "string", "description": "Full file content." },
                },
                "required": ["path", "content"],
            }),
        ),
        spec(
            "edit_file",
            "Edit a file: exact string replacement (path + old + new) OR hashline patch \
             application (patch only — `[path#TAG]` + `SWAP`/`DEL`/`INS.*` ops with \
             snapshot-tag checks and stale-tag recovery). Refuses to edit a file this session \
             hasn't read or that changed on disk since the last read — re-read with \
             read_file(path, fresh=true), rebase the change, then retry.",
            vec![
                param("path", "str", Some("None")),
                param("old", "str", Some("None")),
                param("new", "str", Some("None")),
                param("replace_all", "bool", Some("False")),
                param("patch", "str", Some("None")),
            ],
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File to edit (string-replacement mode)." },
                    "old": { "type": "string", "description": "Exact text to replace; must be unique unless replace_all." },
                    "new": { "type": "string", "description": "Replacement text." },
                    "replace_all": { "type": "boolean", "description": "Replace every occurrence of `old` (default false)." },
                    "patch": { "type": "string", "description": "Hashline patch to apply instead of old/new (carries its own paths and tags)." },
                },
                "required": [],
            }),
        ),
        spec(
            "grep",
            "Regex search across files (skips .git, node_modules, target, etc.). Returns \
             `[path#TAG]` + `LINE:TEXT` hashline rows by default; raw=true for plain \
             `path:line: text` output.",
            vec![
                param("pattern", "str", None),
                param("path", "str", Some("\".\"")),
                param("glob", "str", Some("None")),
                param("max_results", "int", Some("200")),
                param("raw", "bool", Some("False")),
            ],
            json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Regex to search for." },
                    "path": { "type": "string", "description": "Directory or file to search (default \".\")." },
                    "glob": { "type": "string", "description": "Filename glob filter, e.g. \"*.rs\"." },
                    "max_results": { "type": "integer", "description": "Max matching lines (default 200)." },
                    "raw": { "type": "boolean", "description": "Plain `path:line: text` output (default false)." },
                },
                "required": ["pattern"],
            }),
        ),
        spec(
            "find",
            "Find files by glob pattern on filename.",
            vec![
                param("pattern", "str", None),
                param("path", "str", Some("\".\"")),
                param("max_results", "int", Some("200")),
            ],
            json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Filename glob, e.g. \"*.lua\"." },
                    "path": { "type": "string", "description": "Directory to search (default \".\")." },
                    "max_results": { "type": "integer", "description": "Max results (default 200)." },
                },
                "required": ["pattern"],
            }),
        ),
        spec(
            "ls",
            "List a directory (dirs end with /).",
            vec![param("path", "str", Some("\".\""))],
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Directory to list (default \".\")." },
                },
                "required": [],
            }),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use phi_ext::RuntimeBuilder;

    fn runtime() -> phi_ext::AppRuntime {
        RuntimeBuilder::new()
            .register_extension(CodingToolsExtension)
            .build()
            .unwrap()
    }

    #[test]
    fn registers_all_coding_helpers_as_native_python_tools() {
        let rt = runtime();
        let names: Vec<&str> = rt.native_tool_specs().map(|t| t.name.as_str()).collect();
        for expected in [
            "bash",
            "read_file",
            "write_file",
            "edit_file",
            "grep",
            "find",
            "ls",
        ] {
            assert!(names.contains(&expected), "missing {expected}: {names:?}");
            assert!(
                matches!(
                    rt.native_tool_dispatch(expected),
                    Some(ToolDispatch::Python)
                ),
                "{expected} must dispatch into the REPL env"
            );
        }
        assert_eq!(names.len(), 7);
    }

    /// Python-dispatch tools must not generate REPL stubs — the prelude
    /// already defines the functions; a stub would shadow them.
    #[test]
    fn generates_no_prelude_stubs() {
        assert!(runtime().extension_prelude_stubs().is_empty());
    }

    /// The native names must stay tied to real, documented prelude functions
    /// — same drift guard as the coding_tools_section roster test.
    #[test]
    fn every_native_tool_is_documented_in_the_prelude_roster() {
        let roster = crate::prelude::coding_tools_section();
        for spec in coding_tool_specs() {
            assert!(
                roster.contains(&format!("- {}(", spec.name)),
                "native tool `{}` is not a documented prelude helper — prompt/prelude drift",
                spec.name
            );
        }
    }

    #[test]
    fn schemas_mark_required_fields() {
        let specs = coding_tool_specs();
        let schema = |name: &str| {
            specs
                .iter()
                .find(|s| s.name == name)
                .unwrap()
                .native_input_schema()
        };
        assert_eq!(schema("bash")["required"], serde_json::json!(["cmd"]));
        assert_eq!(
            schema("write_file")["required"],
            serde_json::json!(["path", "content"])
        );
        // Two modes (old/new vs patch): nothing is universally required.
        assert_eq!(schema("edit_file")["required"], serde_json::json!([]));
        assert!(
            schema("read_file")["properties"]["fresh"]["description"]
                .as_str()
                .is_some()
        );
    }
}
