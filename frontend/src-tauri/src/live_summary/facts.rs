//! Whether a note's numbers come from the transcript it summarizes. Pure.
//!
//! A small model mixes up figures between subjects ("Acme has 4,000 GPUs"
//! when 4,000 was said about another company a minute earlier). The notes
//! rules (`notes.rs`) strip a figure the window's transcript does not
//! support, and drop a note that was little more than that figure.
//!
//! Matching is generous on purpose: transcripts spell numbers out ("a
//! hundred and fifty thousand", "twenty twenty seven", "two point uh seven")
//! and notes tidy them ("$150K", "2027", "~2.7"), so every value the
//! transcript yields is collected, parts included, and a note's figure only
//! needs to match one of them.
//!
//! - Scale suffixes count: "10K" is 10,000, "$5M" is 5,000,000.
//! - Spoken compounds, ordinals and years are read as numbers.
//! - Small counts (≤ 2) are not checked: notes tidy "a couple" freely.
//! - Not figures, never checked: clock times ("15:30"), versions ("v2.1"),
//!   and names with digits ("Q3", "H100", "M35").
//! - Magnitude words alone ("thousands of GPUs") need the same word, or a
//!   value at least that large, in the transcript.

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
];
const TEENS: &[(&str, f64)] = &[
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
];
const TENS: &[(&str, f64)] = &[
    ("twenty", 20.0),
    ("thirty", 30.0),
    ("forty", 40.0),
    ("fifty", 50.0),
    ("sixty", 60.0),
    ("seventy", 70.0),
    ("eighty", 80.0),
    ("ninety", 90.0),
];
const ORDINALS: &[(&str, f64)] = &[
    ("first", 1.0),
    ("second", 2.0),
    ("third", 3.0),
    ("fourth", 4.0),
    ("fifth", 5.0),
    ("sixth", 6.0),
    ("seventh", 7.0),
    ("eighth", 8.0),
    ("ninth", 9.0),
    ("tenth", 10.0),
    ("eleventh", 11.0),
    ("twelfth", 12.0),
    ("thirteenth", 13.0),
    ("fourteenth", 14.0),
    ("fifteenth", 15.0),
    ("sixteenth", 16.0),
    ("seventeenth", 17.0),
    ("eighteenth", 18.0),
    ("nineteenth", 19.0),
    ("twentieth", 20.0),
    ("thirtieth", 30.0),
];
/// Magnitude words, singular; plurals ("thousands") are matched by stem.
const MAGNITUDES: &[(&str, f64)] = &[
    ("thousand", 1e3),
    ("lakh", 1e5),
    ("million", 1e6),
    ("crore", 1e7),
    ("billion", 1e9),
    ("trillion", 1e12),
];
/// Hesitations transcription keeps inside spoken numbers.
const FILLERS: &[&str] = &["uh", "um", "er", "erm", "uhm"];

fn lookup(table: &[(&str, f64)], word: &str) -> Option<f64> {
    table.iter().find(|(w, _)| *w == word).map(|(_, v)| *v)
}

/// The value of a single number word (cardinal or ordinal below 100).
fn small(word: &str) -> Option<f64> {
    lookup(UNITS, word)
        .or_else(|| lookup(TEENS, word))
        .or_else(|| lookup(TENS, word))
        .or_else(|| lookup(ORDINALS, word))
}

fn magnitude(word: &str) -> Option<(&'static str, f64)> {
    let singular = word.strip_suffix('s').unwrap_or(word);
    let singular = singular
        .strip_suffix("th")
        .filter(|_| word.ends_with("th"))
        .unwrap_or(singular);
    MAGNITUDES
        .iter()
        .find(|(w, _)| *w == singular)
        .map(|(w, v)| (*w, *v))
}

fn is_hundred(word: &str) -> bool {
    matches!(word, "hundred" | "hundreds" | "hundredth")
}

/// One token of a text: the lowercase word or figure, and which
/// whitespace-separated piece of the text it came from.
#[derive(Debug, Clone)]
struct Token {
    text: String,
    piece: usize,
    /// A clause ends after this token (`,` `;` `.` `?` `!` `:`), so "a
    /// hundred; three" is not 103.
    ends_clause: bool,
}

