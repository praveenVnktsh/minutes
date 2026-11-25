//! The fine-tuned mode against the training side's own output
//! (`fixtures/finetuned/`, made by `gen_fixtures.py`).

use std::time::Duration;

use serde_json::{json, Value};

use super::*;
use crate::live_summary::scheduler::{PauseScheduler, Speaker};

fn fixture(name: &str) -> Value {
    let text = match name {
        "replay" => include_str!("../fixtures/finetuned/replay.json"),
        "compact" => include_str!("../fixtures/finetuned/compact.json"),
        "living" => include_str!("../fixtures/finetuned/living.json"),
        "pause" => include_str!("../fixtures/finetuned/pause.json"),
        "template" => include_str!("../fixtures/finetuned/template.json"),
        _ => unreachable!(),
    };
    serde_json::from_str(text).unwrap()
}

fn id_json(id: NoteId) -> Value {
    match id {
        NoteId::Ai(n) => json!(n),
        NoteId::User(_) => json!(id.to_string()),
    }
}

fn id_of(value: &Value) -> NoteId {
    match value {
        Value::String(s) => NoteId::parse(s),
        other => NoteId::Ai(other.as_u64().unwrap() as u32),
    }
}

/// An op as `compact_ops.decode` writes it.
fn op_json(op: &Op) -> Value {
    let new = |kind: &str, note: &NewNote| {
        json!({"op": kind, "id": note.id, "start": note.start, "end": note.end,
               "kind": note.kind.as_str(), "owner": note.owner, "text": note.text})
    };
    match op {
        Op::Add(note) => new("add", note),
        Op::Child { parent, note } => {
            let mut value = new("child", note);
            value["parent"] = id_json(*parent);
            value
        }
        Op::Edit {
            id,
            text,
            kind,
            end,
            owner,
        } => {
            let mut value = json!({"op": "edit", "id": id_json(*id), "text": text});
            if let Some(kind) = kind {
                value["kind"] = json!(kind.as_str());
            }
            if let Some(end) = end {
                value["end"] = json!(end);
            }
            if let Some(owner) = owner {
                value["owner"] = json!(owner);
            }
            value
        }
        Op::Drop(id) => json!({"op": "drop", "id": id_json(*id)}),
    }
}

fn decoded_json(decoded: &Decoded) -> Value {
    let topic = match &decoded.topic {
        Some(topic) => json!({"same": false, "topic": topic}),
        None => json!({"same": true}),
    };
    json!({"topic_decision": topic, "ops": decoded.ops.iter().map(op_json).collect::<Vec<_>>()})
}

fn event_of(value: &Value) -> UserEvent {
    let text = || value["text"].as_str().unwrap().to_string();
    match value["type"].as_str().unwrap() {
        "user_add" => UserEvent::Add {
            id: value["id"].as_str().unwrap()[1..].parse().unwrap(),
            t: value["t"].as_str().map(str::to_string),
            text: text(),
        },
        "user_edit" => UserEvent::Edit {
            id: id_of(&value["id"]),
            text: text(),
        },
        "user_delete" => UserEvent::Delete {
            id: id_of(&value["id"]),
        },
        other => panic!("unknown event {other}"),
    }
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|a| a.iter().map(|v| v.as_str().unwrap().to_string()).collect())
        .unwrap_or_default()
}

fn segment_of(value: &Value, received_ms: u64) -> Segment {
    Segment {
        start_ms: value["start_ms"].as_i64().unwrap(),
        end_ms: value["end_ms"].as_i64().unwrap(),
        speaker: if value["speaker"] == "You" {
            Speaker::You
        } else {
            Speaker::Others
        },
        text: value["text"].as_str().unwrap().to_string(),
        received_at: Duration::from_millis(received_ms),
    }
}

/// A fixture window's segments (`{t, speaker, text}`) as the app's.
fn view_segments(value: &Value) -> Vec<Segment> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            let ms = clock_ms(s["t"].as_str().unwrap()).unwrap();
            segment_of(
                &json!({"start_ms": ms, "end_ms": ms, "speaker": s["speaker"], "text": s["text"]}),
                0,
            )
        })
        .collect()
}

fn window_text(w: &Value) -> WindowText {
    WindowText {
        start: w["start"].as_str().unwrap().to_string(),
        end: w["end"].as_str().unwrap().to_string(),
        max_entries: w["max_entries"].as_u64().unwrap() as usize,
        transcript: w["transcript"].as_str().unwrap().to_string(),
        context: w["context"].as_str().unwrap().to_string(),
    }
}

