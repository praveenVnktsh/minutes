//! Whether a note's numbers come from the transcript it summarizes. Pure.
//!
//! A small model mixes up figures between subjects ("Acme has 4,000 GPUs"
//! when 4,000 was said about another company a minute earlier). A note whose
//! numbers are not in the window it was written from (or the context lines
//! before it) is unsupported, and the parser drops it.
//!
//! Matching is generous on purpose: transcripts spell numbers out ("three and
//! a half", "seventy") and notes tidy them ("3.5", "~70", "$2M"), so every
//! numeral the transcript yields is collected, parts included, and a note's
//! number only needs to match one of them. Magnitude words in a note
//! ("thousands", "a million") need the same word, or a figure at least that
//! large, in the transcript.

use std::collections::HashSet;

const UNITS: &[(&str, f64)] = &[
    ("zero", 0.0),
    ("one", 1.0),
    ("two", 2.0),
    ("three", 3.0),
    ("four", 4.0),
    ("five", 5.0),
    ("six", 6.0),
    ("seven", 7.0),
    ("eight", 8.0),
    ("nine", 9.0),
    ("ten", 10.0),
    ("eleven", 11.0),
    ("twelve", 12.0),
    ("thirteen", 13.0),
    ("fourteen", 14.0),
    ("fifteen", 15.0),
    ("sixteen", 16.0),
    ("seventeen", 17.0),
    ("eighteen", 18.0),
    ("nineteen", 19.0),
    ("twenty", 20.0),
    ("thirty", 30.0),
    ("forty", 40.0),
    ("fifty", 50.0),
    ("sixty", 60.0),
    ("seventy", 70.0),
    ("eighty", 80.0),
    ("ninety", 90.0),
    ("half", 0.5),
    ("dozen", 12.0),
];

/// Magnitude words, singular; plurals ("thousands") are matched by stem.
const MAGNITUDES: &[(&str, f64)] = &[
    ("hundred", 100.0),
    ("thousand", 1e3),
    ("lakh", 1e5),
    ("million", 1e6),
    ("crore", 1e7),
    ("billion", 1e9),
    ("trillion", 1e12),
];

/// Hesitations transcription keeps inside spoken numbers.
const FILLERS: &[&str] = &["uh", "um", "er", "erm", "uhm"];

fn unit(word: &str) -> Option<f64> {
    UNITS.iter().find(|(w, _)| *w == word).map(|(_, v)| *v)
}

fn magnitude(word: &str) -> Option<(&'static str, f64)> {
    let singular = word.strip_suffix('s').unwrap_or(word);
    MAGNITUDES
        .iter()
        .find(|(w, _)| *w == singular)
        .map(|(w, v)| (*w, *v))
}

/// Lowercase words and figures, with an empty token after each clause end
/// (`,` `;` `.` `?` `!` `:`) so "a hundred; three" is not 103.
fn tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for piece in text.split(|c: char| c.is_whitespace() || c == '-' || c == '–' || c == '/') {
        let token = piece
            .trim_matches(|c: char| !c.is_alphanumeric() && c != '.')
            .trim_end_matches('.')
            .to_lowercase();
        if !token.is_empty() {
            out.push(token);
        }
        if piece.ends_with([',', ';', '.', '?', '!', ':']) {
            out.push(String::new());
        }
    }
    out
}

/// "1,500" → 1500, "~70" → 70, "$2m" → 2, "10th" → 10, "3.5x" → 3.5.
/// Tokens that are mostly letters ("m35", "h100", "q3") are not figures.
fn numeral(token: &str) -> Option<f64> {
    let digits_start = token.find(|c: char| c.is_ascii_digit())?;
    if digits_start > 0 {
        return None;
    }
    let end = token
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == ','))
        .unwrap_or(token.len());
    let suffix = &token[end..];
    if !matches!(
        suffix,
        "" | "k" | "m" | "b" | "bn" | "x" | "st" | "nd" | "rd" | "th" | "s"
    ) {
        return None;
    }
    token[..end].replace(',', "").parse().ok()
}

fn same(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-6 * a.abs().max(1.0)
}

/// What a transcript offers as support: every figure, parts included, and
/// the magnitude words it says.
#[derive(Debug, Default)]
pub struct Figures {
    values: Vec<f64>,
    magnitudes: HashSet<&'static str>,
}

