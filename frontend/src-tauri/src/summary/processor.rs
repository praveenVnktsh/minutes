use crate::summary::budget::{
    chars_per_token, estimate_tokens, group_within_budget, shrink_chunk_tokens, ContextBudget,
    MIN_CHUNK_TOKENS, SUMMARY_SEED,
};
use crate::summary::live_outline::{Outline, SECTION_MAX_OUTPUT_TOKENS};
use crate::summary::llm_client::{
    generate_completion, GenerationOptions, LLMProvider, LlmCompletion, LlmError, LlmErrorKind,
};
use once_cell::sync::Lazy;
use regex::Regex;
use reqwest::Client;
use std::collections::VecDeque;
use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

static THINK_ENVELOPE_REGEX: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?is)<think(?:ing)?(?:\s+[^>]*)?>.*?</think(?:ing)?\s*>").unwrap());
static THINK_MARKER_REGEX: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?is)</?think(?:ing)?(?:\s+[^>]*)?>").unwrap());

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanedLlmMarkdown {
    pub markdown: String,
    pub reasoning_stripped: bool,
}

pub fn clean_llm_markdown_detailed(raw: &str) -> CleanedLlmMarkdown {
    let visible = THINK_ENVELOPE_REGEX.replace_all(raw, "");
    let reasoning_stripped = visible.as_ref() != raw;
    let trimmed = visible.trim();
    const PREFIXES: &[&str] = &["```markdown\n", "```\n", "```markdown\r\n", "```\r\n"];
    const SUFFIX: &str = "```";
    let markdown = PREFIXES
        .iter()
        .find_map(|prefix| {
            (trimmed.starts_with(prefix) && trimmed.ends_with(SUFFIX))
                .then(|| trimmed[prefix.len()..trimmed.len() - SUFFIX.len()].trim())
        })
        .unwrap_or(trimmed)
        .to_string();

    if THINK_MARKER_REGEX.is_match(&markdown) {
        warn!(
            raw_len = raw.len(),
            sanitized_len = markdown.len(),
            "LLM output contains an unterminated reasoning marker"
        );
    }

    CleanedLlmMarkdown {
        markdown,
        reasoning_stripped,
    }
}

pub(crate) fn contains_reasoning_marker(markdown: &str) -> bool {
    THINK_MARKER_REGEX.is_match(markdown)
}

pub fn require_visible_markdown(stage: &str, cleaned: &CleanedLlmMarkdown) -> Result<(), String> {
    if contains_reasoning_marker(&cleaned.markdown) {
        Err(format!(
            "{stage} contained an unterminated reasoning marker"
        ))
    } else if cleaned.markdown.is_empty() {
        Err(format!(
            "{stage} returned no visible summary content after reasoning removal"
        ))
    } else {
        Ok(())
    }
}

pub(crate) const MAX_CHUNK_ATTEMPTS: usize = 2;

fn should_retry_chunk_failure(attempt: usize, cancelled: bool) -> bool {
    attempt < MAX_CHUNK_ATTEMPTS && !cancelled
}

/// Seed for the `attempt`th try (1-based). The first try keeps the fixed
/// seed, so a summary is reproducible; a retry of an identical prompt with the
/// same seed would just reproduce the failure, so later tries step the seed.
fn seed_for_attempt(base: Option<u32>, attempt: usize) -> Option<u32> {
    if attempt <= 1 {
        return base;
    }
    let offset = u32::try_from(attempt - 1).unwrap_or(u32::MAX);
    Some(base.unwrap_or(SUMMARY_SEED).wrapping_add(offset))
}

pub(crate) const ENGLISH_BASE_SUMMARY_INSTRUCTION: &str =
    "**Write the summary/report in English regardless of transcript language; non-English prose is invalid.**";

fn resolve_cached_english<'a>(
    cached: Option<&'a str>,
    summary_language: Option<&str>,
) -> Option<&'a str> {
    let cached_clean = cached.filter(|s| !s.trim().is_empty())?;
    let target_is_translation = summary_language
        .and_then(language_name_from_code)
        .is_some_and(|n| n != "English");
    if target_is_translation {
        Some(cached_clean)
    } else {
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FinalLanguageAction {
    ReturnEnglish,
    NormalizeEnglish,
    Translate(&'static str),
}

fn resolve_final_language_action(
    summary_language: Option<&str>,
    detected_transcript_language: Option<&str>,
) -> FinalLanguageAction {
    match summary_language.and_then(language_name_from_code) {
        Some(name) if name != "English" => FinalLanguageAction::Translate(name),
        // `detected_transcript_language` is only set when detection was
        // confident. Unknown or low-confidence detection must not trigger a
        // second full rewrite of the notes, so only a confidently detected
        // non-English transcript is normalised.
        _ => match detected_transcript_language.and_then(language_name_from_code) {
            Some(name) if name != "English" => FinalLanguageAction::NormalizeEnglish,
            _ => FinalLanguageAction::ReturnEnglish,
        },
    }
}

fn english_normalization_system_prompt() -> &'static str {
    r#"You are a precise English Markdown editor. Convert the provided Markdown document into English while preserving structure exactly.

**CRITICAL RULES:**
1. Translate any non-English prose into English.
2. Preserve the Markdown structure EXACTLY: keep every `#`, `**`, `-`, `|`, code fence marker, and table pipe in the same position.
3. Do NOT translate: proper nouns (names of people, products, companies), code identifiers, file paths, URLs, numeric values, or text inside backticks.
4. If the document is already English, lightly preserve it without rewriting meaning.
5. Do not add commentary or explanation. Output ONLY the English Markdown."#
}

fn english_markdown_after_normalization_result(
    original_markdown: &str,
    normalization_result: Result<CleanedLlmMarkdown, String>,
    cancelled: bool,
) -> Result<(CleanedLlmMarkdown, bool), String> {
    match normalization_result {
        Ok(cleaned) if !cleaned.markdown.is_empty() => Ok((cleaned, false)),
        Ok(cleaned) => {
            warn!("English normalization returned no visible content; returning pass-1 markdown");
            Ok((
                CleanedLlmMarkdown {
                    markdown: original_markdown.to_string(),
                    reasoning_stripped: cleaned.reasoning_stripped,
                },
                true,
            ))
        }
        Err(error) if cancelled => Err(error),
        Err(e) => {
            error!(
                "English normalization pass failed; returning pass-1 markdown without hard fail: {}",
                e
            );
            Ok((
                CleanedLlmMarkdown {
                    markdown: original_markdown.to_string(),
                    reasoning_stripped: false,
                },
                true,
            ))
        }
    }
}

/// Maps a BCP-47 tag to the English language name used inside LLM prompts.
///
/// LLMs respond far more reliably to "in Spanish" than to "in es". Regional
/// tags (`pt-BR`, `en_GB`) are normalised to their base language; Chinese
/// variants are disambiguated. Unknown codes return None so the caller falls
/// back to English rather than injecting a literal ISO code into the prompt.
pub(crate) fn language_name_from_code(code: &str) -> Option<&'static str> {
    let normalised = code.to_ascii_lowercase().replace('_', "-");
    let lookup: &str = match normalised.as_str() {
        "zh-cn" => "zh",
        "zh-tw" => return Some("Traditional Chinese"),
        other => other.split('-').next().unwrap_or(other),
    };
    match lookup {
        "en" => Some("English"),
        "zh" => Some("Chinese"),
        "de" => Some("German"),
        "es" => Some("Spanish"),
        "ru" => Some("Russian"),
        "ko" => Some("Korean"),
        "fr" => Some("French"),
        "ja" => Some("Japanese"),
        "pt" => Some("Portuguese"),
        "it" => Some("Italian"),
        "nl" => Some("Dutch"),
        "pl" => Some("Polish"),
        "ar" => Some("Arabic"),
        "hi" => Some("Hindi"),
        "ta" => Some("Tamil"),
        "tr" => Some("Turkish"),
        "vi" => Some("Vietnamese"),
        "th" => Some("Thai"),
        "id" => Some("Indonesian"),
        "sv" => Some("Swedish"),
        "cs" => Some("Czech"),
        "da" => Some("Danish"),
        "fi" => Some("Finnish"),
        "el" => Some("Greek"),
        "he" => Some("Hebrew"),
        "hu" => Some("Hungarian"),
        "no" => Some("Norwegian"),
        "ro" => Some("Romanian"),
        "uk" => Some("Ukrainian"),
        _ => None,
    }
}

fn translation_system_prompt(target_language: &str) -> String {
    format!(
        r#"You are a precise translator. Translate the provided Markdown document into {target_language} while preserving structure exactly.

**CRITICAL RULES:**
1. Translate every sentence, heading, list item, and table cell into {target_language}.
2. Preserve the Markdown structure EXACTLY: keep every `#`, `**`, `-`, `|`, code fence marker, and table pipe in the same position.
3. Do NOT translate: proper nouns (names of people, products, companies), code identifiers, file paths, URLs, numeric values, or text inside backticks.
4. Do not add commentary or explanation. Output ONLY the translated Markdown.
5. If a technical term has no standard translation, keep the original English word."#
    )
}

/// Explains the speaker labels `formatSummaryTranscript` (frontend) writes on
/// every transcript line.
pub(crate) const SPEAKER_GUIDE: &str = "Transcript lines look like `[mm:ss] [Speaker] text`. `[You]` is the person who recorded this meeting (the user). `[Others]` is everyone heard through the computer's audio: remote participants, possibly several different people, so never treat all `[Others]` lines as one person. Use a person's name only when the transcript states it.";

fn chunk_system_prompt() -> String {
    format!("You are an expert meeting summarizer.\n\n{SPEAKER_GUIDE}")
}

const COMBINE_SYSTEM_PROMPT: &str = "You are an expert at synthesizing meeting summaries.";

fn build_chunk_summary_user_prompt(chunk: &str) -> String {
    format!(
        "{ENGLISH_BASE_SUMMARY_INSTRUCTION}\n\nProvide a concise but comprehensive summary of the following transcript chunk. Capture all key points, decisions, action items with their owners and due dates, open questions, and mentioned individuals. Do not include reasoning, self-correction, or meta-commentary — output only the summary content.\n\n<transcript_chunk>\n{chunk}\n</transcript_chunk>"
    )
}

fn build_combine_summary_user_prompt(combined_text: &str) -> String {
    format!(
        "{ENGLISH_BASE_SUMMARY_INSTRUCTION}\n\nThe following are consecutive summaries of a meeting. Combine them into a single, coherent, and detailed narrative summary that retains all important details, organized logically. Do not include reasoning, self-correction, or meta-commentary — output only the summary content.\n\n<summaries>\n{combined_text}\n</summaries>"
    )
}

/// Final-pass prompt when the user typed notes: enrich their notes.
pub fn default_notes_system_prompt() -> String {
    format!(
        r#"You are Minutes, an expert meeting note-taker in the style of Granola.

The user typed their own notes during the meeting. Your job is to ENRICH those notes using the transcript — NOT to write a fresh summary of the transcript.

{SPEAKER_GUIDE}

**METHOD:**
1. The notes in `<my_notes>` are the outline. Turn each distinct note into one bullet, in the user's original order, and preserve the user's own wording and punctuation — including question marks and uncertainty (e.g. `datadog?`, `clippy mode?`).
2. Under each note, add 0-3 short sub-bullets (`-`) with concrete specifics from `<transcript>`: names, numbers, dates, decisions, owners, and the answer when the user wrote a question.
3. Never turn something the user wrote as a question or guess into a confident statement. If the transcript does not clearly resolve it, keep it as an open question or omit the sub-bullet.
4. Do NOT re-order, merge, or rewrite the user's notes into new themes.
5. Start with a single Markdown H1 title (max 6 words) naming the main topic this specific meeting actually discussed, drawn from `<transcript>` and `<my_notes>`. Never use a generic or placeholder title.
6. After the enriched notes, add a `## Also discussed` section with at most 4 short bullets for important transcript items the notes do not cover. Omit the section entirely if there is nothing important.
7. If `<my_notes>` is missing or empty, write concise bullet notes from the transcript alone: the title, then the decisions, action items with owners, and open questions it actually contains.
8. {ENGLISH_BASE_SUMMARY_INSTRUCTION}
9. Output ONLY the Markdown notes — no reasoning, thinking, self-correction, or meta-commentary."#
    )
}

