//! Context-overflow detection, ported from phi's `utils/overflow.ts`.

use std::sync::LazyLock;

use regex_lite::Regex;

use crate::types::{AssistantMessage, StopReason};

static OVERFLOW_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"(?i)prompt is too long",                    // Anthropic token overflow
        r"(?i)request_too_large",                     // Anthropic byte-size overflow (HTTP 413)
        r"(?i)input is too long for requested model", // Amazon Bedrock
        r"(?i)exceeds the context window",            // OpenAI (Completions & Responses)
        r"(?i)exceeds (?:the )?(?:model'?s )?maximum context length of [\d,]+ tokens?", // LiteLLM-style proxies
        r"(?i)input token count.*exceeds the maximum", // Google (Gemini)
        r"(?i)maximum prompt length is \d+",           // xAI (Grok)
        r"(?i)reduce the length of the messages",      // Groq
        r"(?i)maximum context length is \d+ tokens",   // OpenRouter (most backends)
        r"(?i)exceeds (?:the )?maximum allowed input length of [\d,]+ tokens?", // OpenRouter/Poolside
        r"(?i)input \(\d+ tokens\) is longer than the model'?s context length \(\d+ tokens\)", // Together AI
        r"(?i)exceeds the limit of \d+",           // GitHub Copilot
        r"(?i)exceeds the available context size", // llama.cpp server
        r"(?i)greater than the context length",    // LM Studio
        r"(?i)context window exceeds limit",       // MiniMax
        r"(?i)exceeded model token limit",         // Kimi For Coding
        r"(?i)too large for model with \d+ maximum context length", // Mistral
        r"(?i)model_context_window_exceeded",      // z.ai non-standard finish_reason
        r"(?i)prompt too long; exceeded (?:max )?context length", // Ollama explicit overflow
        r"(?i)context[_ ]length[_ ]exceeded",      // Generic fallback
        r"(?i)too many tokens",                    // Generic fallback
        r"(?i)token limit exceeded",               // Generic fallback
        r"(?i)^4(?:00|13)\s*(?:status code)?\s*\(no body\)", // Cerebras: 400/413 with no body
    ]
    .iter()
    .map(|p| Regex::new(p).expect("overflow pattern must compile"))
    .collect()
});

static NON_OVERFLOW_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"(?i)^(Throttling error|Service unavailable):", // AWS Bedrock non-overflow errors
        r"(?i)rate limit",                               // Generic rate limiting
        r"(?i)too many requests",                        // Generic HTTP 429 style
    ]
    .iter()
    .map(|p| Regex::new(p).expect("non-overflow pattern must compile"))
    .collect()
});

/// Check whether an assistant message represents a context-overflow failure.
///
/// Covers explicit provider errors (pattern table), silent overflow
/// (`usage.input > context_window`) and length-stop truncation with zero
/// output.
pub fn is_context_overflow(message: &AssistantMessage, context_window: Option<u64>) -> bool {
    if message.stop_reason == StopReason::Error
        && let Some(error) = &message.error_message
    {
        let non_overflow = NON_OVERFLOW_PATTERNS.iter().any(|p| p.is_match(error));
        if !non_overflow && OVERFLOW_PATTERNS.iter().any(|p| p.is_match(error)) {
            return true;
        }
    }

    if let Some(window) = context_window {
        let input_tokens = message.usage.input + message.usage.cache_read;
        if message.stop_reason == StopReason::Stop && input_tokens > window {
            return true;
        }
        if message.stop_reason == StopReason::Length
            && message.usage.output == 0
            && input_tokens as f64 >= window as f64 * 0.99
        {
            return true;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Api, Usage};

    fn message(stop_reason: StopReason, error: Option<&str>) -> AssistantMessage {
        AssistantMessage {
            content: Vec::new(),
            api: Api::AnthropicMessages,
            provider: "anthropic".into(),
            model: "m".into(),
            usage: Usage::default(),
            stop_reason,
            error_message: error.map(str::to_owned),
            timestamp: 0,
        }
    }

    #[test]
    fn detects_provider_error_patterns() {
        for error in [
            "prompt is too long: 213462 tokens > 200000 maximum",
            "Your input exceeds the context window of this model",
            "The input token count (1196265) exceeds the maximum number of tokens allowed (1048575)",
            "400 status code (no body)",
        ] {
            assert!(
                is_context_overflow(&message(StopReason::Error, Some(error)), None),
                "should detect: {error}"
            );
        }
    }

    #[test]
    fn excludes_rate_limit_errors() {
        let msg = message(
            StopReason::Error,
            Some("Throttling error: Too many tokens, please wait before trying again."),
        );
        assert!(!is_context_overflow(&msg, None));
    }

    #[test]
    fn detects_silent_overflow() {
        let mut msg = message(StopReason::Stop, None);
        msg.usage.input = 250_000;
        assert!(is_context_overflow(&msg, Some(200_000)));
        assert!(!is_context_overflow(&msg, None));
    }

    #[test]
    fn detects_length_stop_truncation() {
        let mut msg = message(StopReason::Length, None);
        msg.usage.input = 199_000;
        msg.usage.output = 0;
        assert!(is_context_overflow(&msg, Some(200_000)));
        msg.usage.output = 10;
        assert!(!is_context_overflow(&msg, Some(200_000)));
    }
}
