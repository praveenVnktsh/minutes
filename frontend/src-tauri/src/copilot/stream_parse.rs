//! Incremental parsers for the providers' streaming formats. Pure: bytes in,
//! events out, so each format is tested against recorded fixtures.

use serde_json::Value;

/// What one streamed event means for the answer.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamItem {
    /// More answer text.
    Delta(String),
    /// The provider stopped; `truncated` when it hit the output limit.
    Finish { truncated: bool },
    /// Prompt size as the provider counted it.
    PromptTokens(u32),
    /// Completion size as the provider counted it.
    CompletionTokens(u32),
    /// The provider reported an error inside the stream.
    Error(String),
    /// The stream is over.
    Done,
}

/// Splits a byte stream into lines. Bytes are kept until a full line arrives,
/// so a multi-byte character split across chunks is never mangled.
#[derive(Debug, Default)]
pub struct LineBuffer {
    pending: Vec<u8>,
}

impl LineBuffer {
    pub fn push(&mut self, bytes: &[u8]) -> Vec<String> {
        self.pending.extend_from_slice(bytes);
        let mut lines = Vec::new();
        while let Some(at) = self.pending.iter().position(|b| *b == b'\n') {
            let mut line: Vec<u8> = self.pending.drain(..=at).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            lines.push(String::from_utf8_lossy(&line).into_owned());
        }
        lines
    }

    /// Whatever is left when the stream ends without a final newline.
    pub fn finish(&mut self) -> Option<String> {
        if self.pending.is_empty() {
            return None;
        }
        let line = String::from_utf8_lossy(&self.pending).trim_end().to_string();
        self.pending.clear();
        (!line.is_empty()).then_some(line)
    }
}

/// One server-sent event.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

/// Server-sent events decoder (the subset the LLM APIs use).
#[derive(Debug, Default)]
pub struct SseDecoder {
    lines: LineBuffer,
    event: Option<String>,
    data: Vec<String>,
}

impl SseDecoder {
    pub fn push(&mut self, bytes: &[u8]) -> Vec<SseEvent> {
        let lines = self.lines.push(bytes);
        let mut events = Vec::new();
        for line in lines {
            self.line(line, &mut events);
        }
        events
    }

    pub fn finish(&mut self) -> Vec<SseEvent> {
        let mut events = Vec::new();
        if let Some(line) = self.lines.finish() {
            self.line(line, &mut events);
        }
        self.dispatch(&mut events);
        events
    }

    fn line(&mut self, line: String, events: &mut Vec<SseEvent>) {
        if line.is_empty() {
            self.dispatch(events);
        } else if line.starts_with(':') {
            // Comment / keep-alive.
        } else if let Some(value) = field(&line, "data") {
            self.data.push(value.to_string());
        } else if let Some(value) = field(&line, "event") {
            self.event = Some(value.to_string());
        }
    }

    fn dispatch(&mut self, events: &mut Vec<SseEvent>) {
        if self.data.is_empty() {
            self.event = None;
            return;
        }
        events.push(SseEvent {
            event: self.event.take(),
            data: self.data.join("\n"),
        });
        self.data.clear();
    }
}

fn field<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let rest = line.strip_prefix(name)?.strip_prefix(':')?;
    Some(rest.strip_prefix(' ').unwrap_or(rest))
}

fn error_text(value: &Value) -> Option<String> {
    let error = value.get("error")?;
    Some(
        error
            .get("message")
            .and_then(Value::as_str)
            .or_else(|| error.as_str())
            .unwrap_or("The provider reported an error")
            .to_string(),
    )
}

/// OpenAI-compatible `chat/completions` streaming (OpenAI, Groq, OpenRouter,
/// custom endpoints).
pub fn openai_event(event: &SseEvent) -> Vec<StreamItem> {
    let data = event.data.trim();
    if data == "[DONE]" {
        return vec![StreamItem::Done];
    }
    let Ok(value) = serde_json::from_str::<Value>(data) else {
        return Vec::new();
    };
    if let Some(error) = error_text(&value) {
        return vec![StreamItem::Error(error)];
    }
    let mut items = Vec::new();
    if let Some(choice) = value.get("choices").and_then(|c| c.get(0)) {
        if let Some(text) = choice
            .get("delta")
            .and_then(|d| d.get("content"))
            .and_then(Value::as_str)
        {
            if !text.is_empty() {
                items.push(StreamItem::Delta(text.to_string()));
            }
        }
        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            items.push(StreamItem::Finish {
                truncated: reason == "length",
            });
        }
    }
    if let Some(usage) = value.get("usage").filter(|u| !u.is_null()) {
        if let Some(prompt) = usage.get("prompt_tokens").and_then(Value::as_u64) {
            items.push(StreamItem::PromptTokens(prompt as u32));
        }
        if let Some(completion) = usage.get("completion_tokens").and_then(Value::as_u64) {
            items.push(StreamItem::CompletionTokens(completion as u32));
        }
    }
    items
}

