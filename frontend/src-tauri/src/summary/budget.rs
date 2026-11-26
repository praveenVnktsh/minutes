//! Context-window budgeting for local models (built-in AI and Ollama).
//!
//! Every local call must fit `prompt + output budget` inside the context the
//! model actually runs with. Ollama's `num_ctx` comes from here, and
//! [`fit_builtin_output`] sizes one-off built-in calls (meeting chat, title)
//! with llama-helper's own token count.

use std::path::PathBuf;

use crate::summary::llm_client::LLMProvider;

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

/// Contexts below this get a larger output share.
pub const SMALL_CONTEXT: usize = 8_192;

/// Default output budget for one local summary call.
pub const DEFAULT_MAX_OUTPUT_TOKENS: usize = 4_096;

/// Smallest output budget worth asking for.
pub const MIN_OUTPUT_TOKENS: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextBudget {
    /// Context window the model runs with.
    pub n_ctx: usize,
    /// Output tokens reserved for the reply.
    pub max_output: usize,
}

impl ContextBudget {
    /// Budget for a context of `n_ctx` tokens: reserve at most a quarter of it
    /// for output, capped at `DEFAULT_MAX_OUTPUT_TOKENS`. A small context
    /// (an Ollama model whose metadata fell back to 4000 or 2048 tokens)
    /// reserves 3/8 instead: a quarter left 1000/512 output tokens, which a
    /// notes-length summary overruns, so summaries failed as truncated. The
    /// transcript is chunked smaller to make room.
    pub fn new(n_ctx: usize) -> Self {
        let n_ctx = n_ctx.max(MIN_NUM_CTX);
        let max_output = if n_ctx < SMALL_CONTEXT {
            n_ctx * 3 / 8
        } else {
            n_ctx / 4
        };
        Self {
            n_ctx,
            max_output: DEFAULT_MAX_OUTPUT_TOKENS.min(max_output),
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

    /// Output budget for a one-off call whose prompt is `prompt_tokens` long:
    /// `max_output`, shrunk to what is left after the prompt and the margin.
    /// None when not even `MIN_OUTPUT_TOKENS` fit.
    pub fn output_for_prompt(&self, prompt_tokens: usize) -> Option<usize> {
        let room = self
            .n_ctx
            .saturating_sub(prompt_tokens + self.safety_margin());
        let output = self.max_output.min(room);
        (output >= MIN_OUTPUT_TOKENS).then_some(output)
    }
}

/// Conservative token estimate for when no tokenizer is available (Ollama).
/// English BPE tokenizers average ~4 characters per token; one token per 3
/// ASCII characters overestimates on purpose. Non-ASCII text (CJK, Cyrillic,
/// accented scripts) is often a token per character or worse, so count each
/// one as a full token.
pub fn estimate_tokens(text: &str) -> usize {
    let (ascii, other) = text.chars().fold((0_usize, 0_usize), |(ascii, other), c| {
        if c.is_ascii() {
            (ascii + 1, other)
        } else {
            (ascii, other + 1)
        }
    });
    ascii.div_ceil(3) + other
}

/// True when a hosted provider's HTTP error body says the prompt did not fit
/// the context window. Only for raw provider bodies: the app's own errors are
/// typed (`llm_client::LlmErrorKind`), and its truncation message mentions
/// "context window" too.
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

/// Chat-template tokens added around a prompt, for estimates.
const CHAT_TEMPLATE_TOKENS: usize = 32;

/// Rough token count from the character count (0.35 tokens per character).
pub fn rough_token_count(s: &str) -> usize {
    let char_count = s.chars().count();
    (char_count as f64 * 0.35).ceil() as usize
}

/// Tokens in the full built-in prompt (chat template included), counted by
/// llama-helper; an estimate when this llama-helper cannot count.
async fn builtin_prompt_tokens(
    app_data_dir: &PathBuf,
    model_name: &str,
    system_prompt: &str,
    user_prompt: &str,
) -> Result<usize, String> {
    use crate::summary::summary_engine::{
        count_builtin_tokens, format_builtin_prompt, BuiltinError,
    };
    let formatted = format_builtin_prompt(model_name, system_prompt, user_prompt)
        .map_err(|error| error.to_string())?;
    match count_builtin_tokens(app_data_dir, model_name, &formatted, None).await {
        Ok(count) => Ok(count),
        // An older llama-helper without `tokenize`: budget on the estimate.
        Err(error)
            if matches!(
                error.downcast_ref::<BuiltinError>(),
                Some(BuiltinError::UnsupportedRequest(_))
            ) =>
        {
            log::warn!("llama-helper cannot count tokens ({error}); using an estimate");
            Ok(estimate_tokens(&formatted))
        }
        Err(error) => Err(format!("Could not count prompt tokens: {error}")),
    }
}

/// Output budget for a one-off built-in AI call (meeting chat, title): the
/// requested budget (or the model default), shrunk so prompt + output fit the
/// model's context. llama-helper refuses a request that does not fit up
/// front, so a long prompt asking for the full default would fail outright.
/// Other providers keep `requested`. When the prompt leaves no useful room,
/// `requested` is kept and llama-helper reports the overflow.
pub(crate) async fn fit_builtin_output(
    provider: &LLMProvider,
    model_name: &str,
    app_data_dir: Option<&PathBuf>,
    system_prompt: &str,
    user_prompt: &str,
    requested: Option<u32>,
) -> Option<u32> {
    builtin_output_room(
        provider,
        model_name,
        app_data_dir,
        system_prompt,
        user_prompt,
        requested,
    )
    .await
    .unwrap_or(requested)
}

/// [`fit_builtin_output`], but `Err(())` when the prompt leaves no useful
/// output room in the built-in model's context, so a caller can drop
/// optional context and try again.
pub(crate) async fn builtin_output_room(
    provider: &LLMProvider,
    model_name: &str,
    app_data_dir: Option<&PathBuf>,
    system_prompt: &str,
    user_prompt: &str,
    requested: Option<u32>,
) -> Result<Option<u32>, ()> {
    if *provider != LLMProvider::BuiltInAI {
        return Ok(requested);
    }
    let Some(model) = crate::summary::summary_engine::get_model_by_name(model_name) else {
        return Ok(requested);
    };
    let mut budget = ContextBudget::new(model.context_size as usize);
    if let Some(requested) = requested {
        budget.max_output = requested as usize;
    }
    let counted = match app_data_dir {
        Some(dir) => builtin_prompt_tokens(dir, model_name, system_prompt, user_prompt).await,
        None => Err("app_data_dir is required for BuiltInAI provider".to_string()),
    };
    let prompt_tokens = counted.unwrap_or_else(|error| {
        log::warn!("Could not count prompt tokens ({error}); using an estimate");
        estimate_tokens(system_prompt) + estimate_tokens(user_prompt) + CHAT_TEMPLATE_TOKENS
    });
    match budget.output_for_prompt(prompt_tokens) {
        Some(output) => Ok(Some(u32::try_from(output).unwrap_or(u32::MAX))),
        None => {
            log::warn!(
                "Prompt of {prompt_tokens} tokens leaves no output room in a {}-token context",
                budget.n_ctx
            );
            Err(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_contexts_reserve_more_room_for_output() {
        // Regression: a quarter of Ollama's 4000/2048-token fallback contexts
        // (1000/512 tokens) truncated ordinary summaries.
        let budget = ContextBudget::new(4_000);
        assert_eq!(budget.max_output, 1_500);
        assert_eq!(budget.safety_margin(), 256);
        assert_eq!(ContextBudget::new(100).n_ctx, 2_048);
        assert_eq!(ContextBudget::new(2_048).max_output, 768);
        // From 8k up, a quarter (capped) as before.
        assert_eq!(ContextBudget::new(8_192).max_output, 2_048);
    }

    #[test]
    fn ollama_budget_caps_num_ctx_for_memory() {
        assert_eq!(ContextBudget::for_ollama(131_072).n_ctx, OLLAMA_MAX_NUM_CTX);
        assert_eq!(ContextBudget::for_ollama(8_192).n_ctx, 8_192);
    }

    #[test]
    fn one_off_output_shrinks_to_the_room_left_after_the_prompt() {
        let budget = ContextBudget::new(32_768);
        assert_eq!(budget.output_for_prompt(10_000), Some(4_096));
        // A 30k-token chat prompt used to ask for 4096 and be refused.
        assert_eq!(
            budget.output_for_prompt(30_000),
            Some(32_768 - 30_000 - 1_024)
        );
        assert_eq!(budget.output_for_prompt(31_600), None);
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
}