#[test]
fn the_system_prompt_is_the_training_one() {
    assert_eq!(SYSTEM_PROMPT, fixture("replay")["system_prompt"]);
}

#[test]
fn a_gold_replay_renders_every_prompt_byte_for_byte() {
    let replay = fixture("replay");
    let mut living = Living::default();
    let (mut topic, mut since): (Option<String>, Option<String>) = (None, None);
    let mut closed: Vec<String> = Vec::new();
    for step in replay["windows"].as_array().unwrap() {
        let w = &step["window"];
        // The window's text from its segments, as the app writes it.
        assert_eq!(
            transcript_lines(&view_segments(&w["segments"])),
            w["transcript"]
        );
        assert_eq!(
            transcript_lines(&view_segments(&w["context_segments"])),
            w["context"]
        );
        for event in step["events"].as_array().unwrap() {
            living.apply_event(&event_of(event));
        }
        let window = window_text(w);
        let shown = &closed[closed.len().saturating_sub(CLOSED_NOTES_IN_PROMPT)..];
        let user = render_user(&window, &living, topic.as_deref(), since.as_deref(), shown);
        assert_eq!(
            user,
            step["user"].as_str().unwrap(),
            "window {}",
            w["window_index"]
        );

        let decoded = decode(step["ops"].as_str().unwrap(), living.maxid + 1).unwrap();
        if let Some(new) = decoded.topic.clone() {
            closed.extend(living.open_texts());
            living.new_topic();
            living.topic = Some(new.clone());
            topic = Some(new);
            since = Some(window.start.clone());
        }
        let reasons: Vec<Vec<String>> = decoded.ops.iter().map(|op| living.apply(op)).collect();
        let expected: Vec<Vec<String>> = step["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .map(strings)
            .collect();
        assert_eq!(reasons, expected);
        assert_eq!(living.render(), strings(&step["notes_after"]));
    }
}

#[test]
fn a_session_replaying_the_gold_answers_writes_the_same_prompts() {
    // The worker's path: the user's lines come from their notes document.
    let replay = fixture("replay");
    let mut session = Session::default();
    let mut doc: Vec<DocLine> = Vec::new();
    let mut previous: Vec<Segment> = Vec::new();
    for step in replay["windows"].as_array().unwrap() {
        let w = &step["window"];
        for event in step["events"].as_array().unwrap() {
            doc.push(DocLine {
                block_id: format!("block-{}", event["id"].as_str().unwrap()),
                ms: event["ms"].as_i64(),
                text: event["text"].as_str().unwrap().to_string(),
                ai_entry: None,
            });
        }
        assert!(session.sync_document(&doc).is_empty());
        let segments = view_segments(&w["segments"]);
        let window = Window {
            start_ms: w["start_ms"].as_i64().unwrap(),
            end_ms: w["end_ms"].as_i64().unwrap(),
            max_entries: 1,
            dropped_segments: 0,
            segments,
        };
        let mut text = WindowText::of(&window, &previous);
        // The fixture's context comes from the previous window's real times;
        // `view_segments` only keeps start times.
        text.context = w["context"].as_str().unwrap().to_string();
        assert_eq!(session.prompt(&text), step["user"].as_str().unwrap());
        let (_, applied) = session.apply_answer(step["ops"].as_str().unwrap(), &window, "p", "m");
        assert!(applied.iter().all(|a| a.rejected.is_empty()), "{applied:?}");
        previous = window.segments;
    }
}

#[test]
fn answers_decode_as_compact_ops_does() {
    for case in fixture("compact").as_array().unwrap() {
        let text = case["text"].as_str().unwrap();
        let decoded = decode(text, case["next_id"].as_u64().unwrap() as u32);
        match case.get("error") {
            Some(_) => assert!(decoded.is_err(), "{text:?} should not parse"),
            None => assert_eq!(decoded_json(&decoded.unwrap()), case["decoded"], "{text:?}"),
        }
    }
}

#[test]
fn notes_events_repairs_and_duplicates_follow_the_validator() {
    for (name, steps) in fixture("living").as_object().unwrap() {
        let mut living = Living::default();
        for (n, step) in steps.as_array().unwrap().iter().enumerate() {
            let at = format!("{name} step {n}");
            if let Some(event) = step.get("event") {
                assert_eq!(
                    living.apply_event(&event_of(event)),
                    step["changed"],
                    "{at}"
                );
            } else if let Some(topic) = step.get("topic") {
                living.new_topic();
                living.topic = Some(topic.as_str().unwrap().to_string());
            } else {
                let decoded = decode(step["output"].as_str().unwrap(), living.maxid + 1).unwrap();
                let results = step["results"].as_array().unwrap();
                assert_eq!(decoded.ops.len(), results.len(), "{at}");
                for (op, expected) in decoded.ops.into_iter().zip(results) {
                    assert_eq!(op_json(&op), expected["op"], "{at}");
                    let applied = apply_op(&mut living, op);
                    let rejected = expected
                        .get("duplicate")
                        .map(strings)
                        .unwrap_or_else(|| strings(&expected["reasons"]));
                    assert_eq!(applied.rejected, rejected, "{at}");
                    assert_eq!(
                        applied.repaired_from,
                        strings(&expected["repaired_from"]),
                        "{at}"
                    );
                }
            }
            assert_eq!(living.maxid, step["maxid"].as_u64().unwrap() as u32, "{at}");
            assert_eq!(living.render(), strings(&step["notes"]), "{at}");
        }
    }
}

#[test]
fn pause_windows_and_their_context_match_the_training_windows() {
    let pause = fixture("pause");
    let mut scheduler = PauseScheduler::default();
    for s in pause["segments"].as_array().unwrap() {
        let received = s["end_ms"].as_u64().unwrap() + 1000;
        scheduler.push(segment_of(s, received));
    }
    let mut windows = Vec::new();
    let mut previous: Vec<Segment> = Vec::new();
    while let Some(window) = scheduler.flush(Duration::from_secs(3600)) {
        let text = WindowText::of(&window, &previous);
        windows.push(json!({"start_ms": window.start_ms, "end_ms": window.end_ms,
            "start": text.start, "end": text.end, "transcript": text.transcript, "context": text.context}));
        previous = window.segments;
        scheduler.complete();
    }
    assert_eq!(Value::Array(windows), pause["windows"]);
}

#[test]
fn the_prompt_is_the_chat_template_the_adapter_was_trained_on() {
    let template = fixture("template");
    let (system, user) = turns(template["user"].as_str().unwrap());
    assert_eq!(system, template["system"]);
    let formatted =
        crate::summary::summary_engine::models::format_prompt("gemma4", system, &user).unwrap();
    // BOS comes from the tokenizer (the GGUF's add_bos_token).
    let rendered = template["rendered"].as_str().unwrap();
    assert_eq!(rendered.strip_prefix("<bos>"), Some(formatted.as_str()));
}

// --- The app's side ------------------------------------------------------------

fn window(start_s: i64, end_s: i64) -> Window {
    Window {
        segments: Vec::new(),
        start_ms: start_s * 1000,
        end_ms: end_s * 1000,
        max_entries: 1,
        dropped_segments: 0,
    }
}

fn adds(changes: &[EntryChange]) -> Vec<&TimelineEntry> {
    changes
        .iter()
        .filter_map(|c| match c {
            EntryChange::Add(e) => Some(e),
            _ => None,
        })
        .collect()
}

#[test]
fn answers_become_entries_edits_and_removals() {
    let mut session = Session::default();
    let (changes, _) = session.apply_answer(
        "T=Launch plan\n+d 00:10-00:20|Launch moves to the second week of May\n>1 p 00:21|Marketing needs the final date by Friday\n+a 00:25 Priya|Priya books the venue for the launch event",
        &window(0, 30),
        "builtin-ai",
        "gemma4:e2b",
    );
    let new = adds(&changes);
    assert_eq!(new.len(), 3);
    assert_eq!(
        (new[0].kind.as_str(), new[0].start_ms, new[0].end_ms),
        ("decision", 10_000, 20_000)
    );
    assert_eq!(new[0].topic.as_deref(), Some("Launch plan"));
    assert_eq!(new[1].parent.as_deref(), Some(new[0].id.as_str()));
    assert_eq!(
        (new[2].kind.as_str(), new[2].speaker.as_deref()),
        ("action", Some("Priya"))
    );
    let (decision, detail) = (new[0].id.clone(), new[1].id.clone());

    // An edit changes the entry; a drop removes it and its sub-bullets.
    let (changes, _) = session.apply_answer(
        "~1 @00:40|Launch moves to May 14\n-3",
        &window(30, 60),
        "builtin-ai",
        "gemma4:e2b",
    );
    assert!(
        matches!(&changes[0], EntryChange::Update(e) if e.id == decision && e.text == "Launch moves to May 14" && e.end_ms == 40_000)
    );
    assert_eq!(changes.len(), 2);
    assert!(matches!(&changes[1], EntryChange::Remove(_)));
    let (changes, _) = session.apply_answer("-1", &window(60, 90), "p", "m");
    let mut removed: Vec<&String> = changes
        .iter()
        .filter_map(|c| match c {
            EntryChange::Remove(id) => Some(id),
            _ => None,
        })
        .collect();
    removed.sort();
    let mut expected = vec![&decision, &detail];
    expected.sort();
    assert_eq!(removed, expected);
}

#[test]
fn a_note_edited_in_the_same_answer_is_added_as_edited_and_times_stay_in_the_window() {
    let mut session = Session::default();
    let (changes, _) = session.apply_answer(
        "T=Budget\n+p 00:10-09:59|Budget capped at 40k for the pilot\n~1|Budget capped at 45k for the pilot",
        &window(0, 30),
        "p",
        "m",
    );
    let new = adds(&changes);
    assert_eq!(new.len(), 1);
    assert_eq!(new[0].text, "Budget capped at 45k for the pilot");
    assert_eq!(new[0].end_ms, 30_000);
    assert_eq!(changes.len(), 1);
}

#[test]
fn an_unreadable_answer_changes_nothing_and_the_gate_answer_is_nothing() {
    let mut session = Session::default();
    for raw in [
        ".",
        "",
        "Sure! Here are the notes:",
        "+p 00:10|ok\nnot a line",
    ] {
        let (changes, applied) = session.apply_answer(raw, &window(0, 30), "p", "m");
        assert!(changes.is_empty() && applied.is_empty(), "{raw:?}");
    }
    assert_eq!(session.living.maxid, 0);
    assert_eq!(session.topic(), None);
}

#[test]
fn the_same_title_again_keeps_the_topic_open() {
    let mut session = Session::default();
    session.apply_answer(
        "T=Launch plan\n+p 00:10|Launch moves to the second week of May",
        &window(0, 30),
        "p",
        "m",
    );
    session.apply_answer("T=launch plan.", &window(200, 230), "p", "m");
    assert_eq!(session.living.render().len(), 1);
    session.apply_answer("T=Hiring", &window(300, 330), "p", "m");
    assert!(session.living.render().is_empty());
    let prompt = session.prompt(&WindowText::of(&window(330, 360), &[]));
    assert!(prompt.contains("TOPIC: Hiring (since 05:00)"));
    assert!(prompt.contains("EARLIER TOPICS, last notes: Launch moves to the second week of May"));
}

#[test]
fn a_new_note_repeating_an_earlier_topic_is_dropped() {
    let mut session = Session::default();
    session.apply_answer(
        "T=Launch plan\n+p 00:10|Launch event moves to the Lisbon venue in May",
        &window(0, 30),
        "p",
        "m",
    );
    let (changes, applied) = session.apply_answer(
        "T=Venue\n+p 03:10|Launch event moves to Lisbon venue in May\n+p 03:12|Catering budget is still open",
        &window(190, 220),
        "p",
        "m",
    );
    assert_eq!(
        applied[0].rejected,
        vec!["repeats_an_earlier_topic".to_string()]
    );
    assert_eq!(adds(&changes).len(), 1);
    // Ids stay sequential for the model: the rejected add's id is free again.
    assert_eq!(session.living.maxid, 3);
}

fn line(block: &str, text: &str, ai_entry: Option<&str>) -> DocLine {
    DocLine {
        block_id: block.into(),
        ms: Some(12_000),
        text: text.into(),
        ai_entry: ai_entry.map(str::to_string),
    }
}

#[test]
fn the_users_lines_reach_the_model_as_you_notes() {
    let mut session = Session::default();
    // Before the first topic a line waits.
    session.sync_document(&[line("b1", "budget 40k?", None)]);
    assert!(session.living.render().is_empty());
    let (changes, _) = session.apply_answer(
        "T=Budget\n+p 00:10|Pilot budget capped at 40k for the quarter",
        &window(0, 30),
        "p",
        "m",
    );
    let entry = adds(&changes)[0].id.clone();
    session.sync_document(&[
        line("b1", "budget 40k?", None),
        line(
            "b2",
            "Pilot budget capped at 40k for the quarter",
            Some(&entry),
        ),
    ]);
    assert_eq!(
        session.living.render(),
        // The line joined once the topic existed, after the AI's note.
        vec![
            "1 [ai] Pilot budget capped at 40k for the quarter",
            "u1 [you] budget 40k?",
        ]
    );
    // The user rewrites the AI's line and their own: both are theirs now.
    session.sync_document(&[
        line("b1", "budget 40k? ask finance", None),
        line("b2", "Pilot budget 40k, maybe 45k", None),
    ]);
    assert_eq!(
        session.living.render(),
        vec![
            "1 [you] (you edited an ai note: now theirs) Pilot budget 40k, maybe 45k",
            "u1 [you] budget 40k? ask finance",
        ]
    );
    let (changes, applied) = session.apply_answer(
        "-1\n>u1 p 00:40|Finance confirms the 40k cap",
        &window(30, 60),
        "p",
        "m",
    );
    assert!(applied[0].rejected[0].starts_with("drop_of_user_note"));
    let child = adds(&changes)[0];
    assert_eq!(child.parent.as_deref(), Some("b1"));
    assert_eq!(changes.len(), 1);
}

#[test]
fn a_deleted_ai_line_drops_its_note_and_entry() {
    let mut session = Session::default();
    let (changes, _) = session.apply_answer(
        "T=Budget\n+p 00:10|Pilot budget capped at 40k for the quarter\n>1 p 00:12|Finance signs off on Friday",
        &window(0, 30),
        "p",
        "m",
    );
    let new = adds(&changes);
    let (parent, child) = (new[0].id.clone(), new[1].id.clone());
    session.sync_document(&[
        line(
            "b1",
            "Pilot budget capped at 40k for the quarter",
            Some(&parent),
        ),
        line("b2", "Finance signs off on Friday", Some(&child)),
    ]);
    let mut removed =
        session.sync_document(&[line("b2", "Finance signs off on Friday", Some(&child))]);
    removed.sort();
    let mut expected = vec![parent, child];
    expected.sort();
    assert_eq!(removed, expected);
    assert!(session.living.render().is_empty());
}

#[test]
fn ai_prefixes_are_not_part_of_the_users_rewrite() {
    assert_eq!(
        without_ai_prefix("Decision: Ship it Friday"),
        "Ship it Friday"
    );
    assert_eq!(
        without_ai_prefix("Action: Priya — Book the venue"),
        "Book the venue"
    );
    assert_eq!(without_ai_prefix("Plain line"), "Plain line");
}

#[test]
fn a_pause_window_waits_for_the_pause_and_closes_on_silence() {
    let mut scheduler = PauseScheduler::default();
    let words = |n: usize| {
        (0..n)
            .map(|i| format!("w{i}"))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let seg = |start: i64, end: i64, speaker: Speaker, n: usize| Segment {
        start_ms: start,
        end_ms: end,
        speaker,
        text: words(n),
        received_at: Duration::from_millis(end as u64 + 500),
    };
    scheduler.push(seg(0, 10_000, Speaker::You, 20));
    scheduler.push(seg(10_100, 21_000, Speaker::You, 20));
    // 21 s long, but no pause yet: nothing.
    assert!(scheduler
        .poll(Duration::from_millis(22_000), false)
        .is_none());
    // Silence: it closes.
    let first = scheduler
        .poll(Duration::from_millis(24_600), false)
        .unwrap();
    assert_eq!(
        (first.start_ms, first.end_ms, first.max_entries),
        (0, 21_000, 1)
    );
    // Windows wait for the one in flight, in order.
    scheduler.push(seg(22_000, 43_000, Speaker::Others, 20));
    scheduler.push(seg(43_100, 44_000, Speaker::You, 3));
    scheduler.push(seg(44_100, 66_000, Speaker::Others, 20));
    scheduler.push(seg(66_100, 67_000, Speaker::You, 3));
    assert!(scheduler.poll(Duration::from_secs(100), false).is_none());
    let mut scheduler = crate::live_summary::scheduler::Scheduler::Pauses(scheduler);
    scheduler.complete();
    assert_eq!(
        scheduler
            .poll(Duration::from_secs(100), false)
            .unwrap()
            .start_ms,
        22_000
    );
    scheduler.complete();
    let third = scheduler.poll(Duration::from_secs(101), false).unwrap();
    assert_eq!(third.start_ms, 43_100);
    scheduler.complete();
    // The last few words close only at stop, and only with enough words.
    assert!(scheduler.poll(Duration::from_secs(200), false).is_none());
    assert!(scheduler.flush(Duration::from_secs(200)).is_none());
}