/// The example's title. Small models copy example titles verbatim, so
/// `is_prompt_echo_title` refuses it as a meeting name.
const TRANSCRIPT_ONLY_EXAMPLE_TITLE: &str = "Office Move Logistics";

/// Final-pass prompt when the user typed no notes (the common case): fixed
/// sections so every meeting's notes have the same shape.
pub fn default_transcript_only_system_prompt() -> String {
    format!(
        r#"You are Minutes, an expert meeting note-taker. Write meeting notes from the transcript in `<transcript>`.

{SPEAKER_GUIDE}

**FORMAT** — use these sections in this order, and leave out any section that would be empty (never write a section that only says "None"):
1. `# Title` — one H1 of at most 6 words naming what this specific meeting discussed. Never generic.
2. `## Summary` — 2-3 sentences on the purpose and outcome of the meeting.
3. `## Decisions` — one bullet per decision that was actually made.
4. `## Action items` — one bullet per task: `- **Owner** — task (due: date)`. Write the due date only if one was stated, and use **Unassigned** when nobody took the task.
5. `## Open questions` — one bullet per question left unresolved.

**RULES:**
- Use only what the transcript says. Never invent names, dates, numbers or decisions.
- Keep bullets short and concrete.
- {ENGLISH_BASE_SUMMARY_INSTRUCTION}
- Output ONLY the Markdown notes — no reasoning, thinking, self-correction, or meta-commentary.

**EXAMPLE** (shows the format only; never copy its content or title):
# {TRANSCRIPT_ONLY_EXAMPLE_TITLE}

## Summary
The team agreed to move to the new office on 3 June. Movers are booked, but desk assignments are still undecided.

## Decisions
- Move day is 3 June.

## Action items
- **You** — send the floor plan to everyone (due: Friday)
- **Unassigned** — book the internet installation

## Open questions
- Who gets the desks by the window?"#
    )
}

/// Where the final-pass system prompt came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SummaryPromptKind {
    /// The built-in fixed-section prompt for meetings without notes.
    DefaultTranscriptOnly,
    /// The built-in enrich-my-notes prompt.
    DefaultNotes,
    /// A prompt the user saved in Settings.
    Custom,
}

/// The final-pass system prompt and its origin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SummaryPrompt {
    pub text: String,
    pub kind: SummaryPromptKind,
}

impl SummaryPrompt {
    /// Placeholder sections ("## Decisions\n- None") are tidied away only for
    /// the fixed-section default prompt, which asks for those sections. A
    /// user's notes, or a custom prompt's output, may legitimately say "None".
    fn drops_placeholder_sections(&self) -> bool {
        self.kind == SummaryPromptKind::DefaultTranscriptOnly
    }
}

fn custom_prompt(override_prompt: Option<&str>) -> Option<SummaryPrompt> {
    override_prompt
        .map(str::trim)
        .filter(|trimmed| !trimmed.is_empty())
        .map(|trimmed| SummaryPrompt {
            text: trimmed.to_string(),
            kind: SummaryPromptKind::Custom,
        })
}

/// The final-pass system prompt. With notes: the user's saved notes prompt,
/// else the enrich-my-notes default. Without notes: the user's saved
/// no-notes prompt, else the fixed-section default. A custom notes prompt is
/// written around `<my_notes>`, so it never applies to a meeting without any.
pub(crate) fn resolve_summary_system_prompt(
    notes_override: Option<&str>,
    transcript_only_override: Option<&str>,
    notes: &str,
) -> SummaryPrompt {
    if notes.trim().is_empty() {
        custom_prompt(transcript_only_override).unwrap_or_else(|| SummaryPrompt {
            text: default_transcript_only_system_prompt(),
            kind: SummaryPromptKind::DefaultTranscriptOnly,
        })
    } else {
        custom_prompt(notes_override).unwrap_or_else(|| SummaryPrompt {
            text: default_notes_system_prompt(),
            kind: SummaryPromptKind::DefaultNotes,
        })
    }
}

static PLACEHOLDER_LINE_REGEX: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?i)^(?:[-*+]\s+)?[_*]*(?:none|n/a|nothing)(?:\s+(?:noted|identified|mentioned|stated|discussed|raised))?[_*]*\.?[_*]*$",
    )
    .unwrap()
});

/// Removes `## ` sections whose only content is a placeholder such as
/// "- None". The prompt asks the model to omit empty sections, but small
/// models still write them.
pub(crate) fn drop_placeholder_sections(markdown: &str) -> String {
    let mut sections: Vec<Vec<&str>> = vec![Vec::new()];
    for line in markdown.lines() {
        if line.starts_with("## ") {
            sections.push(Vec::new());
        }
        sections
            .last_mut()
            .expect("sections is never empty")
            .push(line);
    }
    let is_placeholder_section = |section: &Vec<&str>| {
        section.first().is_some_and(|line| line.starts_with("## ")) && {
            let mut body = section[1..]
                .iter()
                .map(|line| line.trim())
                .filter(|line| !line.is_empty());
            let first = body.next();
            first.is_some_and(|line| PLACEHOLDER_LINE_REGEX.is_match(line))
                && body.all(|line| PLACEHOLDER_LINE_REGEX.is_match(line))
        }
    };
    if !sections.iter().any(is_placeholder_section) {
        return markdown.to_string();
    }
    let kept: Vec<&str> = sections
        .iter()
        .filter(|section| !is_placeholder_section(section))
        .flatten()
        .copied()
        .collect();
    let mut cleaned = kept.join("\n").trim_end().to_string();
    if markdown.ends_with('\n') {
        cleaned.push('\n');
    }
    cleaned
}

fn build_final_user_prompt(notes: &str, content: &str) -> String {
    let mut prompt = String::new();
    if !notes.trim().is_empty() {
        prompt.push_str("<my_notes>\n");
        prompt.push_str(notes);
        prompt.push_str("\n</my_notes>\n\n");
    }
    prompt.push_str(&format!("<transcript>\n{content}\n</transcript>\n"));
    prompt
}

/// The example title the old default notes prompt carried. Small local models
/// copied it verbatim, so every meeting ended up named after it (PRA-636), and
/// user prompt overrides saved from that default may still contain it.
const PROMPT_ECHO_TITLE: &str = "Roadmap Sync";
/// Keeps the title prompt small enough for local models' context windows.
const TITLE_TRANSCRIPT_MAX_CHARS: usize = 12_000;
const TITLE_MAX_WORDS: usize = 8;
const TITLE_MAX_CHARS: usize = 80;

static TITLE_LABEL_REGEX: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)^(?:meeting\s+)?title\s*:").unwrap());

/// System prompt for the standalone title call. It deliberately carries no
/// example title, since small models echo examples instead of reading the transcript.
pub fn title_system_prompt() -> &'static str {
    r#"You name meetings. Read the transcript, and the user's notes if any, and reply with ONLY a short title of at most 6 words describing what this specific meeting discussed: its main topic, decision, or project, using the actual names and subjects from the conversation.

Do not use a generic or placeholder title. Do not add quotes, Markdown, a "Title:" label, trailing punctuation, reasoning, or any explanation. Output ONLY the title on a single line."#
}

pub fn build_title_user_prompt(transcript: &str, notes: Option<&str>) -> String {
    let transcript = transcript.trim();
    let transcript = match transcript.char_indices().nth(TITLE_TRANSCRIPT_MAX_CHARS) {
        Some((cut, _)) => &transcript[..cut],
        None => transcript,
    };
    let mut prompt = String::from("Write a short title (max 6 words) for this meeting.\n\n");
    if let Some(notes) = notes.map(str::trim).filter(|notes| !notes.is_empty()) {
        prompt.push_str(&format!("<my_notes>\n{notes}\n</my_notes>\n\n"));
    }
    prompt.push_str(&format!(
        "<transcript>\n{transcript}\n</transcript>\n\nReturn ONLY the title."
    ));
    prompt
}

/// Turns a raw title completion into a clean single-line title, or None when
/// nothing usable is left.
pub fn sanitize_generated_title(raw: &str) -> Option<String> {
    let cleaned = clean_llm_markdown_detailed(raw);
    if contains_reasoning_marker(&cleaned.markdown) {
        return None;
    }
    cleaned
        .markdown
        .lines()
        .filter(|line| !line.trim_start().starts_with("```"))
        .find_map(sanitize_title_line)
}

fn sanitize_title_line(line: &str) -> Option<String> {
    const WRAPPERS: &[char] = &['"', '`', '*', '_', '“', '”'];
    let mut title = line.trim();
    // Decorations nest in any order (`**Title:** "X"`), so strip until stable.
    loop {
        let before = title;
        title = title.trim_start_matches('#').trim();
        title = title.trim_matches(WRAPPERS).trim();
        for (open, close) in [('\'', '\''), ('‘', '’')] {
            if title.len() > 1 && title.starts_with(open) && title.ends_with(close) {
                title = title[open.len_utf8()..title.len() - close.len_utf8()].trim();
            }
        }
        if let Some(label) = TITLE_LABEL_REGEX.find(title) {
            title = title[label.end()..].trim();
        }
        if title == before {
            break;
        }
    }

    let title = title
        .split_whitespace()
        .take(TITLE_MAX_WORDS)
        .collect::<Vec<_>>()
        .join(" ");
    // Over the character cap, cut at the last word boundary that fits.
    let title = if title.chars().count() > TITLE_MAX_CHARS {
        let cut: String = title.chars().take(TITLE_MAX_CHARS + 1).collect();
        match cut.rfind(' ') {
            Some(end) => cut[..end].to_string(),
            None => cut.chars().take(TITLE_MAX_CHARS).collect(),
        }
    } else {
        title
    };
    let title = title
        .trim_end_matches(['.', ',', ';', ':', '!', '…', '-'])
        .trim_end();
    (!title.is_empty()).then(|| title.to_string())
}

/// True when `title` is a prompt's example title rather than something the
/// meeting discussed, i.e. `source_text` never mentions the example's topic.
pub fn is_prompt_echo_title(title: &str, source_text: &str) -> bool {
    let source_text = source_text.to_lowercase();
    [
        (PROMPT_ECHO_TITLE, "roadmap"),
        (TRANSCRIPT_ONLY_EXAMPLE_TITLE, "office"),
    ]
    .into_iter()
    .any(|(example, topic)| {
        title
            .trim_matches(|c: char| !c.is_alphanumeric())
            .split_whitespace()
            .map(str::to_lowercase)
            .eq(example.split_whitespace().map(str::to_lowercase))
            && !source_text.contains(topic)
    })
}

/// Rough token count estimation using character count
pub fn rough_token_count(s: &str) -> usize {
    let char_count = s.chars().count();
    (char_count as f64 * 0.35).ceil() as usize
}

/// Chunks text into overlapping segments based on token count
/// Uses character-based chunking for proper Unicode support
///
/// # Arguments
/// * `text` - The text to chunk
/// * `chunk_size_tokens` - Maximum tokens per chunk
/// * `overlap_tokens` - Number of overlapping tokens between chunks
///
/// # Returns
/// Vector of text chunks with smart word-boundary splitting
pub fn chunk_text(text: &str, chunk_size_tokens: usize, overlap_tokens: usize) -> Vec<String> {
    info!(
        "Chunking text with token-based chunk_size: {} and overlap: {}",
        chunk_size_tokens, overlap_tokens
    );
    if chunk_size_tokens == 0 {
        return vec![];
    }
    // ~2.85 chars per token (inverse of 0.35 tokens per char from rough_token_count)
    let chars_per_token = 1.0 / 0.35;
    chunk_text_by_chars(
        text,
        (chunk_size_tokens as f64 * chars_per_token).ceil() as usize,
        (overlap_tokens as f64 * chars_per_token).ceil() as usize,
    )
}

