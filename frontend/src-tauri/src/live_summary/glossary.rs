//! A small running glossary of the names in a meeting's notes (people,
//! companies, products), so later notes spell them the same way. Pure.
//!
//! Live transcription often hears one name several ways. The glossary groups
//! spellings that are close to each other, keeps the better-established one
//! (seen more often; the most recent on a tie), shows it to the model, and
//! rewrites a new note's variant spelling to it.

use std::collections::HashMap;

/// Most terms shown to the model.
pub const MAX_TERMS: usize = 20;
/// A spelling counts as established once seen this many times; only then do
/// variants in new notes get rewritten to it.
const ESTABLISHED: usize = 2;

/// Capitalized words that start sentences or headlines, not names.
const COMMON: &[&str] = &[
    "A", "About", "After", "Agreed", "Also", "An", "And", "Are", "As", "At", "Be", "Before",
    "Both", "But", "By", "Can", "Data", "Decision", "Did", "Do", "Each", "Every", "For", "From",
    "Goal", "Has", "Have", "He", "Her", "His", "How", "I", "If", "In", "Is", "It", "Its", "Just",
    "Let", "Many", "More", "Most", "Need", "New", "Next", "No", "Not", "Now", "Of", "On", "One",
    "Only", "Or", "Our", "Plan", "She", "So", "Some", "Team", "That", "The", "Their", "There",
    "These", "They", "This", "Those", "To", "Two", "Use", "User", "Users", "We", "What", "When",
    "Where", "Which", "While", "Who", "Why", "Will", "With", "Yes", "You", "Your",
];

/// One spelling and how it was seen.
#[derive(Debug, Clone, PartialEq)]
struct Spelling {
    term: String,
    count: usize,
    last_seen: usize,
}

/// Names from earlier notes, with variants grouped.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Glossary {
    /// Each group's spellings; the first word of each is what is compared.
    groups: Vec<Vec<Spelling>>,
}

impl Glossary {
    /// The glossary of `texts`, oldest first.
    pub fn from_texts<'a>(texts: impl IntoIterator<Item = &'a str>) -> Self {
        let candidates: Vec<(Vec<String>, Option<String>)> =
            texts.into_iter().map(name_candidates).collect();
        // A capitalized first word ("Budget review moves…") counts only when
        // the same word is a name elsewhere in the notes.
        let known: Vec<String> = candidates
            .iter()
            .flat_map(|(names, _)| names.iter().map(|n| head(n).to_lowercase()))
            .collect();
        let mut counts: HashMap<String, (usize, usize)> = HashMap::new();
        for (index, (names, leading)) in candidates.into_iter().enumerate() {
            let leading = leading.filter(|word| known.contains(&word.to_lowercase()));
            for term in names.into_iter().chain(leading) {
                let entry = counts.entry(term).or_insert((0, index));
                entry.0 += 1;
                entry.1 = index;
            }
        }
        let mut spellings: Vec<Spelling> = counts
            .into_iter()
            .map(|(term, (count, last_seen))| Spelling {
                term,
                count,
                last_seen,
            })
            .collect();
        // Deterministic grouping: the most established spellings first.
        spellings.sort_by(|a, b| {
            b.count
                .cmp(&a.count)
                .then(b.last_seen.cmp(&a.last_seen))
                .then(a.term.cmp(&b.term))
        });
        let mut groups: Vec<Vec<Spelling>> = Vec::new();
        for spelling in spellings {
            match groups
                .iter_mut()
                .find(|group| is_variant(&group[0].term, &spelling.term))
            {
                Some(group) => group.push(spelling),
                None => groups.push(vec![spelling]),
            }
        }
        Self { groups }
    }

    /// The preferred spelling of each group, most recently seen first, at
    /// most `MAX_TERMS`.
    pub fn terms(&self) -> Vec<String> {
        let mut groups: Vec<(usize, String)> = self
            .groups
            .iter()
            .map(|group| {
                let last_seen = group.iter().map(|s| s.last_seen).max().unwrap_or(0);
                (last_seen, preferred(group).term.clone())
            })
            .collect();
        groups.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        groups
            .into_iter()
            .take(MAX_TERMS)
            .map(|(_, term)| term)
            .collect()
    }

    /// Names seen with more than one spelling, preferred spelling first.
    pub fn variant_groups(&self) -> Vec<Vec<String>> {
        self.groups
            .iter()
            .filter(|group| group.len() > 1)
            .map(|group| {
                let preferred = preferred(group).term.clone();
                let mut spellings = vec![preferred.clone()];
                spellings.extend(
                    group
                        .iter()
                        .map(|s| s.term.clone())
                        .filter(|t| *t != preferred),
                );
                spellings
            })
            .collect()
    }

    /// `text` with variant spellings of established names replaced by the
    /// established spelling ("MACO" becomes "Mako" once "Mako" was seen
    /// twice). Only name-like words are touched: acronyms anywhere, and
    /// capitalized words after the first.
    pub fn canonicalize(&self, text: &str) -> String {
        let established: Vec<&str> = self
            .groups
            .iter()
            .map(|group| preferred(group))
            .filter(|spelling| spelling.count >= ESTABLISHED)
            .map(|spelling| head(&spelling.term))
            .collect();
        if established.is_empty() {
            return text.to_string();
        }
        let mut out = String::with_capacity(text.len());
        let mut first = true;
        for (index, piece) in text.split(' ').enumerate() {
            if index > 0 {
                out.push(' ');
            }
            let word = piece.trim_matches(|c: char| !c.is_alphanumeric());
            let name_like = if first {
                is_acronym(word)
            } else {
                starts_upper(word)
            };
            if !word.is_empty() {
                first = false;
            }
            let replacement = name_like
                .then(|| {
                    established
                        .iter()
                        .find(|canonical| **canonical != word && is_variant(canonical, word))
                })
                .flatten();
            match replacement {
                Some(canonical) => out.push_str(&piece.replacen(word, canonical, 1)),
                None => out.push_str(piece),
            }
        }
        out
    }
}

