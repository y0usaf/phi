use crate::store::{Edge, Memory};

pub(crate) fn format_memories(memories: &[Memory]) -> String {
    if memories.is_empty() {
        return "No memories found.".to_owned();
    }
    memories
        .iter()
        .map(|m| {
            let snippet: String = m.body.chars().take(300).collect();
            let dots = if m.body.len() > 300 { "..." } else { "" };
            format!(
                "[#{}] {} ({}) [s={:.2} a={:.2}]
{}{}",
                m.id, m.title, m.kind, m.strength, m.activation, snippet, dots
            )
        })
        .collect::<Vec<_>>()
        .join(
            "

",
        )
}

pub(crate) fn format_explored(memories: &[Memory]) -> String {
    if memories.is_empty() {
        return "No related memories discovered.".to_owned();
    }
    let mut out = "## Graph exploration results
"
    .to_owned();
    for m in memories {
        let snippet: String = m.body.chars().take(200).collect();
        out.push_str(&format!(
            "- **{}** (#{}, {}): {}
",
            m.title, m.id, m.kind, snippet
        ));
    }
    out
}

pub(crate) fn format_edges(edges: &[Edge]) -> String {
    if edges.is_empty() {
        return "No edges.".to_owned();
    }
    edges
        .iter()
        .map(|e| {
            format!(
                "#{} --[{} w={:.2}]--> #{}",
                e.source, e.relation, e.weight, e.target
            )
        })
        .collect::<Vec<_>>()
        .join(
            "
",
        )
}

/// Cheap keyword extraction: longest non-stopword tokens from the prompt.
/// This is what makes auto-inject work on real natural-language prompts.
pub(crate) fn extract_keywords(text: &str, limit: usize) -> Vec<String> {
    const STOPWORDS: &[&str] = &[
        "the", "a", "an", "and", "or", "but", "if", "then", "else", "for", "of", "to", "in", "on",
        "at", "by", "with", "from", "as", "is", "are", "was", "were", "be", "been", "being",
        "have", "has", "had", "do", "does", "did", "will", "would", "could", "should", "may",
        "might", "can", "this", "that", "these", "those", "i", "you", "he", "she", "it", "we",
        "they", "what", "which", "who", "whom", "whose", "when", "where", "why", "how", "about",
        "into", "over", "under", "again", "tell", "me", "show", "give", "get", "make", "use",
        "using", "used", "my", "your", "our",
    ];
    let mut scored: Vec<(usize, String)> = Vec::new();
    for word in text.split(|c: char| !c.is_alphanumeric()) {
        let w = word.to_lowercase();
        if w.len() < 3 || STOPWORDS.contains(&w.as_str()) {
            continue;
        }
        scored.push((w.len(), w));
    }
    scored.sort_by_key(|b| std::cmp::Reverse(b.0));
    scored.into_iter().take(limit).map(|(_, w)| w).collect()
}