/// Chunks text into overlapping segments of at most `chunk_size_chars`
/// characters, preferring sentence, then line, then word boundaries.
pub(crate) fn chunk_text_by_chars(
    text: &str,
    chunk_size_chars: usize,
    overlap_chars: usize,
) -> Vec<String> {
    if text.is_empty() || chunk_size_chars == 0 {
        return vec![];
    }

    // Collect characters for indexing (needed for proper Unicode support)
    let chars: Vec<char> = text.chars().collect();
    let total_chars = chars.len();

    if total_chars <= chunk_size_chars {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    let mut start_char = 0;

    while start_char < total_chars {
        let end_char = (start_char + chunk_size_chars).min(total_chars);
        let mut emitted_end_char = end_char;

        // Convert character indices to byte indices for string slicing
        let start_byte: usize = chars[..start_char].iter().map(|c| c.len_utf8()).sum();
        let mut end_byte: usize = chars[..end_char].iter().map(|c| c.len_utf8()).sum();

        // Try to break at a transcript line, sentence or word boundary
        if end_char < total_chars {
            let slice = &text[start_byte..end_byte];
            let longer_than_overlap = |end: &usize| slice[..*end].chars().count() > overlap_chars;
            let boundary = slice
                .rfind('\n')
                .map(|index| index + 1)
                .filter(longer_than_overlap)
                .or_else(|| {
                    slice
                        .rfind(". ")
                        .map(|index| index + 2)
                        .filter(longer_than_overlap)
                })
                .or_else(|| {
                    slice
                        .rfind(' ')
                        .map(|index| index + 1)
                        .filter(longer_than_overlap)
                });

            if let Some(boundary) = boundary {
                end_byte = start_byte + boundary;
                emitted_end_char = start_char + slice[..boundary].chars().count();
            }
        }

        // Extract chunk
        chunks.push(text[start_byte..end_byte].to_string());

        if emitted_end_char >= total_chars {
            break;
        }

        start_char = emitted_end_char
            .saturating_sub(overlap_chars)
            .max(start_char + 1);
    }

    info!("Created {} chunks from text", chunks.len());
    chunks
}

/// Extracts meeting name from the first heading in markdown
///
/// # Arguments
/// * `markdown` - Markdown content
///
/// # Returns
/// Meeting name if found, None otherwise
pub fn extract_meeting_name_from_markdown(markdown: &str) -> Option<String> {
    markdown
        .lines()
        .find(|line| line.starts_with("# "))
        .map(|line| line.trim_start_matches("# ").trim().to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GeneratedMeetingSummary {
    pub final_markdown: String,
    pub english_markdown: String,
    pub successful_chunk_count: i64,
    pub reasoning_stripped: bool,
    pub normalization_fallback: bool,
}

const CANCELLED_ERROR: &str = "Summary generation was cancelled";

/// Why a summary pass failed. Branching (re-split the chunk, retry, give up)
/// goes by the variant, never by the message text: a truncation message that
/// mentions "context window" is still a truncation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StageError {
    Cancelled,
    /// The prompt does not fit the model's context; a smaller input can.
    ContextOverflow(String),
    /// The reply hit the output limit and is incomplete.
    Truncated(String),
    Failed(String),
}

impl std::fmt::Display for StageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => f.write_str(CANCELLED_ERROR),
            Self::ContextOverflow(message) | Self::Truncated(message) | Self::Failed(message) => {
                f.write_str(message)
            }
        }
    }
}

impl From<String> for StageError {
    fn from(message: String) -> Self {
        Self::Failed(message)
    }
}

impl From<LlmError> for StageError {
    fn from(error: LlmError) -> Self {
        match error.kind {
            LlmErrorKind::ContextOverflow => Self::ContextOverflow(error.message),
            LlmErrorKind::Cancelled => Self::Cancelled,
            LlmErrorKind::Other => Self::Failed(error.message),
        }
    }
}

impl From<StageError> for String {
    fn from(error: StageError) -> Self {
        error.to_string()
    }
}

/// Tokens a chat template adds around system and user text, for estimates.
const CHAT_TEMPLATE_TOKENS: usize = 32;
/// Overlap between consecutive transcript chunks.
const CHUNK_OVERLAP_TOKENS: usize = 100;
const SUMMARY_SEPARATOR: &str = "\n---\n";
const SUMMARY_SEPARATOR_TOKENS: usize = 4;
/// Use slightly less than the measured characters-per-token when sizing chunks,
/// since token density varies across a transcript.
const CHUNK_SIZE_SAFETY: f64 = 0.95;

/// What the summary pipeline needs from a model: generation and token counting.
/// `ProviderBackend` is the real implementation; tests use a scripted fake.
pub(crate) trait SummaryBackend: Sync {
    fn generate(
        &self,
        system_prompt: &str,
        user_prompt: &str,
        options: &GenerationOptions,
    ) -> impl Future<Output = Result<LlmCompletion, LlmError>> + Send;

    /// Tokens in `text` alone.
    fn count_tokens(&self, text: &str) -> impl Future<Output = Result<usize, String>> + Send;

    /// Tokens in the full prompt the model will see, chat template included.
    fn count_prompt_tokens(
        &self,
        system_prompt: &str,
        user_prompt: &str,
    ) -> impl Future<Output = Result<usize, String>> + Send;

    fn provider(&self) -> &LLMProvider;

    fn is_cancelled(&self) -> bool;
}

/// Calls the configured provider through `generate_summary`. Built-in AI counts
/// tokens with the model's own tokenizer; other providers use a conservative
/// estimate.
pub(crate) struct ProviderBackend<'a> {
    pub client: &'a Client,
    pub provider: &'a LLMProvider,
    pub model_name: &'a str,
    pub api_key: &'a str,
    pub ollama_endpoint: Option<&'a str>,
    pub custom_openai_endpoint: Option<&'a str>,
    pub app_data_dir: Option<&'a PathBuf>,
    pub cancellation_token: Option<&'a CancellationToken>,
}

impl ProviderBackend<'_> {
    fn builtin_dir(&self) -> Result<&PathBuf, String> {
        self.app_data_dir
            .ok_or_else(|| "app_data_dir is required for BuiltInAI provider".to_string())
    }
}

impl SummaryBackend for ProviderBackend<'_> {
    async fn generate(
        &self,
        system_prompt: &str,
        user_prompt: &str,
        options: &GenerationOptions,
    ) -> Result<LlmCompletion, LlmError> {
        generate_completion(
            self.client,
            self.provider,
            self.model_name,
            self.api_key,
            system_prompt,
            user_prompt,
            self.ollama_endpoint,
            self.custom_openai_endpoint,
            options,
            self.app_data_dir,
            self.cancellation_token,
        )
        .await
    }

    async fn count_tokens(&self, text: &str) -> Result<usize, String> {
        if *self.provider != LLMProvider::BuiltInAI {
            return Ok(estimate_tokens(text));
        }
        match crate::summary::summary_engine::count_builtin_tokens(
            self.builtin_dir()?,
            self.model_name,
            text,
            self.cancellation_token,
        )
        .await
        {
            Ok(count) => Ok(count),
            // An older llama-helper without `tokenize`: budget on the estimate.
            Err(error)
                if matches!(
                    error.downcast_ref::<crate::summary::summary_engine::BuiltinError>(),
                    Some(crate::summary::summary_engine::BuiltinError::UnsupportedRequest(_))
                ) =>
            {
                warn!("llama-helper cannot count tokens ({error}); using an estimate");
                Ok(estimate_tokens(text))
            }
            Err(error) => Err(format!("Could not count transcript tokens: {error}")),
        }
    }

    async fn count_prompt_tokens(
        &self,
        system_prompt: &str,
        user_prompt: &str,
    ) -> Result<usize, String> {
        if *self.provider != LLMProvider::BuiltInAI {
            return Ok(estimate_tokens(system_prompt)
                + estimate_tokens(user_prompt)
                + CHAT_TEMPLATE_TOKENS);
        }
        let formatted = crate::summary::summary_engine::format_builtin_prompt(
            self.model_name,
            system_prompt,
            user_prompt,
        )
        .map_err(|error| error.to_string())?;
        self.count_tokens(&formatted).await
    }

    fn provider(&self) -> &LLMProvider {
        self.provider
    }

    fn is_cancelled(&self) -> bool {
        self.cancellation_token
            .is_some_and(CancellationToken::is_cancelled)
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
    if *provider != LLMProvider::BuiltInAI {
        return requested;
    }
    let Some(model) = crate::summary::summary_engine::get_model_by_name(model_name) else {
        return requested;
    };
    let mut budget = ContextBudget::new(model.context_size as usize);
    if let Some(requested) = requested {
        budget.max_output = requested as usize;
    }
    let client = Client::new();
    let backend = ProviderBackend {
        client: &client,
        provider,
        model_name,
        api_key: "",
        ollama_endpoint: None,
        custom_openai_endpoint: None,
        app_data_dir,
        cancellation_token: None,
    };
    let prompt_tokens = match backend
        .count_prompt_tokens(system_prompt, user_prompt)
        .await
    {
        Ok(tokens) => tokens,
        Err(error) => {
            warn!("Could not count prompt tokens ({error}); using an estimate");
            estimate_tokens(system_prompt) + estimate_tokens(user_prompt) + CHAT_TEMPLATE_TOKENS
        }
    };
    match budget.output_for_prompt(prompt_tokens) {
        Some(output) => Some(u32::try_from(output).unwrap_or(u32::MAX)),
        None => {
            warn!(
                "Prompt of {prompt_tokens} tokens leaves no output room in a {}-token context",
                budget.n_ctx
            );
            requested
        }
    }
}

/// The user-facing error for an output that hit the model's limit. Such output
/// is incomplete and must never be saved as a finished summary.
/// A small context window is named as the likely cause.
pub(crate) fn truncation_error_for(
    stage: &str,
    provider: &LLMProvider,
    budget: Option<&ContextBudget>,
) -> String {
    if let Some(budget) = budget.filter(|budget| budget.is_small()) {
        let fix = if *provider == LLMProvider::Ollama {
            "Pick a model with a context window of at least 8k tokens, or set num_ctx in its Modelfile so Ollama reports it."
        } else {
            "Pick a model with a context window of at least 8k tokens."
        };
        return format!(
            "{stage} was cut off because the model runs with a context window of only {} tokens, too little room for the summary, so the incomplete summary was not saved. {fix}",
            budget.n_ctx
        );
    }
    let hint = match provider {
        LLMProvider::CustomOpenAI => {
            "Raise Max tokens in the custom endpoint settings, then try again."
        }
        LLMProvider::BuiltInAI | LLMProvider::Ollama => {
            "Try again with a shorter transcript, or pick a model with a larger context window."
        }
        _ => "Try again, or pick a different model.",
    };
    format!(
        "{stage} was cut off because the model reached its output limit, so the incomplete summary was not saved. {hint}"
    )
}

pub(crate) fn chars_for_tokens(tokens: usize, chars_per_token: f64) -> usize {
    ((tokens as f64 * chars_per_token * CHUNK_SIZE_SAFETY).floor() as usize).max(1)
}

/// Runs the summary passes against one backend, with one output budget and,
/// for local models, one context budget.
pub(crate) struct SummaryPipeline<'a, B: SummaryBackend> {
    pub(crate) backend: &'a B,
    options: GenerationOptions,
    pub(crate) budget: Option<ContextBudget>,
    reasoning_stripped: AtomicBool,
}

impl<'a, B: SummaryBackend> SummaryPipeline<'a, B> {
    fn new(backend: &'a B, options: &GenerationOptions, budget: Option<ContextBudget>) -> Self {
        let mut options = options.clone();
        if let Some(budget) = budget {
            options
                .max_tokens
                .get_or_insert(u32::try_from(budget.max_output).unwrap_or(u32::MAX));
            if *backend.provider() == LLMProvider::Ollama {
                options.num_ctx = Some(u32::try_from(budget.n_ctx).unwrap_or(u32::MAX));
            }
        }
        Self {
            backend,
            options,
            budget,
            reasoning_stripped: AtomicBool::new(false),
        }
    }