fn tokens(text: &str) -> Vec<Token> {
    let mut out = Vec::new();
    for (piece, raw) in text.split_whitespace().enumerate() {
        let parts: Vec<&str> = raw.split(['-', '–', '—', '/']).collect();
        let last = parts.len() - 1;
        for (index, part) in parts.into_iter().enumerate() {
            let token = part
                .trim_matches(|c: char| !c.is_alphanumeric() && c != '.' && c != ':')
                .trim_end_matches(['.', ':'])
                .to_lowercase();
            let ends_clause = index == last && raw.ends_with([',', ';', '.', '?', '!', ':']);
            if token.is_empty() {
                if ends_clause {
                    if let Some(previous) = out.last_mut() {
                        let previous: &mut Token = previous;
                        previous.ends_clause = true;
                    }
                }
                continue;
            }
            out.push(Token {
                text: token,
                piece,
                ends_clause,
            });
        }
    }
    out
}

/// A written figure.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Numeral {
    /// Its value, scale applied ("10k" → 10,000), and the bare number
    /// ("10k" → 10), which a following magnitude word may scale.
    Value { scaled: f64, bare: f64 },
    /// A figure that is not checked: a clock time.
    Unchecked,
}

/// "1,500" → 1500, "~70" → 70, "$2m" → 2,000,000, "10th" → 10, "3pm" → 3,
/// "50%" → 50. Tokens that start with a letter ("v2.1", "q3", "h100") are
/// not figures; "15:30" is a figure that is not checked.
fn numeral(token: &str) -> Option<Numeral> {
    if !token.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    if token.contains(':') {
        return Some(Numeral::Unchecked);
    }
    let end = token
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == ','))
        .unwrap_or(token.len());
    let bare: f64 = token[..end].replace(',', "").parse().ok()?;
    let scale = match &token[end..] {
        "" | "x" | "s" | "st" | "nd" | "rd" | "th" | "am" | "pm" | "%" => 1.0,
        "k" => 1e3,
        "m" | "mm" => 1e6,
        "b" | "bn" => 1e9,
        _ => return None,
    };
    Some(Numeral::Value {
        scaled: bare * scale,
        bare,
    })
}

fn same(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-6 * a.abs().max(1.0)
}

/// A run of number words (and digits a magnitude word scales), read as one
/// number. Every reading is returned: the compound ("two thousand five
/// hundred" → 2500), a year when it reads as two pairs ("nineteen ninety
/// nine" → 1999, "twenty twenty seven" → 2027), and each word's own value.
fn read_run(words: &[&str]) -> Vec<f64> {
    let mut readings = Vec::new();
    // Each word on its own, for generous matching.
    for word in words {
        if let Some(value) = small(word) {
            readings.push(value);
        }
    }

    // "two point (uh) seven": the whole part and its first decimal digit.
    if let Some(point) = words.iter().position(|w| *w == "point") {
        let whole = read_compound(&words[..point]);
        let digit = words.get(point + 1).and_then(|w| {
            small(w)
                .or_else(|| match numeral(w) {
                    Some(Numeral::Value { bare, .. }) => Some(bare),
                    _ => None,
                })
                .filter(|d| *d < 10.0 && d.fract() == 0.0)
        });
        if let (Some(whole), Some(digit)) = (whole, digit) {
            let value = whole + digit / 10.0;
            readings.push(value);
            // "two point five million".
            let rest = &words[(point + 2).min(words.len())..];
            if let Some((_, scale)) = rest.iter().find_map(|w| magnitude(w)) {
                readings.push(value * scale);
            }
        }
        if let Some(whole) = whole {
            readings.push(whole);
        }
        return readings;
    }

    if let Some(year) = read_year(words) {
        readings.push(year);
    }
    if let Some(value) = read_compound(words) {
        readings.push(value);
        // "three and a half".
        if words.ends_with(&["and", "a", "half"]) || words.ends_with(&["and", "half"]) {
            readings.push(value + 0.5);
        }
    }
    readings
}

/// "a hundred and fifty thousand" → 150,000; "one hundred twenty" → 120;
/// "twenty fourth" → 24; "5 million" → 5,000,000.
fn read_compound(words: &[&str]) -> Option<f64> {
    let mut total = 0.0;
    let mut current = 0.0;
    let mut any = false;
    for (index, word) in words.iter().enumerate() {
        let next = words.get(index + 1).copied().unwrap_or("");
        if let Some(value) = small(word) {
            current += value;
            any = true;
        } else if let Some(Numeral::Value { bare, .. }) = numeral(word) {
            current += bare;
            any = true;
        } else if is_hundred(word) {
            current = if current == 0.0 { 1.0 } else { current } * 100.0;
            any = true;
        } else if let Some((_, scale)) = magnitude(word) {
            total += if current == 0.0 { 1.0 } else { current } * scale;
            current = 0.0;
            any = true;
        } else if *word == "a" && (is_hundred(next) || magnitude(next).is_some()) {
            current = current.max(0.0);
        }
    }
    any.then_some(total + current)
}

