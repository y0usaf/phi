mod common;
mod models_dev;

use std::path::Path;

use anyhow::Result;
use phi_ai::types::{Api, Model};

/// First-party providers whose models Phi embeds — the ones Phi has native
/// auth flows for. Everything else arrives via `register_provider` or a
/// catalog extension (PLAN.md WS3.2).
const EMBEDDED_PROVIDERS: &[&str] = &["anthropic", "openai"];

pub fn run() -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let providers = models_dev::fetch_providers(&runtime)?;

    let mut models = Vec::new();
    for provider in providers.values() {
        if !EMBEDDED_PROVIDERS.contains(&provider.id.as_str()) {
            continue;
        }
        match models_dev::classify(provider) {
            Some((api, base_url)) => models.extend(models_dev::convert(provider, api, &base_url)),
            None => anyhow::bail!("embedded provider '{}' failed to classify", provider.id),
        }
    }

    let out_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../phi-ai/src/models.json");
    // Cursor's native-API models aren't listed on models.dev; they're
    // hand-maintained in models.json and preserved across regenerations.
    if let Ok(existing) = std::fs::read_to_string(&out_path) {
        let existing: Vec<Model> = serde_json::from_str(&existing)?;
        models.extend(existing.into_iter().filter(|m| m.api == Api::CursorNative));
    }
    models.sort_by(|a, b| (&a.provider, &a.id).cmp(&(&b.provider, &b.id)));

    let mut json = serde_json::to_string_pretty(&models)?;
    json.push('\n');
    std::fs::write(&out_path, json)?;

    let mut per_provider: std::collections::BTreeMap<&str, usize> = Default::default();
    for m in &models {
        *per_provider.entry(m.provider.as_str()).or_default() += 1;
    }
    eprintln!("wrote {} models to {}", models.len(), out_path.display());
    for (provider, count) in &per_provider {
        eprintln!("  {provider}: {count}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::models_dev::override_for;

    #[test]
    fn override_for_unknown_returns_none() {
        assert!(override_for("anthropic", "claude-opus-4-7").is_none());
    }
}
