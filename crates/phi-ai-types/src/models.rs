//! Port of `packages/ai/src/models.ts` — the pure model helpers.
//!
//! The registry half (`getModel` / `getProviders` / `getModels` over the
//! generated catalog) lands with WS2.4 (catalog as data); only the
//! catalog-independent functions live here.

use crate::types::{Model, ModelThinkingLevel, Usage, UsageCost};

/// Spec: `EXTENDED_THINKING_LEVELS`.
pub const EXTENDED_THINKING_LEVELS: [ModelThinkingLevel; 6] = [
    ModelThinkingLevel::Off,
    ModelThinkingLevel::Minimal,
    ModelThinkingLevel::Low,
    ModelThinkingLevel::Medium,
    ModelThinkingLevel::High,
    ModelThinkingLevel::XHigh,
];

/// Spec: `calculateCost` — writes `usage.cost` from the model's per-million
/// rates and returns it.
pub fn calculate_cost(model: &Model, usage: &mut Usage) -> UsageCost {
    usage.cost.input = (model.cost.input / 1_000_000.0) * usage.input as f64;
    usage.cost.output = (model.cost.output / 1_000_000.0) * usage.output as f64;
    usage.cost.cache_read = (model.cost.cache_read / 1_000_000.0) * usage.cache_read as f64;
    usage.cost.cache_write = (model.cost.cache_write / 1_000_000.0) * usage.cache_write as f64;
    usage.cost.total =
        usage.cost.input + usage.cost.output + usage.cost.cache_read + usage.cost.cache_write;
    usage.cost
}

/// Spec: `getSupportedThinkingLevels`.
///
/// A non-reasoning model supports only `off`. For a reasoning model, an
/// explicit `null` in `thinkingLevelMap` marks a level unsupported, a missing
/// key falls back to provider defaults — except `xhigh`, which is supported
/// only when explicitly mapped.
pub fn get_supported_thinking_levels(model: &Model) -> Vec<ModelThinkingLevel> {
    if !model.reasoning {
        return vec![ModelThinkingLevel::Off];
    }

    EXTENDED_THINKING_LEVELS
        .into_iter()
        .filter(|level| {
            let mapped = model
                .thinking_level_map
                .as_ref()
                .and_then(|map| map.get(level));
            match mapped {
                Some(None) => false,
                Some(Some(_)) => true,
                None => *level != ModelThinkingLevel::XHigh,
            }
        })
        .collect()
}

/// Spec: `clampThinkingLevel` — nearest supported level, searching upward
/// from the requested level first, then downward.
pub fn clamp_thinking_level(model: &Model, level: ModelThinkingLevel) -> ModelThinkingLevel {
    let available = get_supported_thinking_levels(model);
    if available.contains(&level) {
        return level;
    }

    let requested_index = EXTENDED_THINKING_LEVELS
        .iter()
        .position(|candidate| *candidate == level)
        .unwrap_or(0);

    for candidate in &EXTENDED_THINKING_LEVELS[requested_index..] {
        if available.contains(candidate) {
            return *candidate;
        }
    }
    for candidate in EXTENDED_THINKING_LEVELS[..requested_index].iter().rev() {
        if available.contains(candidate) {
            return *candidate;
        }
    }
    available
        .first()
        .copied()
        .unwrap_or(ModelThinkingLevel::Off)
}

/// Spec: `modelsAreEqual` — id + provider equality; `None` never matches.
pub fn models_are_equal(a: Option<&Model>, b: Option<&Model>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => a.id == b.id && a.provider == b.provider,
        _ => false,
    }
}