/// A pair of pairs, as years are said: "nineteen ninety nine", "twenty
/// twenty seven", "twenty oh five".
fn read_year(words: &[&str]) -> Option<f64> {
    let first = lookup(TEENS, words.first()?).or_else(|| lookup(TENS, words.first()?))?;
    let rest = &words[1..];
    let second = match rest {
        ["oh", unit] => lookup(UNITS, unit)?,
        [teen] if lookup(TEENS, teen).is_some() => lookup(TEENS, teen)?,
        [tens] => lookup(TENS, tens)?,
        [tens, unit] => {
            lookup(TENS, tens)?
                + lookup(UNITS, unit).or_else(|| lookup(ORDINALS, unit).filter(|v| *v < 10.0))?
        }
        _ => return None,
    };
    Some(first * 100.0 + second)
}

fn is_number_word(word: &str) -> bool {
    small(word).is_some() || is_hundred(word) || magnitude(word).is_some()
}

/// The number runs of `tokens`: (token indices, readings).
fn runs(tokens: &[Token]) -> Vec<(Vec<usize>, Vec<f64>)> {
    let mut out = Vec::new();
    let mut index = 0;
    while index < tokens.len() {
        let starts = is_number_word(&tokens[index].text)
            || (matches!(numeral(&tokens[index].text), Some(Numeral::Value { .. }))
                && tokens
                    .get(index + 1)
                    .is_some_and(|t| magnitude(&t.text).is_some() && !tokens[index].ends_clause))
            || (tokens[index].text == "a"
                && tokens
                    .get(index + 1)
                    .is_some_and(|t| is_hundred(&t.text) || magnitude(&t.text).is_some()));
        if !starts {
            index += 1;
            continue;
        }
        let mut members = vec![index];
        let mut end = index;
        while !tokens[end].ends_clause {
            let Some(next) = tokens.get(end + 1) else {
                break;
            };
            let word = next.text.as_str();
            let continues = is_number_word(word)
                || FILLERS.contains(&word)
                || word == "point"
                || matches!(numeral(word), Some(Numeral::Value { .. }))
                    && tokens[end].text == "point"
                || ((word == "and" || word == "a")
                    && tokens.get(end + 2).is_some_and(|t| {
                        is_number_word(&t.text) || t.text == "a" || t.text == "half"
                    }))
                || word == "half";
            if !continues {
                break;
            }
            end += 1;
            if !FILLERS.contains(&word) {
                members.push(end);
            }
        }
        let words: Vec<&str> = members.iter().map(|&i| tokens[i].text.as_str()).collect();
        out.push((members, read_run(&words)));
        index = end + 1;
    }
    out
}

