mod ai_gateway;
mod common;
mod models_dev;

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;

pub fn run() -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let providers = models_dev::fetch_providers(&runtime)?;

    let mut models = Vec::new();
    let mut skipped = Vec::new();
    for provider in providers.values() {
        match models_dev::classify(provider) {
            Some((api, base_url)) => models.extend(models_dev::convert(provider, api, &base_url)),
            None => skipped.push(provider.id.clone()),
        }
    }
    let ai_gateway_models = ai_gateway::fetch_ai_gateway_models(&runtime)?;
    let extra_provider_count = usize::from(!ai_gateway_models.is_empty());
    models.extend(ai_gateway_models);
    models.sort_by(|a, b| (&a.provider, &a.id).cmp(&(&b.provider, &b.id)));

    let out_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../phi-ai/src/models.json");
    let mut json = serde_json::to_string_pretty(&models)?;
    json.push('\n');
    std::fs::write(&out_path, json)?;

    let mut per_api: BTreeMap<String, usize> = BTreeMap::new();
    for m in &models {
        *per_api
            .entry(serde_json::to_string(&m.api)?.trim_matches('"').to_owned())
            .or_default() += 1;
    }
    eprintln!(
        "wrote {} models from {} providers to {}",
        models.len(),
        providers.len() - skipped.len() + extra_provider_count,
        out_path.display()
    );
    for (api, count) in &per_api {
        eprintln!("  {api}: {count}");
    }
    if !skipped.is_empty() {
        eprintln!("skipped providers (no static base URL or unsupported protocol):");
        eprintln!("  {}", skipped.join(", "));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::models_dev::override_for;

    #[test]
    fn override_for_known_bad_neuralwatt_glm_5_2() {
        // models.dev advertises 1048560 for both; vLLM actually serves 200k.
        let (ctx, max) = override_for("neuralwatt", "glm-5.2").expect("override present");
        assert_eq!(ctx, 200_000);
        assert_eq!(max, 65_536);
    }

    #[test]
    fn override_for_unknown_returns_none() {
        assert!(override_for("anthropic", "claude-opus-4-7").is_none());
    }
}
