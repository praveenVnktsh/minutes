//! Context-window budgeting for local summary models (built-in AI and Ollama).
//!
//! Every local call must fit `prompt + output budget` inside the context the
//! model actually runs with. These are the pure pieces of that arithmetic; the
//! processor supplies real token counts (llama-helper's tokenizer for built-in
//! AI, a conservative estimate for Ollama).

/// Sampling temperature sent to every provider that accepts one, unless the
/// user configured their own (custom OpenAI-compatible endpoints).
pub const SUMMARY_TEMPERATURE: f32 = 0.2;

/// Sampling seed sent to providers that accept one.
pub const SUMMARY_SEED: u32 = 42;

/// Largest context we ask Ollama to allocate. Models advertise up to 128k+,
/// and Ollama sizes the KV cache to `num_ctx`, so the advertised maximum can
/// exhaust memory on ordinary laptops.
pub const OLLAMA_MAX_NUM_CTX: usize = 16_384;

/// Smallest context we will budget against; below this a summary cannot fit.
const MIN_NUM_CTX: usize = 2_048;

/// Default output budget for one local summary call.
pub const DEFAULT_MAX_OUTPUT_TOKENS: usize = 4_096;

/// Chunks smaller than this are not worth re-splitting; a context overflow at
/// this size means the prompt overhead itself does not fit.
pub const MIN_CHUNK_TOKENS: usize = 256;

/// When a chunk overflows, retry it at this fraction of its size.
const SHRINK_FACTOR: f64 = 0.6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextBudget {
    /// Context window the model runs with.
    pub n_ctx: usize,
    /// Output tokens reserved for the reply.
    pub max_output: usize,
}

impl ContextBudget {
    /// Budget for a context of `n_ctx` tokens: reserve at most a quarter of it
    /// for output, capped at `DEFAULT_MAX_OUTPUT_TOKENS`.
    pub fn new(n_ctx: usize) -> Self {
        let n_ctx = n_ctx.max(MIN_NUM_CTX);
        Self {
            n_ctx,
            max_output: DEFAULT_MAX_OUTPUT_TOKENS.min(n_ctx / 4),
        }
    }

    /// Ollama budget: the model's advertised context, capped for memory. The
    /// same `n_ctx` must be sent as `options.num_ctx` on every request.
    pub fn for_ollama(model_context: usize) -> Self {
        Self::new(model_context.min(OLLAMA_MAX_NUM_CTX))
    }

    /// Tokens held back for tokenizer drift, chat-template tokens and estimate error.
    pub fn safety_margin(&self) -> usize {
        (self.n_ctx / 32).max(256)
    }

    /// Tokens left for variable input (transcript or summaries) once the
    /// output budget, the fixed prompt overhead and the margin are reserved.
    pub fn input_budget(&self, prompt_overhead_tokens: usize) -> usize {
        self.n_ctx.saturating_sub(
            self.max_output + prompt_overhead_tokens + self.safety_margin(),
        )
    }

    /// A larger output budget for retrying a truncated reply, or None when the
    /// context has no room for one. Doubles the budget, bounded by what is
    /// left after the prompt.
    pub fn grown_output(&self, current_output: usize, prompt_tokens: usize) -> Option<usize> {
        let room = self
            .n_ctx
            .saturating_sub(prompt_tokens + self.safety_margin());
        let grown = (current_output * 2).min(room);
        (grown > current_output).then_some(grown)
    }
}

/// Conservative token estimate for when no tokenizer is available (Ollama).
/// English BPE tokenizers average ~4 characters per token; one token per 3
/// ASCII characters overestimates on purpose. Non-ASCII text (CJK, Cyrillic,
/// accented scripts) is often a token per character or worse, so count each
/// one as a full token.
pub fn estimate_tokens(text: &str) -> usize {
    let (ascii, other) = text
        .chars()
        .fold((0_usize, 0_usize), |(ascii, other), c| {
            if c.is_ascii() {
                (ascii + 1, other)
            } else {
                (ascii, other + 1)
            }
        });
    ascii.div_ceil(3) + other
}

/// Characters per token measured on `text`, for turning token budgets into
/// chunk sizes. Falls back to a conservative 2.0 when the count is unusable.
pub fn chars_per_token(text: &str, tokens: usize) -> f64 {
    let chars = text.chars().count();
    if tokens == 0 || chars == 0 {
        return 2.0;
    }
    (chars as f64 / tokens as f64).clamp(0.5, 8.0)
}

/// Token budget for re-splitting a chunk that overflowed the context, or None
/// when it is already too small to shrink further.
pub fn shrink_chunk_tokens(current_tokens: usize) -> Option<usize> {
    let shrunk = (current_tokens as f64 * SHRINK_FACTOR).floor() as usize;
    (shrunk >= MIN_CHUNK_TOKENS && shrunk < current_tokens).then_some(shrunk)
}

