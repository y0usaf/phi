//! Fuzzy matching (port of pi's fuzzy.ts): all query characters must appear
//! in order; lower score = better match.

/// Score `query` against `text`; `None` when it does not match. Rewards
/// consecutive runs and word-boundary hits, penalizes gaps and late matches.
pub fn fuzzy_match(query: &str, text: &str) -> Option<f64> {
    let query_lower = query.to_lowercase();
    let text_lower = text.to_lowercase();

    if let Some(score) = match_query(&query_lower, &text_lower) {
        return Some(score);
    }

    // Fallback: swap trailing digits with leading letters (or vice versa),
    // so "gpt5" still matches "5-gpt" style ids; slight penalty.
    let chars: Vec<char> = query_lower.chars().collect();
    let split = chars.iter().position(|c| c.is_ascii_digit());
    let swapped = match split {
        Some(i)
            if i > 0
                && chars[..i].iter().all(|c| c.is_ascii_lowercase())
                && chars[i..].iter().all(|c| c.is_ascii_digit()) =>
        {
            let (letters, digits) = chars.split_at(i);
            format!(
                "{}{}",
                digits.iter().collect::<String>(),
                letters.iter().collect::<String>()
            )
        }
        _ => {
            let i = chars.iter().position(|c| c.is_ascii_lowercase())?;
            if i == 0
                || !chars[..i].iter().all(|c| c.is_ascii_digit())
                || !chars[i..].iter().all(|c| c.is_ascii_lowercase())
            {
                return None;
            }
            let (digits, letters) = chars.split_at(i);
            format!(
                "{}{}",
                letters.iter().collect::<String>(),
                digits.iter().collect::<String>()
            )
        }
    };
    match_query(&swapped, &text_lower).map(|s| s + 5.0)
}

fn match_query(query: &str, text: &str) -> Option<f64> {
    if query.is_empty() {
        return Some(0.0);
    }
    let text_chars: Vec<char> = text.chars().collect();
    let query_chars: Vec<char> = query.chars().collect();
    if query_chars.len() > text_chars.len() {
        return None;
    }

    let mut query_idx = 0;
    let mut score = 0.0;
    let mut last_match: Option<usize> = None;
    let mut consecutive = 0u32;

    for (i, &c) in text_chars.iter().enumerate() {
        if query_idx == query_chars.len() {
            break;
        }
        if c != query_chars[query_idx] {
            continue;
        }
        let boundary =
            i == 0 || matches!(text_chars[i - 1], ' ' | '\t' | '-' | '_' | '.' | '/' | ':');
        if last_match == Some(i.wrapping_sub(1)) && i > 0 {
            consecutive += 1;
            score -= f64::from(consecutive) * 5.0;
        } else {
            consecutive = 0;
            if let Some(last) = last_match {
                score += (i - last - 1) as f64 * 2.0;
            }
        }
        if boundary {
            score -= 10.0;
        }
        score += i as f64 * 0.1;
        last_match = Some(i);
        query_idx += 1;
    }

    if query_idx < query_chars.len() {
        return None;
    }
    if query == text {
        score -= 100.0;
    }
    Some(score)
}

/// Filter and sort by match quality (best first). Space-separated query
/// tokens must all match.
pub fn fuzzy_filter<T>(items: Vec<T>, query: &str, text: impl Fn(&T) -> String) -> Vec<T> {
    let tokens: Vec<&str> = query.split_whitespace().collect();
    if tokens.is_empty() {
        return items;
    }
    let mut scored: Vec<(T, f64)> = items
        .into_iter()
        .filter_map(|item| {
            let t = text(&item);
            let mut total = 0.0;
            for token in &tokens {
                total += fuzzy_match(token, &t)?;
            }
            Some((item, total))
        })
        .collect();
    scored.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.into_iter().map(|(item, _)| item).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subsequence_matches_in_order() {
        assert!(fuzzy_match("mdl", "model").is_some());
        assert!(fuzzy_match("ldm", "model").is_none());
        assert!(fuzzy_match("", "anything").is_some());
    }

    #[test]
    fn exact_and_prefix_score_better() {
        let exact = fuzzy_match("model", "model").unwrap();
        let prefix = fuzzy_match("mod", "model").unwrap();
        let scattered = fuzzy_match("mol", "model").unwrap();
        assert!(exact < prefix);
        assert!(prefix < scattered);
    }

    #[test]
    fn filter_sorts_best_first() {
        let items = vec!["submodel", "model", "compact"];
        let out = fuzzy_filter(items, "mod", |s| (*s).to_owned());
        assert_eq!(out, vec!["model", "submodel"]);
    }

    #[test]
    fn alphanumeric_swap_fallback() {
        assert!(fuzzy_match("gpt5", "5.2-gpt").is_some());
    }

    #[test]
    fn multi_token_queries_require_all() {
        let items = vec!["anthropic claude-opus", "openai gpt"];
        let out = fuzzy_filter(items, "claude anth", |s| (*s).to_owned());
        assert_eq!(out, vec!["anthropic claude-opus"]);
    }
}