/// The spelling a group shows: the most often seen, the most recent on a tie.
fn preferred(group: &[Spelling]) -> &Spelling {
    group
        .iter()
        .max_by(|a, b| a.count.cmp(&b.count).then(a.last_seen.cmp(&b.last_seen)))
        .expect("groups are never empty")
}

fn head(term: &str) -> &str {
    term.split_whitespace().next().unwrap_or(term)
}

fn starts_upper(word: &str) -> bool {
    word.chars().next().is_some_and(char::is_uppercase)
}

fn is_acronym(word: &str) -> bool {
    word.chars().filter(|c| c.is_uppercase()).count() >= 2
}

/// Whether two names are spellings of the same one: their first words are at
/// least four letters, start with the same letter and are at most two edits
/// apart (one when both are four letters).
pub fn is_variant(a: &str, b: &str) -> bool {
    let (a, b) = (head(a).to_lowercase(), head(b).to_lowercase());
    if a == b {
        return true;
    }
    let (la, lb) = (a.chars().count(), b.chars().count());
    if la.min(lb) < 4 || a.chars().next() != b.chars().next() {
        return false;
    }
    let allowed = if la.max(lb) <= 4 { 1 } else { 2 };
    edit_distance(&a, &b) <= allowed
}

fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = (above + 1)
                .min(row[j] + 1)
                .min(diagonal + usize::from(ca != *cb));
            diagonal = above;
        }
    }
    row[b.len()]
}

/// Name-like terms in a note: runs of capitalized words ("Acme Robotics"),
/// acronyms, and capitalized words not at the start.
pub fn names_in(text: &str) -> Vec<String> {
    name_candidates(text).0
}

