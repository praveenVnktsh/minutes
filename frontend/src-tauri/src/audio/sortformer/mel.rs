//! 128-bin log-mel front end for the Nemotron-3 (streaming Sortformer) diarization graph.
//!
//! This reproduces NeMo's `AudioToMelSpectrogramPreprocessor` as configured for
//! the exported model, and must stay bit-for-bit faithful to that contract:
//!
//! * 16 kHz mono `f32` input, no dither;
//! * pre-emphasis `y[0] = x[0]`, `y[n] = x[n] - 0.97 * x[n-1]`;
//! * STFT with `n_fft = 512`, `hop = 160`, `win_length = 400`, symmetric Hann
//!   window (`periodic=False`) centred in the 512-sample frame (taps at 56..456),
//!   `center=True` with 256 zeros of padding on each side;
//! * power spectrum `|X|^2` (257 bins) projected through librosa's Slaney mel
//!   filterbank (`n_mels=128`, `fmin=0`, `fmax=8000`, `norm="slaney"`);
//! * `ln(mel + 2^-24)`, no normalisation;
//! * `floor(len / 160)` frames, laid out `(frames, 128)`.

use std::sync::{Arc, OnceLock};

use ndarray::Array2;
use rayon::prelude::*;
use realfft::{RealFftPlanner, RealToComplex};

/// Sample rate the model expects.
pub const SAMPLE_RATE: usize = 16_000;
/// Number of mel bins per frame.
pub const N_MELS: usize = 128;
/// STFT hop in samples (10 ms at 16 kHz); one output frame per hop.
pub const HOP_LENGTH: usize = 160;

const N_FFT: usize = 512;
const WIN_LENGTH: usize = 400;
const N_FREQS: usize = N_FFT / 2 + 1;
/// Offset of the first window tap inside the 512-sample frame (torch.stft centres
/// a short window inside `n_fft`).
const WIN_OFFSET: usize = (N_FFT - WIN_LENGTH) / 2;
/// Zero padding on each side from `center=True`.
const CENTER_PAD: usize = N_FFT / 2;
const PREEMPH: f32 = 0.97;
/// `2^-24`, the guard added before the log.
const LOG_GUARD: f32 = 5.960_464_5e-8;
const FMIN: f64 = 0.0;
const FMAX: f64 = SAMPLE_RATE as f64 / 2.0;
/// Frames handled per rayon task; each task owns one set of FFT buffers.
const FRAMES_PER_TASK: usize = 256;

/// Precomputed, immutable state shared by every call.
struct MelFrontend {
    fft: Arc<dyn RealToComplex<f32>>,
    /// Symmetric Hann window, `WIN_LENGTH` taps.
    window: Vec<f32>,
    /// Dense filterbank `(N_MELS, N_FREQS)`, as librosa would produce it.
    filters: Array2<f32>,
    /// Non-zero bin range `[start, end)` of each filter, so the projection only
    /// touches the handful of bins a triangle covers.
    filter_spans: Vec<(usize, usize)>,
}

fn frontend() -> &'static MelFrontend {
    static FRONTEND: OnceLock<MelFrontend> = OnceLock::new();
    FRONTEND.get_or_init(|| {
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(N_FFT);
        let window = (0..WIN_LENGTH)
            .map(|n| {
                let phase = 2.0 * std::f64::consts::PI * n as f64 / (WIN_LENGTH as f64 - 1.0);
                (0.5 - 0.5 * phase.cos()) as f32
            })
            .collect();
        let filters = slaney_mel_filterbank();
        let filter_spans = filters
            .outer_iter()
            .map(|row| {
                let start = row.iter().position(|&w| w != 0.0).unwrap_or(0);
                let end = row.iter().rposition(|&w| w != 0.0).map_or(start, |i| i + 1);
                (start, end)
            })
            .collect();
        MelFrontend {
            fft,
            window,
            filters,
            filter_spans,
        }
    })
}

/// Slaney mel scale (librosa `hz_to_mel(htk=False)`): linear below 1 kHz, log above.
fn hz_to_mel(hz: f64) -> f64 {
    const F_SP: f64 = 200.0 / 3.0;
    const MIN_LOG_HZ: f64 = 1000.0;
    const MIN_LOG_MEL: f64 = MIN_LOG_HZ / F_SP;
    let logstep = 6.4f64.ln() / 27.0;
    if hz >= MIN_LOG_HZ {
        MIN_LOG_MEL + (hz / MIN_LOG_HZ).ln() / logstep
    } else {
        hz / F_SP
    }
}