/// Anthropic Messages API streaming.
pub fn claude_event(event: &SseEvent) -> Vec<StreamItem> {
    let Ok(value) = serde_json::from_str::<Value>(event.data.trim()) else {
        return Vec::new();
    };
    match value.get("type").and_then(Value::as_str) {
        Some("content_block_delta") => {
            let delta = value.get("delta");
            match delta.and_then(|d| d.get("type")).and_then(Value::as_str) {
                Some("text_delta") => delta
                    .and_then(|d| d.get("text"))
                    .and_then(Value::as_str)
                    .filter(|t| !t.is_empty())
                    .map(|t| vec![StreamItem::Delta(t.to_string())])
                    .unwrap_or_default(),
                _ => Vec::new(), // thinking_delta, signature_delta, …
            }
        }
        Some("message_start") => value
            .pointer("/message/usage/input_tokens")
            .and_then(Value::as_u64)
            .map(|n| vec![StreamItem::PromptTokens(n as u32)])
            .unwrap_or_default(),
        Some("message_delta") => {
            let mut items = Vec::new();
            if let Some(reason) = value.pointer("/delta/stop_reason").and_then(Value::as_str) {
                items.push(StreamItem::Finish {
                    truncated: matches!(reason, "max_tokens" | "model_context_window_exceeded"),
                });
            }
            if let Some(n) = value
                .pointer("/usage/output_tokens")
                .and_then(Value::as_u64)
            {
                items.push(StreamItem::CompletionTokens(n as u32));
            }
            items
        }
        Some("message_stop") => vec![StreamItem::Done],
        Some("error") => vec![StreamItem::Error(
            error_text(&value).unwrap_or_else(|| "Claude reported an error".into()),
        )],
        _ => Vec::new(),
    }
}

