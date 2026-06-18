use phi_ai::{Model, Usage};

pub(crate) fn format_tokens(count: u64) -> String {
    match count {
        0..=999 => count.to_string(),
        1_000..=9_999 => format!("{:.1}k", count as f64 / 1_000.0),
        10_000..=999_999 => format!("{}k", count / 1_000),
        _ => format!("{:.1}M", count as f64 / 1_000_000.0),
    }
}

pub(crate) fn format_rate(rate: f64) -> String {
    if rate == 0.0 {
        "$0/M".to_owned()
    } else if rate >= 10.0 {
        format!("${rate:.0}/M")
    } else if rate >= 1.0 {
        format!("${rate:.2}/M")
    } else {
        format!("${rate:.4}/M")
    }
}

pub(crate) fn format_model_pricing(model: &Model) -> String {
    let mut parts = vec![
        format!("in {}", format_rate(model.cost.input)),
        format!("out {}", format_rate(model.cost.output)),
    ];
    if model.cost.cache_write > 0.0 {
        parts.push(format!("cache+ {}", format_rate(model.cost.cache_write)));
    }
    if model.cost.cache_read > 0.0 {
        parts.push(format!("cache↺ {}", format_rate(model.cost.cache_read)));
    }
    parts.join(" · ")
}

pub(crate) fn format_usage_summary(usage: &Usage) -> String {
    format!(
        "usage: in {} · out {} · cache+ {} · cache↺ {} · total {} · ${:.4}",
        format_tokens(usage.input),
        format_tokens(usage.output),
        format_tokens(usage.cache_write),
        format_tokens(usage.cache_read),
        format_tokens(usage.total_tokens),
        usage.cost.total
    )
}