/// Inverse of [`hz_to_mel`] (librosa `mel_to_hz(htk=False)`).
fn mel_to_hz(mel: f64) -> f64 {
    const F_SP: f64 = 200.0 / 3.0;
    const MIN_LOG_HZ: f64 = 1000.0;
    const MIN_LOG_MEL: f64 = MIN_LOG_HZ / F_SP;
    let logstep = 6.4f64.ln() / 27.0;
    if mel >= MIN_LOG_MEL {
        MIN_LOG_HZ * (logstep * (mel - MIN_LOG_MEL)).exp()
    } else {
        F_SP * mel
    }
}

/// `N_MELS + 2` band edges in Hz, evenly spaced on the Slaney mel scale
/// (librosa `mel_frequencies`). Filter `i` rises from edge `i`, peaks at edge
/// `i + 1` and falls to zero at edge `i + 2`.
fn mel_band_edges() -> Vec<f64> {
    let (lo, hi) = (hz_to_mel(FMIN), hz_to_mel(FMAX));
    let n = N_MELS + 2;
    (0..n)
        .map(|i| mel_to_hz(lo + (hi - lo) * i as f64 / (n - 1) as f64))
        .collect()
}

/// librosa `filters.mel(sr=16000, n_fft=512, n_mels=128, fmin=0, fmax=8000,
/// norm="slaney", htk=False)`, computed in f64 and stored as f32.
fn slaney_mel_filterbank() -> Array2<f32> {
    let edges = mel_band_edges();
    let fft_freqs: Vec<f64> = (0..N_FREQS)
        .map(|k| FMAX * k as f64 / (N_FREQS - 1) as f64)
        .collect();

    let mut filters = Array2::<f32>::zeros((N_MELS, N_FREQS));
    for i in 0..N_MELS {
        let (f0, f1, f2) = (edges[i], edges[i + 1], edges[i + 2]);
        // Slaney normalisation: each triangle has unit area in Hz.
        let enorm = 2.0 / (f2 - f0);
        for (k, &f) in fft_freqs.iter().enumerate() {
            let lower = (f - f0) / (f1 - f0);
            let upper = (f2 - f) / (f2 - f1);
            let w = lower.min(upper).max(0.0);
            filters[[i, k]] = (w * enorm) as f32;
        }
    }
    filters
}

/// Pre-emphasised sample `i` of `x`, treating anything outside the clip as zero
/// (the `center=True` padding is applied after pre-emphasis).
#[inline]
fn preemphasised(x: &[f32], i: isize) -> f32 {
    if i < 0 || i as usize >= x.len() {
        return 0.0;
    }
    let i = i as usize;
    if i == 0 {
        x[0]
    } else {
        x[i] - PREEMPH * x[i - 1]
    }
}