impl Figures {
    pub fn of(text: &str) -> Self {
        let mut figures = Figures::default();
        let tokens = tokens(text);
        let mut run: Option<f64> = None;
        // "two point (uh) seven" → 2.7: the whole part, waiting for a digit.
        let mut decimal_of: Option<f64> = None;
        let mut index = 0;
        while index < tokens.len() {
            let token = tokens[index].as_str();
            if FILLERS.contains(&token) {
                index += 1;
                continue;
            }
            if let Some(whole) = decimal_of.take() {
                let digit = unit(token)
                    .or_else(|| numeral(token))
                    .filter(|d| *d < 10.0 && d.fract() == 0.0);
                if let Some(digit) = digit {
                    figures.values.push(whole + digit / 10.0);
                    run = Some(whole + digit / 10.0);
                    index += 1;
                    continue;
                }
            }
            if token == "point" {
                if let Some(whole) = run.filter(|w| w.fract() == 0.0) {
                    decimal_of = Some(whole);
                    index += 1;
                    continue;
                }
            }
            if let Some(value) = numeral(token) {
                figures.values.push(value);
                if token.ends_with('k') {
                    figures.values.push(value * 1e3);
                }
                run = Some(value);
            } else if let Some(value) = unit(token) {
                figures.values.push(value);
                // "twenty two" → 22; "three and a half" → 3.5.
                let combined = match run {
                    Some(previous)
                        if previous >= 20.0 && previous % 10.0 == 0.0 && value < 10.0 =>
                    {
                        Some(previous + value)
                    }
                    Some(previous) if token == "half" => Some(previous + 0.5),
                    _ => None,
                };
                if let Some(combined) = combined {
                    figures.values.push(combined);
                }
                run = Some(combined.unwrap_or(value));
            } else if let Some((word, scale)) = magnitude(token) {
                figures.magnitudes.insert(word);
                let base = run.unwrap_or(1.0);
                figures.values.push(base * scale);
                run = Some(base * scale);
            } else if !(matches!(token, "and" | "a") && run.is_some()) {
                run = None;
            }
            index += 1;
        }
        figures
    }

    fn has_value(&self, value: f64) -> bool {
        self.values.iter().any(|v| same(*v, value))
    }

    fn has_magnitude(&self, word: &str, scale: f64) -> bool {
        self.magnitudes.contains(word) || self.values.iter().any(|v| *v >= scale)
    }

    /// The numbers in `note` that this transcript does not support, as
    /// written in the note.
    pub fn unsupported(&self, note: &str) -> Vec<String> {
        let mut missing = Vec::new();
        for token in tokens(note) {
            if let Some(value) = numeral(&token) {
                // Small counts are tidied too freely ("a couple" → 2).
                if value > 2.0 && !self.has_value(value) {
                    missing.push(token);
                }
            } else if let Some(value) = unit(&token) {
                if value > 2.0 && token != "half" && !self.has_value(value) {
                    missing.push(token);
                }
            } else if let Some((word, scale)) = magnitude(&token) {
                if !self.has_magnitude(word, scale) {
                    missing.push(token);
                }
            }
        }
        missing
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spelled_and_tidied_numbers_match() {
        let said = Figures::of(
            "we were thirty or forty people, now almost a hundred; three and a half months; \
             about seventy two engineers and 1,500 customers, a 2M round",
        );
        for note in [
            "Team grew from 30 to 40 people, now ~100",
            "Joined 3.5 months ago",
            "72 engineers and 1500 customers",
            "Raised a $2M round",
        ] {
            assert_eq!(said.unsupported(note), Vec::<String>::new(), "{note}");
        }
        let decimals =
            Figures::of("around two point uh seven or two point eight crores, one point five now");
        for note in ["Net worth ~2.7 to 2.8 crores", "Holds 1.5 crore now"] {
            assert_eq!(decimals.unsupported(note), Vec::<String>::new(), "{note}");
        }
    }

    #[test]
    fn a_figure_from_elsewhere_is_unsupported() {
        let said = Figures::of("they have about seventy people and ten petabytes daily");
        assert_eq!(
            said.unsupported("70 people process 10 PB daily on 4,000 GPUs"),
            vec!["4,000"]
        );
        assert_eq!(
            said.unsupported("Tens of thousands of GPUs"),
            vec!["thousands"]
        );
        let bigger = Figures::of("tens of thousands of distributed GPUs");
        assert!(bigger.unsupported("Tens of thousands of GPUs").is_empty());
    }

    #[test]
    fn names_with_digits_and_small_counts_are_not_figures() {
        let said = Figures::of("nothing numeric here");
        assert!(said
            .unsupported("M35 runs on H100s; two founders; Q3 plan")
            .is_empty());
        assert_eq!(said.unsupported("Launch on the 10th"), vec!["10th"]);
    }
}
