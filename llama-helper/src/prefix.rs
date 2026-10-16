//! Pure helpers for streaming and prompt-prefix reuse. Kept free of llama.cpp
//! types so they can be tested without a model.

use std::sync::atomic::{AtomicU64, Ordering};

/// Length of the longest common prefix of two token lists.
pub fn common_prefix_len<T: PartialEq>(a: &[T], b: &[T]) -> usize {
    a.iter().zip(b).take_while(|(x, y)| x == y).count()
}

/// How a cached request evaluates its prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrefixPlan {
    /// Restore the saved checkpoint before evaluating anything.
    pub restore: bool,
    /// First prompt position to evaluate (the checkpoint length when restoring).
    pub start: usize,
    /// End of the stable prefix. When it is past `start`, the prompt is
    /// evaluated up to here first and a new checkpoint is saved at this point.
    pub stable_end: usize,
}

impl PrefixPlan {
    pub fn saves_checkpoint(&self) -> bool {
        self.stable_end > self.start
    }
}

/// Plans a cached request.
///
/// * `full` - the whole prompt's tokens.
/// * `prefix` - the tokens of the stable prefix tokenized on its own. Only the
///   part that also starts `full` counts: a boundary token can merge with the
///   text after it.
/// * `checkpoint` - the tokens the saved state covers, if any.
///
/// At least one prompt token is always left to evaluate, so the last position
/// has logits to sample from.
pub fn plan_prefix<T: PartialEq>(full: &[T], prefix: &[T], checkpoint: Option<&[T]>) -> PrefixPlan {
    let last_evaluable = full.len().saturating_sub(1);
    let stable_end = common_prefix_len(full, prefix).min(last_evaluable);
    let restore_len = checkpoint
        .filter(|saved| !saved.is_empty() && saved.len() <= last_evaluable)
        .filter(|saved| common_prefix_len(full, saved) == saved.len())
        .map(<[T]>::len);
    match restore_len {
        Some(len) => PrefixPlan {
            restore: true,
            start: len,
            stable_end: stable_end.max(len),
        },
        None => PrefixPlan {
            restore: false,
            start: 0,
            stable_end,
        },
    }
}

/// The largest index of `output` that is safe to stream: nothing after it can
/// be the start of a stop token, so a stop token is never shown, even partly.
pub fn safe_emit_end(output: &str, stop_tokens: &[String]) -> usize {
    let mut end = output.len();
    for stop in stop_tokens {
        for (k, _) in stop.char_indices().skip(1) {
            // `stop[..k]` is a proper prefix of the stop token.
            if output.ends_with(&stop[..k]) {
                end = end.min(output.len() - k);
            }
        }
        if let Some(found) = output.find(stop.as_str()) {
            end = end.min(found);
        }
    }
    while end > 0 && !output.is_char_boundary(end) {
        end -= 1;
    }
    end
}

/// Cancellation shared between the stdin reader thread and generation. Ids
/// grow, so a stale cancel (for a request that already finished) can never
/// match a later one.
#[derive(Debug, Default)]
pub struct CancelFlag {
    highest_cancelled: AtomicU64,
}

impl CancelFlag {
    pub fn cancel(&self, id: u64) {
        self.highest_cancelled.fetch_max(id, Ordering::SeqCst);
    }

    /// Whether request `id` was cancelled. Requests without an id cannot be.
    pub fn is_cancelled(&self, id: Option<u64>) -> bool {
        id.is_some_and(|id| id != 0 && self.highest_cancelled.load(Ordering::SeqCst) == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cold_request_evaluates_the_stable_prefix_first() {
        let plan = plan_prefix(&[1, 2, 3, 4, 5], &[1, 2, 3], None);
        assert_eq!(
            plan,
            PrefixPlan {
                restore: false,
                start: 0,
                stable_end: 3
            }
        );
        assert!(plan.saves_checkpoint());
    }

    #[test]
    fn warm_request_restores_and_skips_the_prefix() {
        let plan = plan_prefix(&[1, 2, 3, 9, 9], &[1, 2, 3], Some(&[1, 2, 3]));
        assert_eq!(
            plan,
            PrefixPlan {
                restore: true,
                start: 3,
                stable_end: 3
            }
        );
        assert!(!plan.saves_checkpoint());
    }

    #[test]
    fn a_grown_prefix_extends_the_old_checkpoint() {
        let plan = plan_prefix(&[1, 2, 3, 4, 5, 6], &[1, 2, 3, 4, 5], Some(&[1, 2, 3]));
        assert_eq!((plan.restore, plan.start, plan.stable_end), (true, 3, 5));
        assert!(plan.saves_checkpoint());
    }

    #[test]
    fn a_changed_prefix_does_not_restore() {
        let plan = plan_prefix(&[1, 7, 3, 4], &[1, 7, 3], Some(&[1, 2, 3]));
        assert_eq!((plan.restore, plan.start, plan.stable_end), (false, 0, 3));
    }

    #[test]
    fn a_boundary_token_that_merges_is_not_part_of_the_prefix() {
        // The prefix tokenized alone ends in 30; in the full prompt that text
        // merged into 31.
        let plan = plan_prefix(&[1, 2, 31, 4], &[1, 2, 30], None);
        assert_eq!(plan.stable_end, 2);
    }

    #[test]
    fn at_least_one_token_is_left_to_evaluate() {
        let plan = plan_prefix(&[1, 2, 3], &[1, 2, 3], Some(&[1, 2, 3]));
        assert!(
            !plan.restore,
            "a checkpoint covering the whole prompt leaves no logits"
        );
        assert_eq!(plan.stable_end, 2);
        assert!(plan_prefix::<i32>(&[], &[], None).stable_end == 0);
    }

    #[test]
    fn streamed_text_never_shows_part_of_a_stop_token() {
        let stops = vec!["<|im_end|>".to_string()];
        assert_eq!(safe_emit_end("Hello <|im", &stops), "Hello ".len());
        assert_eq!(safe_emit_end("Hello <", &stops), "Hello ".len());
        assert_eq!(safe_emit_end("Hello", &stops), 5);
        assert_eq!(safe_emit_end("Hi<|im_end|>", &stops), 2);
        assert_eq!(safe_emit_end("a < b", &[]), 5);
    }

    #[test]
    fn safe_end_respects_char_boundaries() {
        let stops = vec!["é!".to_string()];
        let text = "café";
        let end = safe_emit_end(text, &stops);
        assert!(text.is_char_boundary(end));
        assert_eq!(&text[..end], "caf");
    }

    #[test]
    fn cancel_only_matches_the_cancelled_request() {
        let flag = CancelFlag::default();
        assert!(!flag.is_cancelled(Some(3)));
        flag.cancel(3);
        assert!(flag.is_cancelled(Some(3)));
        assert!(
            !flag.is_cancelled(Some(4)),
            "a stale cancel must not hit the next request"
        );
        assert!(!flag.is_cancelled(None));
        flag.cancel(2);
        assert!(flag.is_cancelled(Some(3)), "an older cancel never rewinds");
    }
}