    fn reasoning_stripped(&self) -> bool {
        self.reasoning_stripped.load(Ordering::Relaxed)
    }

    /// One model call that must produce a complete answer. A truncated reply
    /// is retried once with a larger output budget when the context has room;
    /// otherwise it fails rather than passing incomplete text along.
    async fn complete(
        &self,
        stage: &str,
        system_prompt: &str,
        user_prompt: &str,
        require_visible: bool,
    ) -> Result<CleanedLlmMarkdown, StageError> {
        self.complete_attempt(stage, system_prompt, user_prompt, require_visible, 1)
            .await
    }

    /// [`Self::complete`] for the `attempt`th try of the same prompt, which
    /// samples with a different seed after the first.
    pub(crate) async fn complete_attempt(
        &self,
        stage: &str,
        system_prompt: &str,
        user_prompt: &str,
        require_visible: bool,
        attempt: usize,
    ) -> Result<CleanedLlmMarkdown, StageError> {
        if self.backend.is_cancelled() {
            return Err(StageError::Cancelled);
        }
        let mut options = self.options.clone();
        options.seed = seed_for_attempt(options.seed, attempt);
        for attempt in 1..=2 {
            let completion = self
                .backend
                .generate(system_prompt, user_prompt, &options)
                .await?;
            if completion.reasoning_stripped {
                self.reasoning_stripped.store(true, Ordering::Relaxed);
            }
            if !completion.truncated {
                let mut cleaned = clean_llm_markdown_detailed(&completion.content);
                cleaned.reasoning_stripped |= completion.reasoning_stripped;
                if cleaned.reasoning_stripped {
                    self.reasoning_stripped.store(true, Ordering::Relaxed);
                }
                if require_visible {
                    require_visible_markdown(stage, &cleaned).map_err(StageError::Failed)?;
                }
                return Ok(cleaned);
            }
            if attempt == 1 {
                if let Some(grown) = self
                    .grown_output(&options, &completion, system_prompt, user_prompt)
                    .await?
                {
                    warn!(
                        "{} hit the output limit ({:?} tokens); retrying with {} tokens",
                        stage, options.max_tokens, grown
                    );
                    options.max_tokens = Some(grown);
                    continue;
                }
            }
            break;
        }
        error!("{} was truncated at the output limit", stage);
        Err(StageError::Truncated(truncation_error_for(
            stage,
            self.backend.provider(),
            self.budget.as_ref(),
        )))
    }

    async fn grown_output(
        &self,
        options: &GenerationOptions,
        completion: &LlmCompletion,
        system_prompt: &str,
        user_prompt: &str,
    ) -> Result<Option<u32>, String> {
        let Some(budget) = self.budget else {
            return Ok(None);
        };
        let current = options
            .max_tokens
            .map_or(budget.max_output, |tokens| tokens as usize);
        // A reported count is not the whole prompt when the server reused a
        // cached prefix: Ollama's prompt_eval_count only counts the tokens it
        // evaluated, so a regenerate reports a fraction of the prompt and the
        // grown request would overflow num_ctx. Take the larger of the two.
        let counted = self
            .backend
            .count_prompt_tokens(system_prompt, user_prompt)
            .await?;
        let prompt_tokens = completion
            .prompt_tokens
            .map_or(counted, |reported| counted.max(reported as usize));
        Ok(budget
            .grown_output(current, prompt_tokens)
            .map(|tokens| u32::try_from(tokens).unwrap_or(u32::MAX)))
    }

    /// Pass 1: the English notes, map-reducing the transcript first when it
    /// does not fit the final prompt's budget.
    async fn english_notes(
        &self,
        text: &str,
        notes: &str,
        final_prompt: &SummaryPrompt,
    ) -> Result<(String, i64), StageError> {
        let final_system_prompt = final_prompt.text.as_str();
        let (content, chunk_count) = match self.budget {
            None => (text.to_string(), 1),
            Some(budget) => {
                let overhead = self
                    .backend
                    .count_prompt_tokens(final_system_prompt, &build_final_user_prompt(notes, ""))
                    .await?;
                let input_budget = budget.input_budget(overhead);
                if input_budget < MIN_CHUNK_TOKENS {
                    let message = if notes.trim().is_empty() {
                        format!("This model runs with only {} tokens of context, too little for the summary prompt and a transcript. Pick a model with at least 8k tokens of context.", budget.n_ctx)
                    } else {
                        "Your meeting notes are too long for this model's context window. Shorten the notes, or pick a model with a larger context window.".to_string()
                    };
                    return Err(StageError::Failed(message));
                }
                let transcript_tokens = self.backend.count_tokens(text).await?;
                info!(
                    "Context budget: n_ctx={}, max_output={}, prompt overhead={}, transcript={} tokens, input budget={}",
                    budget.n_ctx, budget.max_output, overhead, transcript_tokens, input_budget
                );
                if transcript_tokens <= input_budget {
                    (text.to_string(), 1)
                } else {
                    let chunk_overhead = self
                        .backend
                        .count_prompt_tokens(
                            &chunk_system_prompt(),
                            &build_chunk_summary_user_prompt(""),
                        )
                        .await?;
                    let chunk_budget = budget.input_budget(chunk_overhead).max(MIN_CHUNK_TOKENS);
                    let ratio = chars_per_token(text, transcript_tokens);
                    let summaries = self
                        .summarize_transcript_chunks(text, chunk_budget, ratio)
                        .await?;
                    let count = summaries.len() as i64;
                    (self.combine_summaries(summaries, budget).await?, count)
                }
            }
        };

        info!("Generating enhanced notes");
        let cleaned = self
            .complete(
                "Final summary",
                final_system_prompt,
                &build_final_user_prompt(notes, &content),
                true,
            )
            .await?;
        let markdown = if final_prompt.drops_placeholder_sections() {
            drop_placeholder_sections(&cleaned.markdown)
        } else {
            cleaned.markdown
        };
        Ok((markdown, chunk_count))
    }

    /// Summarises each transcript chunk. A chunk that overflows the context is
    /// split smaller and retried instead of being resent unchanged.
    async fn summarize_transcript_chunks(
        &self,
        text: &str,
        chunk_budget: usize,
        chars_per_token: f64,
    ) -> Result<Vec<String>, StageError> {
        let system_prompt = chunk_system_prompt();
        let overlap = chars_for_tokens(CHUNK_OVERLAP_TOKENS, chars_per_token);
        let mut queue: VecDeque<(String, usize)> = chunk_text_by_chars(
            text,
            chars_for_tokens(chunk_budget, chars_per_token),
            overlap,
        )
        .into_iter()
        .map(|chunk| (chunk, chunk_budget))
        .collect();
        info!("Summarising transcript in {} chunks", queue.len());

        let mut summaries = Vec::with_capacity(queue.len());
        while let Some((chunk, chunk_tokens)) = queue.pop_front() {
            let section = summaries.len() + 1;
            let total = summaries.len() + queue.len() + 1;
            let prompt = build_chunk_summary_user_prompt(&chunk);
            for attempt in 1..=MAX_CHUNK_ATTEMPTS {
                match self
                    .complete_attempt("Summary chunk", &system_prompt, &prompt, true, attempt)
                    .await
                {
                    Ok(cleaned) => {
                        summaries.push(cleaned.markdown);
                        break;
                    }
                    Err(_) if self.backend.is_cancelled() => {
                        return Err(StageError::Cancelled);
                    }
                    Err(StageError::Cancelled) => return Err(StageError::Cancelled),
                    // Already retried with a larger output budget; splitting
                    // the input would not make the reply shorter.
                    Err(error @ StageError::Truncated(_)) => return Err(error),
                    Err(StageError::ContextOverflow(error)) => {
                        let Some(smaller) = shrink_chunk_tokens(chunk_tokens) else {
                            return Err(StageError::Failed(format!(
                                "Transcript section {section} of {total} does not fit the model's context window even after splitting it: {error}"
                            )));
                        };
                        warn!(
                            "Transcript section {}/{} overflowed the context at ~{} tokens; splitting to ~{} tokens",
                            section, total, chunk_tokens, smaller
                        );
                        let pieces = chunk_text_by_chars(
                            &chunk,
                            chars_for_tokens(smaller, chars_per_token),
                            overlap.min(chars_for_tokens(smaller / 4, chars_per_token)),
                        );
                        for piece in pieces.into_iter().rev() {
                            queue.push_front((piece, smaller));
                        }
                        break;
                    }
                    Err(error)
                        if should_retry_chunk_failure(attempt, self.backend.is_cancelled()) =>
                    {
                        warn!(
                            "Failed processing chunk {}/{} on attempt {}/{}: {}; retrying",
                            section, total, attempt, MAX_CHUNK_ATTEMPTS, error
                        );
                    }
                    Err(error) => {
                        error!(
                            "Failed processing chunk {}/{} on attempt {}/{}: {}",
                            section, total, attempt, MAX_CHUNK_ATTEMPTS, error
                        );
                        return Err(StageError::Failed(format!(
                            "Summary generation could not complete because transcript section {} of {} failed after {} attempts: {}. Please retry.",
                            section, total, MAX_CHUNK_ATTEMPTS, error
                        )));
                    }
                }
            }
        }
        Ok(summaries)
    }

    /// Combines chunk summaries in groups that fit the context, repeating until
    /// one summary remains.
    async fn combine_summaries(
        &self,
        mut summaries: Vec<String>,
        budget: ContextBudget,
    ) -> Result<String, StageError> {
        if summaries.is_empty() {
            return Err(StageError::Failed(
                "Multi-level summarization failed: No chunks were processed successfully."
                    .to_string(),
            ));
        }
        let overhead = self
            .backend
            .count_prompt_tokens(
                COMBINE_SYSTEM_PROMPT,
                &build_combine_summary_user_prompt(""),
            )
            .await?;
        let combine_budget = budget.input_budget(overhead);
        while summaries.len() > 1 {
            let mut tokens = Vec::with_capacity(summaries.len());
            for summary in &summaries {
                tokens.push(self.backend.count_tokens(summary).await?);
            }
            let mut groups = group_within_budget(&tokens, combine_budget, SUMMARY_SEPARATOR_TOKENS);
            if groups.len() == summaries.len() {
                // No two summaries fit together. Pair them anyway so every
                // round makes progress; an overflow then surfaces as an error.
                groups = (0..summaries.len())
                    .step_by(2)
                    .map(|start| start..(start + 2).min(summaries.len()))
                    .collect();
            }
            let mut next = Vec::with_capacity(groups.len());
            for group in groups {
                if group.len() == 1 {
                    next.push(summaries[group.start].clone());
                    continue;
                }
                let joined = summaries[group].join(SUMMARY_SEPARATOR);
                let cleaned = self
                    .complete(
                        "Combined summary",
                        COMBINE_SYSTEM_PROMPT,
                        &build_combine_summary_user_prompt(&joined),
                        true,
                    )
                    .await?;
                next.push(cleaned.markdown);
            }
            summaries = next;
        }
        Ok(summaries.remove(0))
    }

    async fn translate_markdown(
        &self,
        english_markdown: &str,
        target_language: &str,
    ) -> Result<CleanedLlmMarkdown, StageError> {
        let system_prompt = translation_system_prompt(target_language);
        let user_prompt = format!(
            "Translate the following Markdown document into {target_language}. Return ONLY the translated Markdown, nothing else.\n\n<document>\n{english_markdown}\n</document>"
        );
        self.complete("Translation", &system_prompt, &user_prompt, true)
            .await
    }