/// What a transcript offers as support: every value, parts included, and
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
        for token in &tokens {
            if let Some(Numeral::Value { scaled, bare }) = numeral(&token.text) {
                figures.values.push(scaled);
                figures.values.push(bare);
            }
            if let Some((word, _)) = magnitude(&token.text) {
                figures.magnitudes.insert(word);
            }
            if is_hundred(&token.text) {
                figures.magnitudes.insert("hundred");
            }
        }
        for (_, readings) in runs(&tokens) {
            figures.values.extend(readings);
        }
        figures
    }

    /// The values a text states, for comparing two notes.
    pub fn values(&self) -> &[f64] {
        &self.values
    }

    fn has_value(&self, value: f64) -> bool {
        self.values.iter().any(|v| same(*v, value))
    }

    fn has_magnitude(&self, word: &str, scale: f64) -> bool {
        self.magnitudes.contains(word) || self.values.iter().any(|v| *v >= scale)
    }

    /// The figures of `note` this transcript does not support: the index of
    /// each whitespace-separated piece of the note that holds one, and the
    /// figure as read.
    pub fn unsupported_pieces(&self, note: &str) -> Vec<(usize, String)> {
        let tokens = tokens(note);
        let mut missing: Vec<(usize, String)> = Vec::new();
        let mut in_runs = HashSet::new();
        for (members, readings) in runs(&tokens) {
            in_runs.extend(members.iter().copied());
            let words: Vec<&str> = members.iter().map(|&i| tokens[i].text.as_str()).collect();
            // A magnitude word alone ("thousands of GPUs").
            if let [only] = words.as_slice() {
                if let Some((word, scale)) = magnitude(only) {
                    if !self.has_magnitude(word, scale) {
                        missing.push((tokens[members[0]].piece, only.to_string()));
                    }
                    continue;
                }
                if is_hundred(only) {
                    if !self.has_magnitude("hundred", 100.0) {
                        missing.push((tokens[members[0]].piece, only.to_string()));
                    }
                    continue;
                }
            }
            let checked: Vec<f64> = readings.iter().copied().filter(|v| *v > 2.0).collect();
            if checked.is_empty() || checked.iter().any(|v| self.has_value(*v)) {
                continue;
            }
            for &member in &members {
                missing.push((tokens[member].piece, words.join(" ")));
            }
        }
        for (index, token) in tokens.iter().enumerate() {
            if in_runs.contains(&index) {
                continue;
            }
            if let Some(Numeral::Value { scaled, .. }) = numeral(&token.text) {
                if scaled > 2.0 && !self.has_value(scaled) {
                    missing.push((token.piece, token.text.clone()));
                }
            }
        }
        missing.sort_by_key(|(piece, _)| *piece);
        missing.dedup_by(|a, b| a.0 == b.0);
        missing
    }

    /// The figures of `note` this transcript does not support, as read.
    pub fn unsupported(&self, note: &str) -> Vec<String> {
        let mut figures: Vec<String> = Vec::new();
        for (_, figure) in self.unsupported_pieces(note) {
            if !figures.contains(&figure) {
                figures.push(figure);
            }
        }
        figures
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (what was said, a note's figure that it supports).
    const SUPPORTED: &[(&str, &str)] = &[
        // Scale suffixes.
        ("we hit 10,000 users", "10K users"),
        ("about a hundred and fifty thousand dollars", "$150K budget"),
        ("five million in the round", "Raised $5M"),
        ("a 2M round", "Raised a $2M round"),
        ("two point five billion", "$2.5B valuation"),
        ("150 thousand a year", "$150K a year"),
        // Spoken compounds.
        ("two thousand five hundred seats", "2,500 seats"),
        ("one hundred twenty people", "120 people"),
        ("seventy two engineers", "72 engineers"),
        ("three and a half months", "3.5 months"),
        ("two point uh seven crores", "~2.7 crores"),
        (
            "thirty or forty people, now almost a hundred",
            "Grew from 30 to 40, now ~100",
        ),
        // Ordinals and years.
        ("on the twenty fourth", "Launch on May 24"),
        ("it moves to the third", "Moves to the 3rd"),
        ("in twenty twenty seven", "Targets 2027"),
        ("since nineteen ninety nine", "Founded in 1999"),
        ("by two thousand twenty four", "By 2024"),
        // Percentages, money, times, dates, ranges.
        ("fifty percent of the team", "50% of the team"),
        ("costs twelve dollars", "$12 per seat"),
        ("at three pm", "Call at 3pm"),
        ("may twenty fourth", "Due May 24th"),
        ("thirty to forty people", "30–40 people"),
        (
            "tens of thousands of distributed GPUs",
            "Tens of thousands of GPUs",
        ),
    ];

    /// (what was said, a note, the figures it does not support).
    const UNSUPPORTED: &[(&str, &str, &[&str])] = &[
        (
            "about seventy people",
            "70 people run 4,000 GPUs",
            &["4,000"],
        ),
        ("ten thousand users", "10M users", &["10m"]),
        ("five hundred", "$5M round", &["5m"]),
        ("two thousand five hundred", "2005 seats", &["2005"]),
        ("the third", "Launch on the 13th", &["13th"]),
        ("in twenty twenty six", "Targets 2027", &["2027"]),
        (
            "seventy people",
            "Tens of thousands of GPUs",
            &["thousands"],
        ),
        ("ten people", "Launch on May 24", &["24"]),
    ];

    #[test]
    fn spelled_scaled_and_tidied_figures_match() {
        for (said, note) in SUPPORTED {
            assert_eq!(
                Figures::of(said).unsupported(note),
                Vec::<String>::new(),
                "{said:?} / {note:?}"
            );
        }
    }

    #[test]
    fn a_figure_from_elsewhere_is_unsupported() {
        for (said, note, missing) in UNSUPPORTED {
            assert_eq!(
                Figures::of(said).unsupported(note),
                *missing,
                "{said:?} / {note:?}"
            );
        }
    }

    #[test]
    fn names_versions_times_and_small_counts_are_not_checked() {
        let said = Figures::of("nothing numeric here");
        for note in [
            "M35 runs on H100s; two founders; Q3 plan",
            "Ships in v2.1",
            "Standup moved to 15:30",
            "One or two people",
        ] {
            assert!(said.unsupported(note).is_empty(), "{note}");
        }
    }

    #[test]
    fn pieces_point_at_the_words_holding_the_figure() {
        let said = Figures::of("seventy people");
        assert_eq!(
            said.unsupported_pieces("70 people run 4,000 GPUs"),
            vec![(3, "4,000".to_string())]
        );
    }
}