/// Compute NeMo-compatible log-mel features for 16 kHz mono audio.
///
/// Returns an array of shape `(samples.len() / HOP_LENGTH, N_MELS)`; frame `t`
/// is centred on sample `t * HOP_LENGTH`. Fewer than `HOP_LENGTH` samples give
/// an empty `(0, N_MELS)` array.
pub fn log_mel_features(samples: &[f32]) -> Array2<f32> {
    let n_frames = samples.len() / HOP_LENGTH;
    let mut out = vec![0.0f32; n_frames * N_MELS];
    if n_frames == 0 {
        return Array2::from_shape_vec((0, N_MELS), out).expect("empty mel shape");
    }

    let fe = frontend();
    out.par_chunks_mut(FRAMES_PER_TASK * N_MELS)
        .enumerate()
        .for_each(|(task, chunk)| {
            // One set of buffers per task, reused for every frame in it.
            let mut frame = fe.fft.make_input_vec();
            let mut spectrum = fe.fft.make_output_vec();
            let mut scratch = fe.fft.make_scratch_vec();
            let mut power = [0.0f32; N_FREQS];

            for (local, mel_row) in chunk.chunks_exact_mut(N_MELS).enumerate() {
                let t = task * FRAMES_PER_TASK + local;
                // Sample index (in the unpadded clip) under window tap 0.
                let base = (t * HOP_LENGTH + WIN_OFFSET) as isize - CENTER_PAD as isize;

                frame.fill(0.0);
                let taps = &mut frame[WIN_OFFSET..WIN_OFFSET + WIN_LENGTH];
                if base >= 1 && (base as usize + WIN_LENGTH) <= samples.len() {
                    // Fast path: the whole window lies inside the clip and past sample 0.
                    let b = base as usize;
                    for (j, (tap, &w)) in taps.iter_mut().zip(&fe.window).enumerate() {
                        *tap = w * (samples[b + j] - PREEMPH * samples[b + j - 1]);
                    }
                } else {
                    for (j, (tap, &w)) in taps.iter_mut().zip(&fe.window).enumerate() {
                        *tap = w * preemphasised(samples, base + j as isize);
                    }
                }

                fe.fft
                    .process_with_scratch(&mut frame, &mut spectrum, &mut scratch)
                    .expect("FFT buffers are sized by the planner");

                for (p, c) in power.iter_mut().zip(spectrum.iter()) {
                    *p = c.norm_sqr();
                }

                for ((m, row), &(start, end)) in mel_row
                    .iter_mut()
                    .zip(fe.filters.outer_iter())
                    .zip(&fe.filter_spans)
                {
                    let weights = &row.as_slice().expect("filterbank is row-major")[start..end];
                    let energy: f32 = weights
                        .iter()
                        .zip(&power[start..end])
                        .map(|(w, p)| w * p)
                        .sum();
                    *m = (energy + LOG_GUARD).ln();
                }
            }
        });

    Array2::from_shape_vec((n_frames, N_MELS), out).expect("mel output shape")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn silence_value() -> f32 {
        LOG_GUARD.ln()
    }

    #[test]
    fn log_guard_is_two_to_minus_24() {
        assert_eq!(LOG_GUARD, 2.0f32.powi(-24));
    }

    #[test]
    fn filterbank_shape_and_sanity() {
        let fb = &frontend().filters;
        assert_eq!(fb.dim(), (N_MELS, N_FREQS));
        for (i, row) in fb.outer_iter().enumerate() {
            assert!(
                row.iter().all(|&w| w >= 0.0),
                "negative weight in filter {i}"
            );
            assert!(row.iter().any(|&w| w > 0.0), "filter {i} is empty");
        }
        let edges = mel_band_edges();
        assert!((edges[0] - FMIN).abs() < 1e-9);
        assert!((edges[N_MELS + 1] - FMAX).abs() < 1e-6);
        // The scale is linear below 1 kHz and must round-trip.
        for hz in [0.0, 250.0, 999.0, 1000.0, 1234.5, 8000.0] {
            assert!((mel_to_hz(hz_to_mel(hz)) - hz).abs() < 1e-9);
        }
    }

    #[test]
    #[ignore = "needs MEETILY_MEL_FILTERS pointing at a librosa float32 [128,257] dump"]
    fn filterbank_matches_librosa_dump() {
        let path = std::env::var("MEETILY_MEL_FILTERS")
            .expect("set MEETILY_MEL_FILTERS to the librosa mel_filters.bin dump");
        let bytes = std::fs::read(&path).expect("read mel filter dump");
        assert_eq!(bytes.len(), N_MELS * N_FREQS * 4, "unexpected dump size");
        let reference: Vec<f32> = bytes
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();
        let fb = &frontend().filters;
        let mut worst = 0.0f32;
        for (i, (&ours, &theirs)) in fb.iter().zip(&reference).enumerate() {
            let diff = (ours - theirs).abs();
            worst = worst.max(diff);
            assert!(
                diff <= 1e-6,
                "filter {} bin {}: ours {ours} librosa {theirs}",
                i / N_FREQS,
                i % N_FREQS
            );
        }
        eprintln!("max |ours - librosa| = {worst:e}");
    }

    #[test]
    fn frame_count_is_floor_len_over_hop() {
        for len in [0usize, 1, 159, 160, 161, 319, 320, 16_000, 16_159, 48_001] {
            let feats = log_mel_features(&vec![0.0; len]);
            assert_eq!(feats.dim(), (len / HOP_LENGTH, N_MELS), "len {len}");
        }
    }

    #[test]
    fn silence_is_log_guard_everywhere() {
        let feats = log_mel_features(&vec![0.0; SAMPLE_RATE]);
        let expected = silence_value();
        assert!((expected - (-24.0 * 2f32.ln())).abs() < 1e-6);
        assert!(feats.iter().all(|&v| v == expected));
    }

    #[test]
    fn sine_peaks_in_the_nearest_mel_bin() {
        let freq = 1000.0f64;
        let samples: Vec<f32> = (0..SAMPLE_RATE)
            .map(|n| {
                (0.5 * (2.0 * std::f64::consts::PI * freq * n as f64 / SAMPLE_RATE as f64).sin())
                    as f32
            })
            .collect();
        let feats = log_mel_features(&samples);

        // Filter i peaks at band edge i + 1.
        let edges = mel_band_edges();
        let expected = (0..N_MELS)
            .min_by(|&a, &b| {
                (edges[a + 1] - freq)
                    .abs()
                    .partial_cmp(&(edges[b + 1] - freq).abs())
                    .unwrap()
            })
            .unwrap();

        for t in [10, 50, 90] {
            let row = feats.row(t);
            let peak = (0..N_MELS)
                .max_by(|&a, &b| row[a].partial_cmp(&row[b]).unwrap())
                .unwrap();
            assert_eq!(peak, expected, "frame {t}");
        }
    }

    /// Reference single-frame computation straight from the contract, using an
    /// explicit DFT in f64, for a signal whose pre-emphasised form is sparse.
    fn reference_frame(padded_frame: &[(usize, f64)]) -> Vec<f64> {
        let fb = &frontend().filters;
        let power: Vec<f64> = (0..N_FREQS)
            .map(|k| {
                let (mut re, mut im) = (0.0f64, 0.0f64);
                for &(n, v) in padded_frame {
                    let ang = -2.0 * std::f64::consts::PI * (k * n) as f64 / N_FFT as f64;
                    re += v * ang.cos();
                    im += v * ang.sin();
                }
                re * re + im * im
            })
            .collect();
        (0..N_MELS)
            .map(|m| {
                let e: f64 = (0..N_FREQS).map(|k| fb[[m, k]] as f64 * power[k]).sum();
                (e + LOG_GUARD as f64).ln()
            })
            .collect()
    }

    fn hann(n: usize) -> f64 {
        0.5 - 0.5 * (2.0 * std::f64::consts::PI * n as f64 / (WIN_LENGTH as f64 - 1.0)).cos()
    }

    #[test]
    fn impulse_checks_preemphasis_padding_and_window_placement() {
        // An impulse at sample 200 pre-emphasises to +1 at 200 and -0.97 at 201.
        let mut samples = vec![0.0f32; 2 * HOP_LENGTH];
        samples[200] = 1.0;
        let feats = log_mel_features(&samples);
        assert_eq!(feats.nrows(), 2);

        // Frame 0 spans clip samples -256..256, but its window taps only cover
        // -200..200, so the impulse falls just outside: pure silence.
        assert!(feats.row(0).iter().all(|&v| v == silence_value()));

        // Frame 1 taps cover clip samples -40..360: sample 200 is tap 240 at
        // frame position 296, sample 201 is tap 241 at position 297.
        let expected = reference_frame(&[
            (WIN_OFFSET + 240, hann(240)),
            (WIN_OFFSET + 241, -0.97 * hann(241)),
        ]);
        for (m, (&got, &want)) in feats.row(1).iter().zip(&expected).enumerate() {
            assert!(
                (got as f64 - want).abs() < 1e-4,
                "bin {m}: got {got}, want {want}"
            );
        }
    }

    #[test]
    fn first_sample_is_not_differenced() {
        // y[0] = x[0]: a step starting at sample 0 has no -0.97 partner before it.
        let mut samples = vec![0.0f32; HOP_LENGTH];
        samples[0] = 1.0;
        let feats = log_mel_features(&samples);
        // Frame 0 taps cover clip samples -200..200; sample 0 is tap 200 at 256,
        // sample 1 (-0.97) is tap 201 at 257.
        let expected = reference_frame(&[
            (WIN_OFFSET + 200, hann(200)),
            (WIN_OFFSET + 201, -0.97 * hann(201)),
        ]);
        for (m, (&got, &want)) in feats.row(0).iter().zip(&expected).enumerate() {
            assert!(
                (got as f64 - want).abs() < 1e-4,
                "bin {m}: got {got}, want {want}"
            );
        }
    }

    #[test]
    fn interior_frame_matches_reference_dft() {
        // Interior frames take the unchecked fast path; check one against the
        // contract computed directly.
        let samples: Vec<f32> = (0..8_000)
            .map(|n| ((n as f32 * 0.37).sin() + (n as f32 * 0.011).cos()) * 0.3)
            .collect();
        let full = log_mel_features(&samples);
        // Frame 20 is fully interior in `samples`.
        let t = 20;
        let base = t * HOP_LENGTH + WIN_OFFSET - CENTER_PAD;
        let taps: Vec<(usize, f64)> = (0..WIN_LENGTH)
            .map(|j| {
                let i = base + j;
                let y = samples[i] - PREEMPH * samples[i - 1];
                (WIN_OFFSET + j, hann(j) * y as f64)
            })
            .collect();
        let expected = reference_frame(&taps);
        for (m, (&got, &want)) in full.row(t).iter().zip(&expected).enumerate() {
            assert!(
                (got as f64 - want).abs() < 1e-3,
                "bin {m}: got {got}, want {want}"
            );
        }
    }
}