/// Ollama `/api/chat` streaming: one JSON object per line.
pub fn ollama_line(line: &str) -> Vec<StreamItem> {
    let Ok(value) = serde_json::from_str::<Value>(line.trim()) else {
        return Vec::new();
    };
    if let Some(error) = error_text(&value) {
        return vec![StreamItem::Error(error)];
    }
    let mut items = Vec::new();
    if let Some(text) = value
        .pointer("/message/content")
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
    {
        items.push(StreamItem::Delta(text.to_string()));
    }
    if value.get("done").and_then(Value::as_bool) == Some(true) {
        if let Some(n) = value.get("prompt_eval_count").and_then(Value::as_u64) {
            items.push(StreamItem::PromptTokens(n as u32));
        }
        if let Some(n) = value.get("eval_count").and_then(Value::as_u64) {
            items.push(StreamItem::CompletionTokens(n as u32));
        }
        items.push(StreamItem::Finish {
            truncated: value.get("done_reason").and_then(Value::as_str) == Some("length"),
        });
        items.push(StreamItem::Done);
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deltas(items: &[StreamItem]) -> String {
        items
            .iter()
            .filter_map(|item| match item {
                StreamItem::Delta(text) => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    /// Feeds `bytes` in awkward chunk sizes, splitting lines and characters.
    fn chunked(bytes: &[u8], size: usize) -> Vec<&[u8]> {
        bytes.chunks(size).collect()
    }

    const OPENAI_FIXTURE: &str = concat!(
        ": keep-alive\n\n",
        "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Say \"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"we ship Friday – café [12:04].\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":null}\n\n",
        "data: {\"id\":\"c1\",\"choices\":[],\"usage\":{\"prompt_tokens\":1500,\"completion_tokens\":12}}\n\n",
        "data: [DONE]\n\n",
    );

    #[test]
    fn openai_compatible_sse_streams_text_finish_and_usage() {
        for size in [1, 3, 7, 64, 4096] {
            let mut decoder = SseDecoder::default();
            let mut items = Vec::new();
            for chunk in chunked(OPENAI_FIXTURE.as_bytes(), size) {
                for event in decoder.push(chunk) {
                    items.extend(openai_event(&event));
                }
            }
            for event in decoder.finish() {
                items.extend(openai_event(&event));
            }
            assert_eq!(deltas(&items), "Say we ship Friday – café [12:04].", "chunk {size}");
            assert!(items.contains(&StreamItem::Finish { truncated: false }));
            assert!(items.contains(&StreamItem::PromptTokens(1500)));
            assert_eq!(items.last(), Some(&StreamItem::Done));
        }
    }

    #[test]
    fn openai_errors_and_length_stops_are_reported() {
        let error = SseEvent {
            event: None,
            data: r#"{"error":{"message":"Rate limit reached","type":"rate_limit"}}"#.into(),
        };
        assert_eq!(
            openai_event(&error),
            vec![StreamItem::Error("Rate limit reached".into())]
        );
        let cut = SseEvent {
            event: None,
            data: r#"{"choices":[{"delta":{"content":"x"},"finish_reason":"length"}]}"#.into(),
        };
        assert_eq!(
            openai_event(&cut),
            vec![
                StreamItem::Delta("x".into()),
                StreamItem::Finish { truncated: true }
            ]
        );
    }

    const CLAUDE_FIXTURE: &str = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\",\"message\":{\"id\":\"m\",\"usage\":{\"input_tokens\":2100,\"output_tokens\":1}}}\n\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: ping\n",
        "data: {\"type\": \"ping\"}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"hmm\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"It hasn't come up \"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"in the meeting so far.\"}}\n\n",
        "event: message_delta\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":11}}\n\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );

    #[test]
    fn claude_sse_streams_text_only_and_reports_usage() {
        for size in [1, 5, 4096] {
            let mut decoder = SseDecoder::default();
            let mut items = Vec::new();
            for chunk in chunked(CLAUDE_FIXTURE.as_bytes(), size) {
                for event in decoder.push(chunk) {
                    items.extend(claude_event(&event));
                }
            }
            assert_eq!(deltas(&items), "It hasn't come up in the meeting so far.");
            assert!(items.contains(&StreamItem::PromptTokens(2100)));
            assert!(items.contains(&StreamItem::CompletionTokens(11)));
            assert!(items.contains(&StreamItem::Finish { truncated: false }));
            assert_eq!(items.last(), Some(&StreamItem::Done));
        }
        let overloaded = SseEvent {
            event: Some("error".into()),
            data: r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#
                .into(),
        };
        assert_eq!(
            claude_event(&overloaded),
            vec![StreamItem::Error("Overloaded".into())]
        );
    }

    const OLLAMA_FIXTURE: &str = concat!(
        "{\"model\":\"qwen3\",\"message\":{\"role\":\"assistant\",\"content\":\"Ask \"},\"done\":false}\n",
        "{\"model\":\"qwen3\",\"message\":{\"role\":\"assistant\",\"content\":\"about the budget [03:10].\"},\"done\":false}\n",
        "{\"model\":\"qwen3\",\"message\":{\"role\":\"assistant\",\"content\":\"\"},\"done\":true,\"done_reason\":\"stop\",\"prompt_eval_count\":900,\"eval_count\":9}\n",
    );

    #[test]
    fn ollama_ndjson_streams_text_and_final_stats() {
        for size in [1, 9, 4096] {
            let mut buffer = LineBuffer::default();
            let mut items = Vec::new();
            for chunk in chunked(OLLAMA_FIXTURE.as_bytes(), size) {
                for line in buffer.push(chunk) {
                    items.extend(ollama_line(&line));
                }
            }
            assert_eq!(deltas(&items), "Ask about the budget [03:10].");
            assert!(items.contains(&StreamItem::PromptTokens(900)));
            assert!(items.contains(&StreamItem::Finish { truncated: false }));
            assert_eq!(items.last(), Some(&StreamItem::Done));
        }
        assert_eq!(
            ollama_line(r#"{"error":"model 'x' not found"}"#),
            vec![StreamItem::Error("model 'x' not found".into())]
        );
    }

    #[test]
    fn a_trailing_line_without_newline_is_not_lost() {
        let mut buffer = LineBuffer::default();
        assert!(buffer.push(b"{\"done\":true}").is_empty());
        assert_eq!(buffer.finish().as_deref(), Some("{\"done\":true}"));
        let mut sse = SseDecoder::default();
        assert!(sse.push(b"data: [DONE]").is_empty());
        assert_eq!(sse.finish()[0].data, "[DONE]");
    }

    #[test]
    fn multi_line_data_fields_join_with_newlines() {
        let mut sse = SseDecoder::default();
        let events = sse.push(b"event: x\r\ndata: a\r\ndata: b\r\n\r\n");
        assert_eq!(
            events,
            vec![SseEvent {
                event: Some("x".into()),
                data: "a\nb".into()
            }]
        );
    }
}
