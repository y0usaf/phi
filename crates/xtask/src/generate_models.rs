mod common;
mod models_dev;

use std::path::Path;

use anyhow::{Context as _, Result};
use phi_ai::types::{Api, Modality, Model};
use phi_ext::{ProviderConfig, ProviderModelConfig, ProviderModelCost};

/// First-party providers whose models Phi embeds in `phi-ai` — the ones Phi
/// has native auth flows for. Every other provider that classifies (i.e.
/// exposes a plain API-key endpoint one of Phi's backends can speak) goes
/// into the bundled catalog, which `phi-builtins` registers through the same
/// `register_provider` API user extensions use (PLAN.md WS3.2).
const EMBEDDED_PROVIDERS: &[&str] = &["anthropic", "openai"];

pub fn run() -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let providers = models_dev::fetch_providers(&runtime)?;

    let mut models = Vec::new();
    let mut catalog: Vec<ProviderConfig> = Vec::new();
    let mut skipped: Vec<&str> = Vec::new();
    for provider in providers.values() {
        if EMBEDDED_PROVIDERS.contains(&provider.id.as_str()) {
            match models_dev::classify(provider) {
                Some((api, base_url)) => {
                    models.extend(models_dev::convert(provider, api, &base_url))
                }
                None => anyhow::bail!("embedded provider '{}' failed to classify", provider.id),
            }
            continue;
        }
        // Bundled catalog: providers that don't classify need auth or protocol
        // code Phi doesn't have (SigV4, vertex tokens, ...) — listing their
        // models would advertise entries that can't stream.
        let Some((api, base_url)) = models_dev::classify(provider) else {
            skipped.push(&provider.id);
            continue;
        };
        let converted = models_dev::convert(provider, api, &base_url);
        if converted.is_empty() {
            continue;
        }
        catalog.push(ProviderConfig {
            name: provider.id.clone(),
            base_url: Some(base_url),
            api_key: None,
            api: Some(api_name(api)?),
            models: converted.iter().map(to_provider_model).collect(),
        });
    }

    let out_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../phi-ai/src/models.json");
    // Cursor's native-API models aren't listed on models.dev; they're
    // hand-maintained in models.json and preserved across regenerations.
    match std::fs::read_to_string(&out_path) {
        Ok(existing) => {
            let existing: Vec<Model> = serde_json::from_str(&existing)?;
            models.extend(existing.into_iter().filter(|m| m.api == Api::CursorNative));
        }
        // First run: no models.json yet.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        // A read error (permissions, I/O) must not silently drop the
        // hand-maintained CursorNative entries — that's data loss.
        Err(e) => {
            return Err(anyhow::Error::new(e))
                .with_context(|| format!("reading existing {}", out_path.display()));
        }
    }
    models.sort_by(|a, b| (&a.provider, &a.id).cmp(&(&b.provider, &b.id)));

    let mut json = serde_json::to_string_pretty(&models)?;
    json.push('\n');
    write_atomic(&out_path, json.as_bytes())?;

    catalog.sort_by(|a, b| a.name.cmp(&b.name));
    let catalog_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../phi-builtins/src/catalog.json");
    // Compact JSON: this file is embedded in the binary and read by machines;
    // regeneration diffs are per-provider lines either way.
    let mut catalog_json = serde_json::to_string(&catalog)?;
    catalog_json.push('\n');
    write_atomic(&catalog_path, catalog_json.as_bytes())?;

    let mut per_provider: std::collections::BTreeMap<&str, usize> = Default::default();
    for m in &models {
        *per_provider.entry(m.provider.as_str()).or_default() += 1;
    }
    eprintln!("wrote {} models to {}", models.len(), out_path.display());
    for (provider, count) in &per_provider {
        eprintln!("  {provider}: {count}");
    }
    let catalog_models: usize = catalog.iter().map(|p| p.models.len()).sum();
    eprintln!(
        "wrote {} providers / {} models to {}",
        catalog.len(),
        catalog_models,
        catalog_path.display()
    );
    if !skipped.is_empty() {
        eprintln!(
            "skipped {} providers without a plain-key endpoint: {}",
            skipped.len(),
            skipped.join(", ")
        );
    }
    Ok(())
}

/// The `api` string a `ProviderConfig` carries — `Api`'s serde name.
fn api_name(api: Api) -> Result<String> {
    match serde_json::to_value(api)? {
        serde_json::Value::String(s) => Ok(s),
        other => anyhow::bail!("api serialized to non-string: {other}"),
    }
}

fn to_provider_model(m: &Model) -> ProviderModelConfig {
    ProviderModelConfig {
        id: m.id.clone(),
        name: m.name.clone(),
        reasoning: m.reasoning,
        context_window: m.context_window,
        max_tokens: m.max_tokens,
        cost: (m.cost.input != 0.0
            || m.cost.output != 0.0
            || m.cost.cache_read != 0.0
            || m.cost.cache_write != 0.0)
            .then_some(ProviderModelCost {
                input: m.cost.input,
                output: m.cost.output,
                cache_read: m.cost.cache_read,
                cache_write: m.cost.cache_write,
            }),
        thinking_levels: m.thinking_levels.as_ref().map(|levels| {
            levels
                .iter()
                .map(|(level, value)| (level.as_str().to_owned(), value.clone()))
                .collect()
        }),
        input: m
            .input
            .contains(&Modality::Image)
            .then(|| vec!["text".to_owned(), "image".to_owned()]),
        headers: m.headers.clone().into_iter().collect(),
        adaptive_thinking: m.adaptive_thinking,
    }
}

#[cfg(test)]
mod tests {
    use super::models_dev::override_for;

    #[test]
    fn override_for_unknown_returns_none() {
        assert!(override_for("anthropic", "claude-opus-4-7").is_none());
    }

    #[test]
    fn override_for_neuralwatt_glm() {
        assert_eq!(
            override_for("neuralwatt", "glm-5.2"),
            Some((200_000, 65_536))
        );
    }
}

/// Write `bytes` to `path` atomically: tmp file + rename, so a crash mid-write
/// can't leave a torn file (models.json and catalog.json are embedded in the
/// binary at the next build — corruption ships).
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path).with_context(|| format!("renaming {}", tmp.display()))
}