/// The names in a note, and separately a capitalized single first word,
/// which may be a name ("Acme hires…") or just a headline's first word.
fn name_candidates(text: &str) -> (Vec<String>, Option<String>) {
    let words: Vec<&str> = text
        .split(|c: char| c.is_whitespace() || c == '-' || c == '/' || c == '(' || c == ')')
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric() && c != '&'))
        .collect();
    let mut names = Vec::new();
    let mut leading = None;
    let mut run: Vec<&str> = Vec::new();
    let mut run_starts_text = false;
    let mut seen_word = false;
    let mut flush = |run: &mut Vec<&str>, starts_text: bool| {
        if run.is_empty() {
            return;
        }
        if starts_text && run.len() == 1 && !is_acronym(run[0]) {
            leading = Some(run[0].to_string());
        } else if run.len() <= 4 {
            names.push(run.join(" "));
        }
        run.clear();
    };
    for word in words {
        if word.is_empty() {
            // Punctuation-only piece (an em dash): it ends a run.
            flush(&mut run, run_starts_text);
            continue;
        }
        let name_like = (starts_upper(word) || is_acronym(word))
            && !COMMON.contains(&word)
            && word.chars().any(char::is_alphabetic)
            && word.chars().count() >= 2;
        if name_like {
            if run.is_empty() {
                run_starts_text = !seen_word;
            }
            run.push(word);
        } else {
            flush(&mut run, run_starts_text);
        }
        seen_word = true;
    }
    flush(&mut run, run_starts_text);
    (names, leading)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_runs_of_capitalized_words_and_acronyms() {
        assert_eq!(
            names_in("Pilot with Acme Robotics starts in March, says the CFO"),
            ["Acme Robotics", "March", "CFO"]
        );
        // A capitalized first word alone is a sentence start, not a name...
        assert_eq!(names_in("Budget review moves to Friday"), ["Friday"]);
        // ...unless it is an acronym or part of a longer name.
        assert_eq!(names_in("ACME ships 10 TB a day"), ["ACME", "TB"]);
        assert_eq!(names_in("Acme Robotics hires 40 people"), ["Acme Robotics"]);
        assert_eq!(names_in("The team grew; Priya leads it"), ["Priya"]);
        assert!(names_in("plain lowercase note").is_empty());
    }

    #[test]
    fn variants_are_close_spellings_with_the_same_initial() {
        assert!(is_variant("Mako", "MACO"));
        assert!(is_variant("Kestrel AI", "Kestral"));
        assert!(is_variant("Priya", "Priyah"));
        assert!(!is_variant("Priya", "Bria"));
        assert!(!is_variant("Acme", "Axle"));
        assert!(!is_variant("AI", "AL"));
        assert!(!is_variant("Monday", "Sunday"));
    }

    #[test]
    fn the_glossary_prefers_the_established_spelling_and_lists_recent_first() {
        let texts = [
            "Partnership with Kestrel AI announced",
            "Kestrel AI has ~70 people",
            "Growth at Kestral: 10 TB a day",
            "Pricing model is owned by Priya",
        ];
        let glossary = Glossary::from_texts(texts);
        let terms = glossary.terms();
        assert_eq!(terms[0], "Priya");
        assert!(terms.contains(&"Kestrel AI".to_string()));
        assert!(!terms.contains(&"Kestral".to_string()));
        assert_eq!(glossary.variant_groups(), [["Kestrel AI", "Kestral"]]);
        // On a tie, the most recent spelling wins.
        let tie = Glossary::from_texts(["Talk with Kestral", "Hiring at Kestrel"]);
        assert_eq!(tie.terms(), ["Kestrel"]);
        // A capitalized first word counts once it is known as a name.
        let leading = Glossary::from_texts(["Budget is fine", "Priya joins", "Thanks to Priya"]);
        assert_eq!(leading.terms(), ["Priya"]);
    }

    #[test]
    fn the_glossary_is_capped() {
        // 26 names that are not variants of each other (different initials).
        let texts: Vec<String> = ('A'..='Z')
            .map(|initial| format!("Met with {initial}ondon about it"))
            .collect();
        let glossary = Glossary::from_texts(texts.iter().map(String::as_str));
        let terms = glossary.terms();
        assert_eq!(terms.len(), MAX_TERMS);
        assert_eq!(terms[0], "Zondon");
    }

    #[test]
    fn new_notes_use_the_established_spelling() {
        let glossary = Glossary::from_texts([
            "Series B for Mako announced",
            "Office opening for Mako in Austin",
        ]);
        assert_eq!(
            glossary.canonicalize("MACO processes ~10 TB a day"),
            "Mako processes ~10 TB a day"
        );
        assert_eq!(
            glossary.canonicalize("Growth at Maco, says the team."),
            "Growth at Mako, says the team."
        );
        // Ordinary words and unestablished names are left alone.
        assert_eq!(
            glossary.canonicalize("Make the deck by Monday"),
            "Make the deck by Monday"
        );
        let once = Glossary::from_texts(["Series B for Mako"]);
        assert_eq!(once.canonicalize("MACO grows"), "MACO grows");
    }
}