/// True when a provider error says the prompt did not fit the context window.
pub fn is_context_overflow_error(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    [
        "context overflow",
        "context_overflow",
        "context length",
        "context_length_exceeded",
        "context window",
        "maximum context",
        "prompt is too long",
        "too many tokens",
        "exceeds the context",
        "input is too long",
    ]
    .iter()
    .any(|needle| error.contains(needle))
}

/// Groups consecutive items so each group's token total stays within
/// `budget_tokens`. An item larger than the budget gets a group of its own.
/// `separator_tokens` is charged between items of the same group.
pub fn group_within_budget(
    item_tokens: &[usize],
    budget_tokens: usize,
    separator_tokens: usize,
) -> Vec<std::ops::Range<usize>> {
    let mut groups = Vec::new();
    let mut start = 0;
    let mut used = 0;
    for (index, tokens) in item_tokens.iter().copied().enumerate() {
        let cost = if index == start {
            tokens
        } else {
            tokens + separator_tokens
        };
        if index > start && used + cost > budget_tokens {
            groups.push(start..index);
            start = index;
            used = tokens;
        } else {
            used += cost;
        }
    }
    if start < item_tokens.len() {
        groups.push(start..item_tokens.len());
    }
    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_budget_reserves_output_overhead_and_margin() {
        let budget = ContextBudget::new(32_768);
        assert_eq!(budget.max_output, 4_096);
        assert_eq!(budget.safety_margin(), 1_024);
        assert_eq!(budget.input_budget(1_500), 32_768 - 4_096 - 1_500 - 1_024);
    }

    #[test]
    fn input_budget_saturates_when_overhead_exceeds_context() {
        assert_eq!(ContextBudget::new(4_096).input_budget(10_000), 0);
    }

    #[test]
    fn small_contexts_reserve_a_quarter_for_output() {
        let budget = ContextBudget::new(4_096);
        assert_eq!(budget.max_output, 1_024);
        assert_eq!(budget.safety_margin(), 256);
        assert_eq!(ContextBudget::new(100).n_ctx, 2_048);
    }

    #[test]
    fn ollama_budget_caps_num_ctx_for_memory() {
        assert_eq!(ContextBudget::for_ollama(131_072).n_ctx, OLLAMA_MAX_NUM_CTX);
        assert_eq!(ContextBudget::for_ollama(8_192).n_ctx, 8_192);
    }

    #[test]
    fn grown_output_doubles_within_remaining_room() {
        let budget = ContextBudget::new(32_768);
        assert_eq!(budget.grown_output(4_096, 10_000), Some(8_192));
        // Only 32768 - 26000 - 1024 = 5744 tokens are left.
        assert_eq!(budget.grown_output(4_096, 26_000), Some(5_744));
        assert_eq!(budget.grown_output(4_096, 28_000), None);
    }

    #[test]
    fn estimate_overcounts_english_and_counts_non_ascii_per_char() {
        let english = "The quick brown fox jumps over the lazy dog.";
        assert_eq!(estimate_tokens(english), english.len().div_ceil(3));
        assert!(estimate_tokens(english) >= english.len() / 4);
        assert_eq!(estimate_tokens("会議の内容"), 5);
        assert_eq!(estimate_tokens(""), 0);
    }

    #[test]
    fn chars_per_token_is_measured_and_clamped() {
        assert_eq!(chars_per_token("abcdefgh", 2), 4.0);
        assert_eq!(chars_per_token("abc", 0), 2.0);
        assert_eq!(chars_per_token("a", 100), 0.5);
    }

    #[test]
    fn shrinking_reduces_until_minimum() {
        assert_eq!(shrink_chunk_tokens(10_000), Some(6_000));
        assert_eq!(shrink_chunk_tokens(430), Some(258));
        assert_eq!(shrink_chunk_tokens(420), None);
        assert_eq!(shrink_chunk_tokens(0), None);
    }

    #[test]
    fn overflow_errors_are_recognised_across_providers() {
        for error in [
            "Generation failed: context overflow: prompt has 9000 tokens",
            "LLM API request failed with status 400: {\"error\":{\"code\":\"context_length_exceeded\"}}",
            "This model's maximum context length is 8192 tokens",
            "prompt is too long: 210000 tokens > 200000 maximum",
        ] {
            assert!(is_context_overflow_error(error), "{error}");
        }
        for error in [
            "LLM request timed out after 300 seconds",
            "API key not found for openai",
            "Summary generation was cancelled",
        ] {
            assert!(!is_context_overflow_error(error), "{error}");
        }
    }

    #[test]
    fn grouping_respects_budget_and_separators() {
        assert_eq!(group_within_budget(&[40, 40, 40], 100, 5), vec![0..2, 2..3]);
        assert_eq!(group_within_budget(&[40, 40, 40], 130, 5), vec![0..3]);
        assert_eq!(group_within_budget(&[150, 10], 100, 0), vec![0..1, 1..2]);
        assert!(group_within_budget(&[], 100, 0).is_empty());
    }
}