    async fn normalize_markdown_to_english(
        &self,
        markdown: &str,
    ) -> Result<CleanedLlmMarkdown, String> {
        let user_prompt = format!(
            "Convert the following Markdown document into English. Return ONLY the English Markdown, nothing else.\n\n<document>\n{markdown}\n</document>"
        );
        self.complete(
            "English normalization pass",
            english_normalization_system_prompt(),
            &user_prompt,
            false,
        )
        .await
        .map_err(|error| match error {
            StageError::Cancelled => CANCELLED_ERROR.to_string(),
            error => format!("English normalization pass failed: {error}"),
        })
    }
}

/// Generates the meeting notes.
///
/// `context_budget` is the context window local models run with (built-in AI,
/// Ollama); `None` means a hosted model with room for the whole transcript.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn generate_meeting_summary<B: SummaryBackend>(
    backend: &B,
    model_name: &str,
    text: &str,
    custom_prompt: &str,
    final_prompt: &SummaryPrompt,
    context_budget: Option<ContextBudget>,
    options: &GenerationOptions,
    summary_language: Option<&str>,
    detected_transcript_language: Option<&str>,
    cached_english: Option<&str>,
    outline: Option<&Outline>,
) -> Result<GeneratedMeetingSummary, String> {
    if backend.is_cancelled() {
        return Err(CANCELLED_ERROR.to_string());
    }
    info!(
        "Starting summary generation with provider: {:?}, model: {}",
        backend.provider(),
        model_name
    );
    let pipeline = SummaryPipeline::new(backend, options, context_budget);
    // Section calls write a handful of bullets: a smaller output reserve
    // leaves more of the context for the topic's transcript.
    let section_pipeline = outline.map(|_| {
        let budget = context_budget.map(|budget| ContextBudget {
            max_output: budget.max_output.min(SECTION_MAX_OUTPUT_TOKENS),
            ..budget
        });
        SummaryPipeline::new(backend, options, budget)
    });

    let (mut english_markdown, successful_chunk_count) =
        if let Some(cached) = resolve_cached_english(cached_english, summary_language) {
            info!(
                "✓ Using cached English summary ({} chars), skipping pass 1",
                cached.len()
            );
            (cached.to_string(), 1_i64)
        } else if let (Some(outline), Some(section_pipeline)) = (outline, &section_pipeline) {
            section_pipeline
                .outline_notes(outline, text, custom_prompt)
                .await?
        } else {
            pipeline
                .english_notes(text, custom_prompt, final_prompt)
                .await?
        };

    let (final_markdown, normalization_fallback) =
        match resolve_final_language_action(summary_language, detected_transcript_language) {
            FinalLanguageAction::Translate(language) => {
                let translated = pipeline
                    .translate_markdown(&english_markdown, language)
                    .await
                    .map_err(|error| format!("Translation to {language} failed: {error}"))?;
                (translated.markdown, false)
            }
            FinalLanguageAction::NormalizeEnglish => {
                let (normalized, fallback) = english_markdown_after_normalization_result(
                    &english_markdown,
                    pipeline
                        .normalize_markdown_to_english(&english_markdown)
                        .await,
                    backend.is_cancelled(),
                )?;
                english_markdown = normalized.markdown.clone();
                (normalized.markdown, fallback)
            }
            FinalLanguageAction::ReturnEnglish => (english_markdown.clone(), false),
        };

    Ok(GeneratedMeetingSummary {
        final_markdown,
        english_markdown,
        successful_chunk_count,
        reasoning_stripped: pipeline.reasoning_stripped()
            || section_pipeline
                .as_ref()
                .is_some_and(SummaryPipeline::reasoning_stripped),
        normalization_fallback,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_text_preserves_content_after_early_sentence_boundary() {
        let marker = "LOST_MARKER";
        let text = format!("Intro. {marker} trailing content ensures chunking");

        let chunks = chunk_text(&text, 10, 1);

        assert!(
            chunks.iter().any(|chunk| chunk.contains(marker)),
            "marker was omitted from all chunks: {chunks:?}"
        );
    }

    #[test]
    fn chunk_text_keeps_unicode_boundaries() {
        assert_eq!(chunk_text("é ab", 1, 0), vec!["é ", "ab"]);
    }

    #[test]
    fn chunk_text_progresses_when_overlap_matches_window() {
        assert_eq!(chunk_text("abcd", 1, 1), vec!["abc", "bcd"]);
    }

    #[test]
    fn chunk_summary_prompt_forces_english_base_output() {
        let prompt = build_chunk_summary_user_prompt("会議の内容");

        assert!(prompt.contains(ENGLISH_BASE_SUMMARY_INSTRUCTION));
        assert!(prompt.contains("<transcript_chunk>"));
    }

    #[test]
    fn combine_summary_prompt_forces_english_base_output() {
        let prompt = build_combine_summary_user_prompt("chunk one\n---\nchunk two");

        assert!(prompt.contains(ENGLISH_BASE_SUMMARY_INSTRUCTION));
        assert!(prompt.contains("<summaries>"));
    }

    #[test]
    fn notes_prompt_forces_english_base_output() {
        let prompt = default_notes_system_prompt();

        assert!(prompt.contains(ENGLISH_BASE_SUMMARY_INSTRUCTION));
        assert!(prompt.contains("bullet"));
        assert!(prompt.contains("my_notes"));
        // The user's notes must stay the outline rather than being replaced.
        assert!(prompt.contains("original order"));
    }

    #[test]
    fn notes_prompt_forbids_reasoning_output() {
        let prompt = default_notes_system_prompt();
        assert!(
            prompt.to_lowercase().contains("no reasoning")
                || prompt.contains("meta-commentary")
                || prompt.contains("self-correction")
        );
    }

    #[test]
    fn chunk_prompt_forbids_reasoning_output() {
        let prompt = build_chunk_summary_user_prompt("x");
        assert!(
            prompt.contains("Do not include reasoning")
                || prompt.contains("meta-commentary")
                || prompt.contains("self-correction")
        );
    }

    #[test]
    fn english_base_instruction_marks_non_english_prose_invalid_without_bloat() {
        assert!(ENGLISH_BASE_SUMMARY_INSTRUCTION.contains("non-English prose is invalid"));
        assert!(ENGLISH_BASE_SUMMARY_INSTRUCTION.len() <= 120);
    }

    #[test]
    fn english_target_with_english_transcript_skips_normalization() {
        assert_eq!(
            resolve_final_language_action(Some("en"), Some("en")),
            FinalLanguageAction::ReturnEnglish
        );
    }

    #[test]
    fn english_target_with_non_english_transcript_normalizes_to_english() {
        assert_eq!(
            resolve_final_language_action(Some("en"), Some("ja")),
            FinalLanguageAction::NormalizeEnglish
        );
    }

    #[test]
    fn unknown_transcript_language_skips_normalization() {
        for summary_language in [Some("en"), None] {
            assert_eq!(
                resolve_final_language_action(summary_language, None),
                FinalLanguageAction::ReturnEnglish
            );
            assert_eq!(
                resolve_final_language_action(summary_language, Some("zz-unknown")),
                FinalLanguageAction::ReturnEnglish
            );
        }
    }

    #[test]
    fn confidently_detected_non_english_transcript_normalizes() {
        assert_eq!(
            resolve_final_language_action(None, Some("de")),
            FinalLanguageAction::NormalizeEnglish
        );
        assert_eq!(
            resolve_final_language_action(None, Some("en-GB")),
            FinalLanguageAction::ReturnEnglish
        );
    }

    #[test]
    fn non_english_target_uses_translation_flow() {
        assert_eq!(
            resolve_final_language_action(Some("fr"), Some("ja")),
            FinalLanguageAction::Translate("French")
        );
    }

    #[test]
    fn normalization_fallback_preserves_markdown_and_observed_reasoning() {
        assert_eq!(
            english_markdown_after_normalization_result(
                "# Original",
                Ok(CleanedLlmMarkdown {
                    markdown: String::new(),
                    reasoning_stripped: true,
                }),
                false,
            )
            .unwrap(),
            (
                CleanedLlmMarkdown {
                    markdown: "# Original".to_string(),
                    reasoning_stripped: true,
                },
                true,
            )
        );
        assert!(english_markdown_after_normalization_result(
            "# Original",
            Err("Summary generation was cancelled".to_string()),
            true,
        )
        .is_err());
    }

    #[test]
    fn custom_notes_prompt_applies_only_when_there_are_notes() {
        let with_notes = resolve_summary_system_prompt(Some("Custom prompt text"), None, "ship?");
        assert_eq!(with_notes.text, "Custom prompt text");
        assert_eq!(with_notes.kind, SummaryPromptKind::Custom);
        // A notes prompt is written around <my_notes>: a meeting without
        // notes gets the fixed-section prompt instead.
        let without = resolve_summary_system_prompt(Some("Custom prompt text"), None, " \n");
        assert_eq!(without.text, default_transcript_only_system_prompt());
        assert_eq!(without.kind, SummaryPromptKind::DefaultTranscriptOnly);
    }

    #[test]
    fn custom_no_notes_prompt_applies_only_without_notes() {
        let without = resolve_summary_system_prompt(None, Some(" Mine \n"), "");
        assert_eq!(without.text, "Mine");
        assert_eq!(without.kind, SummaryPromptKind::Custom);
        let with_notes = resolve_summary_system_prompt(None, Some("Mine"), "ship?");
        assert_eq!(with_notes.text, default_notes_system_prompt());
        assert_eq!(with_notes.kind, SummaryPromptKind::DefaultNotes);
    }

    #[test]
    fn summary_prompt_without_override_depends_on_notes() {
        for override_prompt in [None, Some("   \n\t")] {
            assert_eq!(
                resolve_summary_system_prompt(override_prompt, override_prompt, "launch date?")
                    .text,
                default_notes_system_prompt()
            );
            assert_eq!(
                resolve_summary_system_prompt(override_prompt, override_prompt, "  \n ").text,
                default_transcript_only_system_prompt()
            );
        }
    }

    #[test]
    fn notes_prompt_still_says_what_to_do_without_notes() {
        assert!(default_notes_system_prompt().contains("If `<my_notes>` is missing or empty"));
    }

    #[test]
    fn retries_step_the_seed_but_the_first_attempt_keeps_it() {
        assert_eq!(seed_for_attempt(None, 1), None);
        assert_eq!(seed_for_attempt(Some(7), 1), Some(7));
        assert_eq!(seed_for_attempt(None, 2), Some(SUMMARY_SEED + 1));
        assert_eq!(seed_for_attempt(Some(7), 3), Some(9));
    }

    #[test]
    fn every_summary_prompt_explains_speaker_labels() {
        for prompt in [
            default_notes_system_prompt(),
            default_transcript_only_system_prompt(),
            chunk_system_prompt(),
        ] {
            assert!(
                prompt.contains("`[You]` is the person who recorded"),
                "{prompt}"
            );
            assert!(prompt.contains("`[Others]`"), "{prompt}");
            assert!(prompt.contains("several different people"), "{prompt}");
        }
    }

    #[test]
    fn transcript_only_prompt_has_fixed_sections_and_example() {
        let prompt = default_transcript_only_system_prompt();
        let positions: Vec<usize> = [
            "`# Title`",
            "`## Summary`",
            "`## Decisions`",
            "`## Action items`",
            "`## Open questions`",
        ]
        .iter()
        .map(|section| prompt.find(section).unwrap_or_else(|| panic!("{section}")))
        .collect();
        assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(prompt.contains("at most 6 words"));
        assert!(prompt.contains("2-3 sentences"));
        assert!(prompt.contains("due date only if one was stated"));
        assert!(prompt.contains("leave out any section that would be empty"));
        assert!(prompt.contains("**EXAMPLE**"));
        assert!(prompt.contains(ENGLISH_BASE_SUMMARY_INSTRUCTION));
        assert!(prompt.contains("no reasoning"));
        assert!(!prompt.contains(PROMPT_ECHO_TITLE));
    }

    #[test]
    fn placeholder_only_sections_are_dropped() {
        let markdown = "# Launch\n\n## Summary\nShipped.\n\n## Decisions\n- None\n\n## Action items\n- **Priya** — checklist\n\n## Open questions\n- None.\n";
        assert_eq!(
            drop_placeholder_sections(markdown),
            "# Launch\n\n## Summary\nShipped.\n\n## Action items\n- **Priya** — checklist\n"
        );
        for placeholder in ["None", "* n/a", "- Nothing", "- None noted.", "_None_"] {
            let markdown = format!("# T\n\n## Open questions\n{placeholder}\n");
            assert_eq!(
                drop_placeholder_sections(&markdown),
                "# T\n",
                "{placeholder}"
            );
        }
        let kept = "# T\n\n## Decisions\n- None of the vendors met the bar.\n";
        assert_eq!(drop_placeholder_sections(kept), kept);
        let untouched = "# T\n\n- note\n";
        assert_eq!(drop_placeholder_sections(untouched), untouched);
    }

    #[test]
    fn final_user_prompt_wraps_notes_only_when_present() {
        assert_eq!(
            build_final_user_prompt("", "[00:01] [You] hi"),
            "<transcript>\n[00:01] [You] hi\n</transcript>\n"
        );
        assert_eq!(
            build_final_user_prompt(" \n", "x"),
            "<transcript>\nx\n</transcript>\n"
        );
        assert_eq!(
            build_final_user_prompt("ship?", "x"),
            "<my_notes>\nship?\n</my_notes>\n\n<transcript>\nx\n</transcript>\n"
        );
    }

    #[test]
    fn truncation_error_is_clear_and_provider_specific() {
        let custom = truncation_error_for("Final summary", &LLMProvider::CustomOpenAI, None);
        assert!(custom.starts_with("Final summary was cut off"));
        assert!(custom.contains("not saved"));
        assert!(custom.contains("Max tokens"));
        assert!(
            truncation_error_for("Final summary", &LLMProvider::BuiltInAI, None)
                .contains("larger context window")
        );
        let small = truncation_error_for(
            "Final summary",
            &LLMProvider::Ollama,
            Some(&ContextBudget::new(4_000)),
        );
        assert!(
            small.contains("context window of only 4000 tokens"),
            "{small}"
        );
        assert!(small.contains("num_ctx"), "{small}");
        assert!(!truncation_error_for(
            "Final summary",
            &LLMProvider::Ollama,
            Some(&ContextBudget::new(16_384)),
        )
        .contains("only"));
    }

    #[test]
    fn prompts_carry_no_echoable_example_title() {
        assert!(!default_notes_system_prompt().contains(PROMPT_ECHO_TITLE));
        assert!(!default_notes_system_prompt().contains(TRANSCRIPT_ONLY_EXAMPLE_TITLE));
        assert!(!default_notes_system_prompt().contains("e.g. `#"));
        assert!(!title_system_prompt().contains(PROMPT_ECHO_TITLE));
        assert!(!title_system_prompt().contains("e.g."));
    }

    #[test]
    fn title_user_prompt_wraps_transcript_and_non_blank_notes() {
        let prompt = build_title_user_prompt("Alice: ship it", Some("  launch date?  "));
        assert!(prompt.contains("<transcript>\nAlice: ship it\n</transcript>"));
        assert!(prompt.contains("<my_notes>\nlaunch date?\n</my_notes>"));

        for notes in [None, Some(""), Some("  \n\t")] {
            let prompt = build_title_user_prompt("Alice: ship it", notes);
            assert!(!prompt.contains("<my_notes>"));
            assert!(prompt.contains("<transcript>"));
        }
    }

    #[test]
    fn title_user_prompt_truncates_long_transcript_on_char_boundary() {
        let transcript = "é".repeat(TITLE_TRANSCRIPT_MAX_CHARS + 500);
        let prompt = build_title_user_prompt(&transcript, None);
        let kept = prompt
            .split("<transcript>\n")
            .nth(1)
            .and_then(|rest| rest.split("\n</transcript>").next())
            .unwrap();
        assert_eq!(kept.chars().count(), TITLE_TRANSCRIPT_MAX_CHARS);
    }

    #[test]
    fn sanitizer_extracts_clean_title() {
        for (raw, expected) in [
            ("Budget Review", "Budget Review"),
            (
                "<think>they talk about hiring</think>\nHiring Plan",
                "Hiring Plan",
            ),
            ("# Q3 Launch Planning", "Q3 Launch Planning"),
            ("## Q3 Launch Planning\n\nbody", "Q3 Launch Planning"),
            ("\"Vendor Contract Renewal\"", "Vendor Contract Renewal"),
            ("'Vendor Contract Renewal'", "Vendor Contract Renewal"),
            ("“Vendor Contract Renewal”", "Vendor Contract Renewal"),
            ("`Vendor Contract Renewal`", "Vendor Contract Renewal"),
            (
                "Title: Onboarding Flow Redesign",
                "Onboarding Flow Redesign",
            ),
            (
                "**Title:** \"Onboarding Flow Redesign.\"",
                "Onboarding Flow Redesign",
            ),
            ("Meeting title: Onboarding Flow", "Onboarding Flow"),
            (
                "```\nDatabase Migration Plan\n```",
                "Database Migration Plan",
            ),
            (
                "```text\nDatabase Migration Plan\n```",
                "Database Migration Plan",
            ),
            (
                "\n\n  Weekly   Sync\twith  Design.  \nextra line",
                "Weekly Sync with Design",
            ),
            ("Hire or Build?", "Hire or Build?"),
        ] {
            assert_eq!(
                sanitize_generated_title(raw).as_deref(),
                Some(expected),
                "raw: {raw:?}"
            );
        }
    }

    #[test]
    fn sanitizer_rejects_empty_and_reasoning_only_output() {
        for raw in [
            "",
            "   \n\t",
            "<think>only thinking</think>",
            "```\n```",
            "#",
            "\"\"",
            "Visible\n<think>unterminated",
        ] {
            assert_eq!(sanitize_generated_title(raw), None, "raw: {raw:?}");
        }
    }

    #[test]
    fn sanitizer_caps_long_titles() {
        let title = sanitize_generated_title(
            "One two three four five six seven eight nine ten eleven twelve.",
        )
        .unwrap();
        assert_eq!(title, "One two three four five six seven eight");

        let title =
            sanitize_generated_title(&"word ".repeat(3).replace("word", &"x".repeat(40))).unwrap();
        assert!(title.chars().count() <= TITLE_MAX_CHARS);
        assert_eq!(title.split_whitespace().count(), 1);

        let title = sanitize_generated_title(&"é".repeat(200)).unwrap();
        assert_eq!(title.chars().count(), TITLE_MAX_CHARS);

        let title =
            sanitize_generated_title("Alpha, beta, gamma, delta, epsilon, zeta, eta, theta, iota")
                .unwrap();
        assert_eq!(
            title,
            "Alpha, beta, gamma, delta, epsilon, zeta, eta, theta"
        );
    }

    #[test]
    fn echo_guard_flags_copied_example_title_only() {
        let transcript = "Alice: let's go over the hiring pipeline for Q3.";
        assert!(is_prompt_echo_title("Roadmap Sync", transcript));
        assert!(is_prompt_echo_title("  roadmap   SYNC ", transcript));
        assert!(is_prompt_echo_title("Roadmap Sync.", transcript));

        assert!(!is_prompt_echo_title(
            "Roadmap Sync",
            "Bob: the ROADMAP for next quarter is ready."
        ));
        assert!(!is_prompt_echo_title("Hiring Pipeline Review", transcript));
        assert!(!is_prompt_echo_title("Roadmap Sync Followup", transcript));

        assert!(is_prompt_echo_title("Office Move Logistics", transcript));
        assert!(!is_prompt_echo_title(
            "Office Move Logistics",
            "Others: the office move is on Monday."
        ));
    }

    #[test]
    fn chunk_retries_once_unless_cancelled() {
        assert!(should_retry_chunk_failure(1, false));
        assert!(!should_retry_chunk_failure(2, false));
        assert!(!should_retry_chunk_failure(1, true));
    }

    #[test]
    fn chunk_by_chars_prefers_transcript_line_boundaries() {
        let text = "[00:01] [You] first line here\n[00:05] [Others] second line here\n";
        let chunks = chunk_text_by_chars(text, 40, 0);
        assert_eq!(chunks[0], "[00:01] [You] first line here\n");
        assert_eq!(chunks.concat(), text);
    }

    // Pipeline tests against a scripted backend -----------------------------

    type Responder = dyn Fn(usize, &str, &str, &GenerationOptions) -> Result<LlmCompletion, LlmError>
        + Send
        + Sync;

    /// Counts one token per character so budgets are easy to reason about.
    struct FakeBackend {
        provider: LLMProvider,
        respond: Box<Responder>,
        calls: std::sync::Mutex<Vec<(String, String, GenerationOptions)>>,
    }

    impl FakeBackend {
        fn new(
            provider: LLMProvider,
            respond: impl Fn(usize, &str, &str, &GenerationOptions) -> Result<LlmCompletion, LlmError>
                + Send
                + Sync
                + 'static,
        ) -> Self {
            Self {
                provider,
                respond: Box::new(respond),
                calls: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn calls(&self) -> Vec<(String, String, GenerationOptions)> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl SummaryBackend for FakeBackend {
        async fn generate(
            &self,
            system_prompt: &str,
            user_prompt: &str,
            options: &GenerationOptions,
        ) -> Result<LlmCompletion, LlmError> {
            let index = {
                let mut calls = self.calls.lock().unwrap();
                calls.push((
                    system_prompt.to_string(),
                    user_prompt.to_string(),
                    options.clone(),
                ));
                calls.len() - 1
            };
            (self.respond)(index, system_prompt, user_prompt, options)
        }

        async fn count_tokens(&self, text: &str) -> Result<usize, String> {
            Ok(text.chars().count())
        }

        async fn count_prompt_tokens(
            &self,
            system_prompt: &str,
            user_prompt: &str,
        ) -> Result<usize, String> {
            Ok(system_prompt.chars().count() + user_prompt.chars().count())
        }

        fn provider(&self) -> &LLMProvider {
            &self.provider
        }

        fn is_cancelled(&self) -> bool {
            false
        }
    }

    fn reply(content: &str) -> Result<LlmCompletion, LlmError> {
        Ok(LlmCompletion {
            content: content.to_string(),
            reasoning_stripped: false,
            truncated: false,
            prompt_tokens: None,
        })
    }

    fn truncated(content: &str) -> Result<LlmCompletion, LlmError> {
        Ok(LlmCompletion {
            content: content.to_string(),
            reasoning_stripped: false,
            truncated: true,
            prompt_tokens: None,
        })
    }

    async fn run(
        backend: &FakeBackend,
        text: &str,
        budget: Option<ContextBudget>,
        detected_language: Option<&str>,
    ) -> Result<GeneratedMeetingSummary, String> {
        run_with_prompt(
            backend,
            text,
            "",
            &resolve_summary_system_prompt(None, None, ""),
            budget,
            detected_language,
        )
        .await
    }

    async fn run_with_prompt(
        backend: &FakeBackend,
        text: &str,
        notes: &str,
        prompt: &SummaryPrompt,
        budget: Option<ContextBudget>,
        detected_language: Option<&str>,
    ) -> Result<GeneratedMeetingSummary, String> {
        generate_meeting_summary(
            backend,
            "model",
            text,
            notes,
            prompt,
            budget,
            &GenerationOptions::default(),
            None,
            detected_language,
            None,
            None,
        )
        .await
    }

    #[tokio::test]
    async fn transcript_that_fits_is_summarised_in_one_budgeted_call() {
        let backend = FakeBackend::new(LLMProvider::BuiltInAI, |_, _, _, _| reply("# Notes"));
        let generated = run(
            &backend,
            "[00:01] [You] hello",
            Some(ContextBudget::new(32_768)),
            None,
        )
        .await
        .unwrap();
        assert_eq!(generated.final_markdown, "# Notes");
        let calls = backend.calls();
        assert_eq!(
            calls.len(),
            1,
            "unknown language must not add a normalization pass"
        );
        assert!(calls[0]
            .1
            .contains("<transcript>\n[00:01] [You] hello\n</transcript>"));
        assert_eq!(calls[0].2.max_tokens, Some(4_096));
        assert_eq!(calls[0].2.num_ctx, None);
    }

    #[tokio::test]
    async fn ollama_calls_carry_num_ctx_matching_the_budget() {
        let backend = FakeBackend::new(LLMProvider::Ollama, |_, _, _, _| reply("# Notes"));
        run(
            &backend,
            "short",
            Some(ContextBudget::for_ollama(131_072)),
            None,
        )
        .await
        .unwrap();
        let options = &backend.calls()[0].2;
        assert_eq!(options.num_ctx, Some(16_384));
        assert_eq!(options.max_tokens, Some(4_096));
    }

    #[tokio::test]
    async fn truncated_reply_is_retried_with_a_larger_output_budget() {
        let backend = FakeBackend::new(LLMProvider::BuiltInAI, |index, _, _, _| {
            if index == 0 {
                truncated("# Notes\n- half")
            } else {
                reply("# Notes\n- complete")
            }
        });
        let generated = run(&backend, "short", Some(ContextBudget::new(32_768)), None)
            .await
            .unwrap();
        assert_eq!(generated.final_markdown, "# Notes\n- complete");
        let calls = backend.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1].2.max_tokens, Some(8_192));
        assert_eq!(calls[0].1, calls[1].1);
    }

    #[tokio::test]
    async fn grown_retry_fits_even_when_ollama_reports_a_cached_prompt() {
        // Regression: Ollama's prompt_eval_count leaves out a reused prefix,
        // so a regenerate reported a few tokens and the grown retry asked for
        // prompt + num_predict > num_ctx.
        let budget = ContextBudget::for_ollama(8_192);
        let backend = FakeBackend::new(LLMProvider::Ollama, |index, _, _, _| {
            if index == 0 {
                Ok(LlmCompletion {
                    content: "# Notes\n- half".to_string(),
                    reasoning_stripped: false,
                    truncated: true,
                    prompt_tokens: Some(12),
                })
            } else {
                reply("# Notes\n- complete")
            }
        });
        let text = "x".repeat(2_400);
        run(&backend, &text, Some(budget), None).await.unwrap();
        let calls = backend.calls();
        assert_eq!(calls.len(), 2, "one chunk, one grown retry");
        let (system, user, options) = &calls[1];
        let prompt = system.chars().count() + user.chars().count();
        let output = options.max_tokens.unwrap() as usize;
        assert!(
            prompt + output + budget.safety_margin() <= budget.n_ctx,
            "prompt {prompt} + output {output} must fit num_ctx {}",
            budget.n_ctx
        );
    }

    #[tokio::test]
    async fn repeated_truncation_fails_instead_of_saving_partial_notes() {
        let backend = FakeBackend::new(LLMProvider::BuiltInAI, |_, _, _, _| {
            truncated("# Notes\n- half")
        });
        let error = run(&backend, "short", Some(ContextBudget::new(32_768)), None)
            .await
            .unwrap_err();
        assert!(error.contains("cut off"), "{error}");
        assert_eq!(backend.calls().len(), 2);
    }

    #[tokio::test]
    async fn hosted_truncation_fails_without_a_blind_retry() {
        let backend = FakeBackend::new(LLMProvider::Claude, |_, _, _, _| truncated("# Notes"));
        let error = run(&backend, "short", None, None).await.unwrap_err();
        assert!(error.contains("cut off"), "{error}");
        assert_eq!(backend.calls().len(), 1);
    }

    #[tokio::test]
    async fn long_transcripts_are_chunked_within_the_context_budget() {
        let budget = ContextBudget::new(4_096);
        let backend = FakeBackend::new(LLMProvider::BuiltInAI, |_, system, _, _| {
            if system == COMBINE_SYSTEM_PROMPT {
                reply("combined summary")
            } else {
                reply("chunk summary")
            }
        });
        let line = "[00:01] [Others] we discussed the quarterly plan in detail today.\n";
        let text = line.repeat(120); // ~8k tokens at one token per char
        let generated = run(&backend, &text, Some(budget), None).await.unwrap();
        assert!(generated.successful_chunk_count > 2);

        let window = budget.n_ctx - budget.max_output;
        for (system, user, _) in backend.calls() {
            let prompt_tokens = system.chars().count() + user.chars().count();
            assert!(prompt_tokens <= window, "{prompt_tokens} > {window}");
        }
        let calls = backend.calls();
        let final_call = calls.last().unwrap();
        assert!(final_call.0.contains("`## Summary`"));
        assert!(final_call.1.contains("combined summary"));
    }

    mod from_live_notes {
        use super::*;
        use crate::summary::live_outline::{build_outline, section_system_prompt, LiveNote};

        fn note(min: i64, kind: &str, text: &str, topic: &str) -> LiveNote {
            let speaker = (kind == "action").then_some("You");
            LiveNote::new(
                min * 60_000,
                min * 60_000 + 20_000,
                kind,
                text,
                speaker,
                Some(topic),
            )
        }

        /// Ten minutes of transcript, one line every 30 s, `filler` chars long.
        fn transcript(filler: usize) -> String {
            (0..20)
                .map(|i| {
                    let said = if i == 12 {
                        "I will draft the role by Monday "
                    } else {
                        ""
                    };
                    format!(
                        "[{:02}:{:02}] [Others] line {i} {said}{}",
                        i / 2,
                        (i % 2) * 30,
                        "x".repeat(filler)
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        }

        fn notes() -> Vec<LiveNote> {
            vec![
                note(0, "point", "Pricing at forty per seat", "Pricing"),
                note(2, "decision", "Annual plan ships first", "Pricing"),
                note(5, "point", "Two engineers to hire", "Hiring"),
                note(7, "action", "post the job ads", "Hiring"),
            ]
        }

        async fn run_outline(
            backend: &FakeBackend,
            text: &str,
            user_notes: &str,
            budget: Option<ContextBudget>,
        ) -> Result<GeneratedMeetingSummary, String> {
            let outline = build_outline(&notes(), text, user_notes).unwrap();
            generate_meeting_summary(
                backend,
                "model",
                text,
                user_notes,
                &resolve_summary_system_prompt(None, None, user_notes),
                budget,
                &GenerationOptions::default(),
                None,
                Some("en"),
                None,
                Some(&outline),
            )
            .await
        }

        fn section_calls(backend: &FakeBackend) -> Vec<String> {
            backend
                .calls()
                .into_iter()
                .filter(|(system, _, _)| *system == section_system_prompt())
                .map(|(_, user, _)| user)
                .collect()
        }

        #[tokio::test]
        async fn each_topic_is_written_from_its_own_transcript_slice() {
            let backend = FakeBackend::new(LLMProvider::Claude, |_, system, user, _| {
                if system == title_system_prompt() {
                    return reply("Pricing and Hiring Plan");
                }
                if user.contains("Part of the meeting: Pricing") {
                    reply("- Forty dollars per seat [00:30]\nQuestion: Monthly price? [01:00]")
                } else {
                    reply("- Two engineers, backend first [05:00]\nAction: Priya — draft the role (due: Monday) [06:00]")
                }
            });
            let generated = run_outline(&backend, &transcript(10), "", None)
                .await
                .unwrap();

            let sections = section_calls(&backend);
            assert_eq!(sections.len(), 2, "one call per topic");
            assert!(sections[0].contains("[04:30] [Others] line 9"));
            assert!(!sections[0].contains("[05:00] [Others] line 10"));
            assert!(sections[1].contains("[05:00] [Others] line 10"));
            assert!(!sections[1].contains("[04:30] [Others]"));
            assert!(sections[0].contains("DECISION: Annual plan ships first"));
            assert!(sections[1].contains("ACTION (You): post the job ads"));
            assert_eq!(
                generated.final_markdown,
                "# Pricing and Hiring Plan\n\n## Pricing\n\n- Forty dollars per seat [00:30]\n\n## Hiring\n\n- Two engineers, backend first [05:00]\n\n## Decisions\n\n- Annual plan ships first [02:00]\n\n## Action items\n\n- **You** — post the job ads [07:00]\n- **Priya** — draft the role (due: Monday) [06:00]\n\n## Open questions\n\n- Monthly price? [01:00]\n"
            );
            // One title call plus one per section.
            assert_eq!(generated.successful_chunk_count, 3);
        }

        #[tokio::test]
        async fn user_notes_reach_their_topic_and_survive_a_model_that_drops_them() {
            let backend = FakeBackend::new(LLMProvider::Claude, |_, system, _, _| {
                if system == title_system_prompt() {
                    reply("Planning")
                } else {
                    reply("- Something said [00:30]")
                }
            });
            let generated = run_outline(
                &backend,
                &transcript(10),
                "[06:10]\nbackend or frontend first?",
                None,
            )
            .await
            .unwrap();
            let sections = section_calls(&backend);
            assert!(!sections[0].contains("<my_notes>"));
            assert!(sections[1].contains("<my_notes>\n- backend or frontend first?\n</my_notes>"));
            assert!(generated
                .final_markdown
                .contains("## Hiring\n\n- backend or frontend first?\n- Something said\n"));
            // The Hiring copy of the bullet lost its link: 00:30 is not in Hiring.
            assert!(generated
                .final_markdown
                .contains("## Pricing\n\n- Something said [00:30]"));
        }

        #[tokio::test]
        async fn a_failed_section_keeps_its_live_notes() {
            let backend = FakeBackend::new(LLMProvider::Claude, |_, system, user, _| {
                if system == title_system_prompt() {
                    reply("Planning")
                } else if user.contains("Part of the meeting: Hiring") {
                    Err(LlmError::new(LlmErrorKind::Other, "server error"))
                } else {
                    reply("- Pricing bullet [00:30]")
                }
            });
            let generated = run_outline(&backend, &transcript(10), "", None)
                .await
                .unwrap();
            assert!(generated.final_markdown.contains(
                "## Hiring\n\n- Two engineers to hire [05:00]\n- post the job ads [07:00]"
            ));
        }

        #[tokio::test]
        async fn a_section_too_long_for_the_context_is_written_in_pieces() {
            let budget = ContextBudget::new(8_000);
            let backend = FakeBackend::new(LLMProvider::BuiltInAI, |_, system, user, _| {
                if system == title_system_prompt() {
                    reply("Planning")
                } else if system == section_system_prompt() && user.chars().count() > 5_500 {
                    Err(LlmError::new(
                        LlmErrorKind::ContextOverflow,
                        "context overflow",
                    ))
                } else {
                    reply("- A bullet")
                }
            });
            run_outline(&backend, &transcript(400), "", Some(budget))
                .await
                .unwrap();
            let sections = section_calls(&backend);
            let window = budget.n_ctx - budget.max_output.min(1_536);
            for (system, user, _) in backend.calls() {
                if system == section_system_prompt() {
                    assert!(system.chars().count() + user.chars().count() <= window);
                }
            }
            // Pricing (10 lines of ~420 chars) does not fit one call.
            let pricing = sections
                .iter()
                .filter(|user| user.contains("Part of the meeting: Pricing"))
                .count();
            assert!(pricing >= 2, "{pricing} pricing calls");
            // Every transcript line is sent exactly once, overflowed calls aside.
            for i in 0..20 {
                let marker = format!("] [Others] line {i} ");
                let sent = sections
                    .iter()
                    .filter(|user| user.chars().count() <= 5_500 && user.contains(&marker))
                    .count();
                assert_eq!(sent, 1, "line {i} sent {sent} times");
            }
        }
    }

    #[tokio::test]
    async fn context_overflow_resplits_the_chunk_instead_of_resending_it() {
        let budget = ContextBudget::new(4_096);
        // The "model" can only take chunk prompts below 1,800 characters.
        let backend = FakeBackend::new(LLMProvider::BuiltInAI, |_, system, user, _| {
            if system == chunk_system_prompt() && user.chars().count() > 1_800 {
                Err(LlmError::new(
                    LlmErrorKind::ContextOverflow,
                    "Generation failed: context overflow: prompt has 3000 tokens",
                ))
            } else {
                reply("summary")
            }
        });
        // Unique lines, so identical prompts can only come from a resend.
        let text: String = (0..200)
            .map(|i| {
                format!(
                    "[{:02}:{:02}] [You] item {i}: plan the launch.\n",
                    i / 60,
                    i % 60
                )
            })
            .collect();
        run(&backend, &text, Some(budget), None).await.unwrap();

        let chunk_prompts: Vec<String> = backend
            .calls()
            .into_iter()
            .filter(|(system, _, _)| *system == chunk_system_prompt())
            .map(|(_, user, _)| user)
            .collect();
        let overflowed: Vec<&String> = chunk_prompts
            .iter()
            .filter(|user| user.chars().count() > 1_800)
            .collect();
        assert!(!overflowed.is_empty(), "test should exercise an overflow");
        for user in overflowed {
            assert_eq!(
                chunk_prompts.iter().filter(|other| *other == user).count(),
                1,
                "an overflowing chunk was resent unchanged"
            );
        }
        assert!(chunk_prompts
            .iter()
            .any(|user| user.chars().count() <= 1_800));
    }

    #[tokio::test]
    async fn a_truncated_chunk_fails_without_resplitting() {
        let budget = ContextBudget::new(4_096);
        // The truncation message mentions "context window"; it must still
        // not be treated as an overflow.
        let backend = FakeBackend::new(LLMProvider::BuiltInAI, |_, system, _, _| {
            if system == chunk_system_prompt() {
                truncated("partial")
            } else {
                reply("summary")
            }
        });
        let line = "[00:01] [Others] we discussed the quarterly plan in detail today.\n";
        let error = run(&backend, &line.repeat(120), Some(budget), None)
            .await
            .unwrap_err();
        assert!(error.contains("cut off"), "{error}");
        assert!(error.contains("context window"), "{error}");
        let chunk_calls = backend
            .calls()
            .into_iter()
            .filter(|(system, _, _)| *system == chunk_system_prompt())
            .count();
        // One chunk: the first try plus its larger-budget retry, no splits.
        assert_eq!(chunk_calls, 2);
    }

    #[tokio::test]
    async fn a_failed_chunk_is_retried_with_a_new_seed() {
        let budget = ContextBudget::new(4_096);
        let backend = FakeBackend::new(LLMProvider::BuiltInAI, |index, system, _, _| {
            if system == chunk_system_prompt() && index == 0 {
                Err(LlmError::from("connection reset"))
            } else {
                reply("summary")
            }
        });
        let line = "[00:01] [Others] we discussed the quarterly plan in detail today.\n";
        run(&backend, &line.repeat(120), Some(budget), None)
            .await
            .unwrap();
        let calls = backend.calls();
        assert_eq!(calls[0].1, calls[1].1, "the same chunk was retried");
        assert_eq!(calls[0].2.seed, None, "the first try keeps the fixed seed");
        assert_eq!(calls[1].2.seed, Some(SUMMARY_SEED + 1));
    }

    #[tokio::test]
    async fn placeholder_sections_are_kept_unless_the_fixed_section_default_ran() {
        let markdown = "# Plan\n\n## Blockers\n- None\n";
        let backend = FakeBackend::new(LLMProvider::Claude, move |_, _, _, _| reply(markdown));
        let notes = "## Blockers\n- None";
        let enriched = run_with_prompt(
            &backend,
            "short",
            notes,
            &resolve_summary_system_prompt(None, None, notes),
            None,
            None,
        )
        .await
        .unwrap();
        assert_eq!(enriched.final_markdown, markdown.trim_end());
        let custom = run_with_prompt(
            &backend,
            "short",
            "",
            &resolve_summary_system_prompt(None, Some("Mine"), ""),
            None,
            None,
        )
        .await
        .unwrap();
        assert_eq!(custom.final_markdown, markdown.trim_end());
        let fixed = run(&backend, "short", None, None).await.unwrap();
        assert_eq!(fixed.final_markdown, "# Plan");
    }

    /// Manual smoke test against a real built-in model:
    /// MEETILY_LLAMA_HELPER=<path to llama-helper> MINUTES_SMOKE_APP_DIR=<app data dir>
    /// cargo test -p meetily builtin_summary_is_deterministic -- --ignored --nocapture
    #[tokio::test]
    #[ignore = "needs a downloaded built-in model and the llama-helper binary"]
    async fn builtin_summary_is_deterministic() {
        let Ok(app_dir) = std::env::var("MINUTES_SMOKE_APP_DIR") else {
            return;
        };
        let app_dir = PathBuf::from(app_dir);
        let model = std::env::var("MINUTES_SMOKE_MODEL").unwrap_or_else(|_| "qwen3.5:4b".into());
        let client = Client::new();
        let backend = ProviderBackend {
            client: &client,
            provider: &LLMProvider::BuiltInAI,
            model_name: &model,
            api_key: "",
            ollama_endpoint: None,
            custom_openai_endpoint: None,
            app_data_dir: Some(&app_dir),
            cancellation_token: None,
        };
        let text = "[00:00] [You] Okay, let's settle the launch date for the mobile app.\n\
[00:05] [Others] Design needs one more week for the onboarding screens.\n\
[00:12] [You] Then we ship on March 14th. Priya, can you update the release checklist by Friday?\n\
[00:20] [Others] Sure. Also, do we still need the Android tablet layout?\n\
[00:26] [You] Not for launch, let's revisit in April.";
        let mut outputs = Vec::new();
        for _ in 0..2 {
            let generated = generate_meeting_summary(
                &backend,
                &model,
                text,
                "",
                &resolve_summary_system_prompt(None, None, ""),
                Some(ContextBudget::new(32_768)),
                &GenerationOptions::default(),
                None,
                Some("en"),
                None,
                None,
            )
            .await
            .unwrap();
            outputs.push(generated.final_markdown);
            // Restart the helper so the second run is a fresh process.
            crate::summary::summary_engine::force_shutdown_sidecar()
                .await
                .unwrap();
        }
        println!("{}", outputs[0]);
        assert_eq!(outputs[0], outputs[1]);
    }

    #[tokio::test]
    async fn confidently_non_english_transcript_gets_one_normalization_pass() {
        let backend = FakeBackend::new(LLMProvider::BuiltInAI, |index, _, _, _| {
            reply(if index == 0 { "# Notizen" } else { "# Notes" })
        });
        let generated = run(
            &backend,
            "kurz",
            Some(ContextBudget::new(32_768)),
            Some("de"),
        )
        .await
        .unwrap();
        assert_eq!(generated.final_markdown, "# Notes");
        assert_eq!(backend.calls().len(), 2);
    }

    // resolve_cached_english matrix -------------------------------------------

    #[test]
    fn no_cache_no_language_returns_none() {
        assert_eq!(resolve_cached_english(None, None), None);
    }

    #[test]
    fn empty_cache_with_translation_target_returns_none() {
        assert_eq!(resolve_cached_english(Some(""), Some("fr")), None);
    }

    #[test]
    fn whitespace_only_cache_returns_none() {
        assert_eq!(resolve_cached_english(Some("   \n"), Some("fr")), None);
    }

    #[test]
    fn valid_cache_no_language_returns_none() {
        assert_eq!(resolve_cached_english(Some("body"), None), None);
    }

    #[test]
    fn valid_cache_english_target_returns_none() {
        assert_eq!(resolve_cached_english(Some("body"), Some("en")), None);
    }

    #[test]
    fn valid_cache_english_variant_returns_none() {
        // "en-GB" normalises to English — cache should not be used (re-run pass 1)
        assert_eq!(resolve_cached_english(Some("body"), Some("en-GB")), None);
    }

    #[test]
    fn valid_cache_french_target_returns_cache() {
        assert_eq!(
            resolve_cached_english(Some("body"), Some("fr")),
            Some("body")
        );
    }

    #[test]
    fn valid_cache_unknown_language_returns_none() {
        // Unknown code -> language_name_from_code returns None -> not a translation
        assert_eq!(
            resolve_cached_english(Some("body"), Some("zz-unknown")),
            None
        );
    }

    #[test]
    fn uppercase_translation_code_returns_cache() {
        assert_eq!(
            resolve_cached_english(Some("body"), Some("FR")),
            Some("body")
        );
    }

    #[test]
    fn uppercase_english_code_returns_none() {
        assert_eq!(resolve_cached_english(Some("body"), Some("EN")), None);
    }

    #[test]
    fn underscore_locale_variant_returns_none() {
        // OS locale APIs (notably macOS) may emit "en_GB" with underscore.
        assert_eq!(resolve_cached_english(Some("body"), Some("en_GB")), None);
    }

    #[test]
    fn cleaner_removes_closed_reasoning_envelopes_everywhere() {
        let cleaned = clean_llm_markdown_detailed(
            "Intro\n<think>private</think>\n# Meeting\nHello\n<thinking>also private</thinking>\nTail",
        );
        assert!(cleaned.reasoning_stripped);
        assert!(!cleaned.markdown.contains("<think"));
        assert!(!cleaned.markdown.contains("<thinking"));
        assert!(cleaned.markdown.contains("Intro"));
        assert!(cleaned.markdown.contains("# Meeting"));
        assert!(cleaned.markdown.contains("Tail"));

        let fenced = clean_llm_markdown_detailed("```\n<think>private</think>\nvisible\n```");
        assert_eq!(fenced.markdown, "visible");
        assert!(fenced.reasoning_stripped);

        let attributed = clean_llm_markdown_detailed(
            "Visible\n<thinking class=\"internal\">private</thinking>\nTail",
        );
        assert!(attributed.reasoning_stripped);
        assert!(!attributed.markdown.contains("private"));
        assert!(attributed.markdown.contains("Visible"));
        assert!(attributed.markdown.contains("Tail"));

        let literal = clean_llm_markdown_detailed("<thinker>Visible</thinker>");
        assert!(!literal.reasoning_stripped);
        assert_eq!(literal.markdown, "<thinker>Visible</thinker>");
    }

    #[test]
    fn cleaner_rejects_unterminated_reasoning_markers() {
        for raw in [
            "Visible\n<think>private",
            "Visible\n</thinking>",
            "Visible\n<think class=\"internal\">private",
        ] {
            let cleaned = clean_llm_markdown_detailed(raw);
            assert_eq!(
                require_visible_markdown("Final summary", &cleaned),
                Err("Final summary contained an unterminated reasoning marker".to_string())
            );
        }
    }

    #[test]
    fn reasoning_only_and_empty_fences_fail_visible_content_guard() {
        let reasoning_only = clean_llm_markdown_detailed("<think>private</think>");
        assert_eq!(
            require_visible_markdown("Final summary", &reasoning_only),
            Err(
                "Final summary returned no visible summary content after reasoning removal"
                    .to_string()
            )
        );
        for raw in [
            "```\n```",
            "```markdown\n```",
            "```\r\n```",
            "```markdown\r\n```",
        ] {
            let empty_fence = clean_llm_markdown_detailed(raw);
            assert_eq!(empty_fence.markdown, "");
            assert_eq!(
                require_visible_markdown("Translation", &empty_fence),
                Err(
                    "Translation returned no visible summary content after reasoning removal"
                        .to_string()
                )
            );
        }
    }

    #[test]
    fn cleaner_strips_outer_crlf_fence_without_normalizing_inner_markdown() {
        let markdown = "# Heading\r\n\r\n```rust\r\nlet x = 1;\r\n```";
        let wrapped = format!("```\r\n{markdown}\r\n```");
        let cleaned = clean_llm_markdown_detailed(&wrapped);
        assert_eq!(cleaned.markdown, markdown);
        assert!(!cleaned.reasoning_stripped);
        assert_eq!(clean_llm_markdown_detailed(markdown).markdown, markdown);
    }
}
