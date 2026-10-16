//! Pure helpers for streaming and prompt-prefix reuse. Kept free of llama.cpp
//! types so they can be tested without a model.

use std::collections::VecDeque;
use std::sync::Mutex;

/// Length of the longest common prefix of two token lists.
pub fn common_prefix_len<T: PartialEq>(a: &[T], b: &[T]) -> usize {
    a.iter().zip(b).take_while(|(x, y)| x == y).count()
}

/// How a cached request evaluates its prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrefixPlan {
    /// Index of the checkpoint to restore before evaluating anything.
    pub restore: Option<usize>,
    /// First prompt position to evaluate (the checkpoint's length when restoring).
    pub start: usize,
    /// Positions (ascending, all past `start`) where evaluation pauses to save
    /// a new checkpoint: the ends of the stable boundaries.
    pub save_at: Vec<usize>,
}

/// Plans a cached request.
///
/// * `full` - the whole prompt's tokens.
/// * `boundaries` - token lists of the prompt's stable prefixes (an earlier
///   "anchor" and the full stable part), each tokenized on its own. Only the
///   part of each that also starts `full` counts: a boundary token can merge
///   with the text after it.
/// * `checkpoints` - the token lists the saved states cover.
///
/// The longest checkpoint that still starts the prompt is restored. At least
/// one prompt token is always left to evaluate, so the last position has
/// logits to sample from.
pub fn plan_prefix<T: PartialEq>(
    full: &[T],
    boundaries: &[&[T]],
    checkpoints: &[&[T]],
) -> PrefixPlan {
    let last_evaluable = full.len().saturating_sub(1);
    let restore = checkpoints
        .iter()
        .enumerate()
        .filter(|(_, saved)| !saved.is_empty() && saved.len() <= last_evaluable)
        .filter(|(_, saved)| common_prefix_len(full, saved) == saved.len())
        .max_by_key(|(_, saved)| saved.len())
        .map(|(index, saved)| (index, saved.len()));
    let start = restore.map_or(0, |(_, len)| len);
    let mut save_at: Vec<usize> = boundaries
        .iter()
        .map(|boundary| common_prefix_len(full, boundary).min(last_evaluable))
        .filter(|end| *end > start)
        .collect();
    save_at.sort_unstable();
    save_at.dedup();
    PrefixPlan {
        restore: restore.map(|(index, _)| index),
        start,
        save_at,
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

/// Cancellation shared between the stdin reader thread and generation: the
/// exact set of cancelled request ids. Ids are not cancelled in order (the
/// app's priority queue runs requests out of id order), so no "highest id"
/// shortcut. A cancel may arrive while its request still waits in the queue;
/// the id stays in the set until that request finishes. Cancels for requests
/// that already finished are bounded by `MAX_PENDING_CANCELS`.
#[derive(Debug, Default)]
pub struct CancelFlag {
    ids: Mutex<VecDeque<u64>>,
}

/// Most cancelled ids kept at once; older (stale) ones are forgotten.
pub const MAX_PENDING_CANCELS: usize = 256;

impl CancelFlag {
    pub fn cancel(&self, id: u64) {
        let mut ids = self.ids.lock().unwrap_or_else(|e| e.into_inner());
        if !ids.contains(&id) {
            ids.push_back(id);
            if ids.len() > MAX_PENDING_CANCELS {
                ids.pop_front();
            }
        }
    }

    /// Whether request `id` was cancelled. Requests without an id cannot be.
    pub fn is_cancelled(&self, id: Option<u64>) -> bool {
        id.is_some_and(|id| {
            self.ids
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains(&id)
        })
    }

    /// Request `id` finished: forget its cancel, if any.
    pub fn finished(&self, id: Option<u64>) {
        if let Some(id) = id {
            self.ids
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .retain(|cancelled| *cancelled != id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(full: &[i32], boundaries: &[&[i32]], checkpoints: &[&[i32]]) -> PrefixPlan {
        plan_prefix(full, boundaries, checkpoints)
    }

    fn expect(restore: Option<usize>, start: usize, save_at: &[usize]) -> PrefixPlan {
        PrefixPlan {
            restore,
            start,
            save_at: save_at.to_vec(),
        }
    }

    #[test]
    fn cold_request_evaluates_and_saves_each_boundary() {
        let p = plan(&[1, 2, 3, 4, 5], &[&[1, 2], &[1, 2, 3]], &[]);
        assert_eq!(p, expect(None, 0, &[2, 3]));
    }

    #[test]
    fn warm_request_restores_the_longest_matching_checkpoint() {
        let p = plan(
            &[1, 2, 3, 9, 9],
            &[&[1, 2], &[1, 2, 3]],
            &[&[1, 2], &[1, 2, 3]],
        );
        assert_eq!(p, expect(Some(1), 3, &[]));
    }

    #[test]
    fn a_grown_prefix_extends_the_old_checkpoint() {
        let p = plan(
            &[1, 2, 3, 4, 5, 6],
            &[&[1, 2], &[1, 2, 3, 4, 5]],
            &[&[1, 2, 3]],
        );
        assert_eq!(p, expect(Some(0), 3, &[5]));
    }

    #[test]
    fn a_moved_window_falls_back_to_the_anchor_checkpoint() {
        // The transcript part changed; the summary anchor did not.
        let p = plan(
            &[1, 2, 7, 8, 9],
            &[&[1, 2], &[1, 2, 7, 8]],
            &[&[1, 2], &[1, 2, 3, 4]],
        );
        assert_eq!(p, expect(Some(0), 2, &[4]));
    }

    #[test]
    fn a_changed_prefix_does_not_restore() {
        let p = plan(&[1, 7, 3, 4], &[&[1, 7, 3]], &[&[1, 2, 3]]);
        assert_eq!(p, expect(None, 0, &[3]));
    }

    #[test]
    fn a_boundary_token_that_merges_is_not_part_of_the_prefix() {
        // The prefix tokenized alone ends in 30; in the full prompt that text
        // merged into 31.
        let p = plan(&[1, 2, 31, 4], &[&[1, 2, 30]], &[]);
        assert_eq!(p.save_at, vec![2]);
    }

    #[test]
    fn at_least_one_token_is_left_to_evaluate() {
        let p = plan(&[1, 2, 3], &[&[1, 2, 3]], &[&[1, 2, 3]]);
        assert_eq!(
            p.restore, None,
            "a checkpoint covering the whole prompt leaves no logits"
        );
        assert_eq!(p.save_at, vec![2]);
        assert!(plan(&[], &[], &[]).save_at.is_empty());
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
        assert!(!flag.is_cancelled(Some(4)));
        assert!(!flag.is_cancelled(None));
    }

    #[test]
    fn cancels_out_of_id_order_all_take_effect() {
        // Regression: a "highest cancelled id" ignored cancel(10) after
        // cancel(11), so Copilot waited behind a full timeline generation.
        let flag = CancelFlag::default();
        flag.cancel(11);
        flag.cancel(10);
        assert!(flag.is_cancelled(Some(10)));
        assert!(flag.is_cancelled(Some(11)));
    }

    #[test]
    fn a_cancel_before_its_request_starts_still_applies_and_is_cleared_after() {
        let flag = CancelFlag::default();
        flag.cancel(7); // arrives while request 7 is still queued
        assert!(flag.is_cancelled(Some(7)));
        flag.finished(Some(7));
        assert!(!flag.is_cancelled(Some(7)));
    }

    #[test]
    fn stale_cancels_are_bounded() {
        let flag = CancelFlag::default();
        for id in 0..(MAX_PENDING_CANCELS as u64 + 10) {
            flag.cancel(id);
        }
        assert!(!flag.is_cancelled(Some(0)));
        assert!(flag.is_cancelled(Some(MAX_PENDING_CANCELS as u64 + 9)));
    }
}
