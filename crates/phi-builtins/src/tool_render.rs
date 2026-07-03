//! Built-in tool display metadata (WS1.3a): declarative render specs for
//! the prelude coding helpers, registered through `register_tool_renderer`
//! like any user extension would. This is where the frontends' knowledge of
//! `bash`/`read_file`/… display shapes comes from — core crates hold only
//! the generic fallback rendering, so a builtins-less build degrades to
//! `name key=value` anchors (the same story as commands and overlays).

use anyhow::Result;
use phi_ext::{
    AnchorArg, AnchorStyle, Extension, ExtensionHost, ExtensionManifest, ResultLang, ToolBody,
    ToolRenderCategory, ToolRenderSpec,
};

pub struct ToolRenderExtension;

impl Extension for ToolRenderExtension {
    fn manifest(&self) -> ExtensionManifest {
        ExtensionManifest {
            id: "phi:tool-render".to_string(),
            name: "Phi Tool Rendering".to_string(),
            version: "0.1.0".to_string(),
            description: "Display metadata for the prelude coding tools".to_string(),
        }
    }

    fn register(&self, host: &mut dyn ExtensionHost) -> Result<()> {
        for spec in specs() {
            host.register_tool_renderer(spec)?;
        }
        Ok(())
    }
}

pub fn specs() -> Vec<ToolRenderSpec> {
    vec![
        ToolRenderSpec::new("bash")
            .with_verb("$")
            .with_display_name("Shell")
            .with_category(ToolRenderCategory::Commands)
            .with_body(ToolBody::Code {
                key: "cmd".to_owned(),
                lang: Some("bash".to_owned()),
                prompt: Some("$".to_owned()),
            })
            .with_exit_code_trailer(),
        ToolRenderSpec::new("read_file")
            .with_verb("read")
            .with_category(ToolRenderCategory::Reads)
            .with_anchor(AnchorArg::new("path", AnchorStyle::Path))
            .with_body(ToolBody::None)
            .with_result_lang(ResultLang::PathArg {
                key: "path".to_owned(),
            }),
        ToolRenderSpec::new("write_file")
            .with_verb("write")
            .with_category(ToolRenderCategory::Edits)
            .with_anchor(AnchorArg::new("path", AnchorStyle::Path))
            .with_body(ToolBody::None),
        ToolRenderSpec::new("edit_file")
            .with_verb("edit")
            .with_category(ToolRenderCategory::Edits)
            .with_anchor(AnchorArg::new("path", AnchorStyle::Path))
            .with_body(ToolBody::None),
        ToolRenderSpec::new("grep")
            .with_verb("grep")
            .with_category(ToolRenderCategory::Searches)
            .with_anchor(AnchorArg::new("pattern", AnchorStyle::Pattern))
            .with_anchor(
                AnchorArg::new("path", AnchorStyle::Path)
                    .with_prefix("in")
                    .skip("."),
            )
            .with_body(ToolBody::None),
        ToolRenderSpec::new("find")
            .with_verb("find")
            .with_category(ToolRenderCategory::Searches)
            .with_anchor(AnchorArg::new("pattern", AnchorStyle::Pattern))
            .with_anchor(
                AnchorArg::new("path", AnchorStyle::Path)
                    .with_prefix("in")
                    .skip("."),
            )
            .with_body(ToolBody::None),
        ToolRenderSpec::new("ls")
            .with_verb("ls")
            .with_anchor(AnchorArg::new("path", AnchorStyle::Path))
            .with_body(ToolBody::None),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every spec must describe a documented prelude helper — the same
    /// drift guard as the native coding tools roster test.
    #[test]
    fn specs_match_prelude_roster() {
        let roster = crate::prelude::coding_tools_section();
        for spec in specs() {
            assert!(
                roster.contains(&format!("- {}(", spec.tool)),
                "tool render spec `{}` is not a documented prelude helper — prelude drift",
                spec.tool
            );
        }
    }

    #[test]
    fn categories_cover_the_summary_vocabulary() {
        let specs = specs();
        let cat = |name: &str| {
            specs
                .iter()
                .find(|s| s.tool == name)
                .map(|s| s.category)
                .unwrap()
        };
        assert_eq!(cat("read_file"), ToolRenderCategory::Reads);
        assert_eq!(cat("edit_file"), ToolRenderCategory::Edits);
        assert_eq!(cat("write_file"), ToolRenderCategory::Edits);
        assert_eq!(cat("bash"), ToolRenderCategory::Commands);
        assert_eq!(cat("grep"), ToolRenderCategory::Searches);
        assert_eq!(cat("find"), ToolRenderCategory::Searches);
        assert_eq!(cat("ls"), ToolRenderCategory::Other);
    }
}
