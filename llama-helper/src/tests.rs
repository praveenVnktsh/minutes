use super::*;

#[test]
fn context_budget_accepts_prompt_that_fits_with_output() {
    assert!(check_context_budget(1000, 3000, 4096).is_ok());
    assert!(check_context_budget(1096, 3000, 4096).is_ok());
}

#[test]
fn context_budget_rejects_prompt_plus_output_over_window() {
    let error = check_context_budget(1097, 3000, 4096).unwrap_err();
    assert!(matches!(
        error,
        GenerateError::ContextOverflow {
            prompt_tokens: 1097,
            max_tokens: 3000,
            context_size: 4096
        }
    ));
    assert!(error.to_string().contains("context overflow"));
}

#[test]
fn context_budget_rejects_empty_prompt_as_other_error() {
    assert!(matches!(
        check_context_budget(0, 10, 4096),
        Err(GenerateError::Other(_))
    ));
}

#[test]
fn overflow_response_carries_machine_readable_kind() {
    let response = Response::from_generation(
        Err(GenerateError::ContextOverflow {
            prompt_tokens: 5000,
            max_tokens: 4096,
            context_size: 8192,
        }),
        None,
    );
    let json = serde_json::to_value(&response).unwrap();
    assert_eq!(json["type"], "response");
    assert_eq!(json["error_kind"], "context_overflow");
    assert!(json["error"].as_str().unwrap().contains("context overflow"));
    assert!(json.get("id").is_none());
}

fn output(finish_reason: FinishReason) -> GenerationOutput {
    GenerationOutput {
        text: "partial".to_string(),
        finish_reason,
        prompt_tokens: 12,
        completion_tokens: 34,
        reused_tokens: None,
        first_token_ms: Some(80),
        tokens_per_second: 25.0,
    }
}

#[test]
fn success_response_reports_finish_reason_and_token_counts() {
    let response = Response::from_generation(Ok(output(FinishReason::Length)), None);
    let json = serde_json::to_value(&response).unwrap();
    assert_eq!(json["finish_reason"], "length");
    assert_eq!(json["prompt_tokens"], 12);
    assert_eq!(json["completion_tokens"], 34);
    assert!(json["error"].is_null());
    assert!(json.get("error_kind").is_none());
    assert!(
        json.get("id").is_none(),
        "old callers see no new fields they did not ask for"
    );
    assert!(json.get("reused_tokens").is_none());
}

#[test]
fn a_cancelled_request_still_gets_its_one_final_reply_with_its_id() {
    let mut cancelled = output(FinishReason::Cancelled);
    cancelled.reused_tokens = Some(900);
    let json = serde_json::to_value(Response::from_generation(Ok(cancelled), Some(7))).unwrap();
    assert_eq!(json["type"], "response");
    assert_eq!(json["finish_reason"], "cancelled");
    assert_eq!(json["id"], 7);
    assert_eq!(json["reused_tokens"], 900);
    assert_eq!(json["first_token_ms"], 80);
}

#[test]
fn token_lines_carry_the_request_id() {
    let json = serde_json::to_string(&Response::Token {
        id: Some(3),
        text: "Hi".into(),
    })
    .unwrap();
    assert_eq!(json, r#"{"type":"token","id":3,"text":"Hi"}"#);
}

#[test]
fn streaming_and_cache_fields_are_optional_and_parsed() {
    let plain: Request = serde_json::from_str(r#"{"type":"generate","prompt":"p"}"#).unwrap();
    let Request::Generate {
        id,
        stream,
        cache_prompt,
        cache_prefix_chars,
        cache_context_size,
        ..
    } = plain
    else {
        panic!("expected generate request");
    };
    assert_eq!(
        (
            id,
            stream,
            cache_prompt,
            cache_prefix_chars,
            cache_context_size
        ),
        (None, None, None, None, None)
    );

    let cached: Request = serde_json::from_str(
        r#"{"type":"generate","prompt":"p","id":9,"stream":true,"cache_prompt":true,"cache_prefix_chars":120,"cache_context_size":8192}"#,
    )
    .unwrap();
    let Request::Generate {
        id,
        stream,
        cache_prompt,
        cache_prefix_chars,
        cache_context_size,
        ..
    } = cached
    else {
        panic!("expected generate request");
    };
    assert_eq!(
        (
            id,
            stream,
            cache_prompt,
            cache_prefix_chars,
            cache_context_size
        ),
        (Some(9), Some(true), Some(true), Some(120), Some(8192))
    );
}

#[test]
fn cancel_requests_parse() {
    let request: Request = serde_json::from_str(r#"{"type":"cancel","id":4}"#).unwrap();
    assert!(matches!(request, Request::Cancel { id: 4 }));
}

#[test]
fn prefix_boundary_never_splits_a_character() {
    assert_eq!(floor_char_boundary("héllo", 2), 1);
    assert_eq!(floor_char_boundary("héllo", 3), 3);
    assert_eq!(floor_char_boundary("abc", 99), 3);
}

#[test]
fn generate_request_seed_is_optional_and_parsed() {
    let without: Request = serde_json::from_str(r#"{"type":"generate","prompt":"p"}"#).unwrap();
    let Request::Generate { seed, .. } = without else {
        panic!("expected generate request");
    };
    assert_eq!(seed, None);

    let with: Request =
        serde_json::from_str(r#"{"type":"generate","prompt":"p","seed":7}"#).unwrap();
    let Request::Generate { seed, .. } = with else {
        panic!("expected generate request");
    };
    assert_eq!(seed, Some(7));
    assert_eq!(DEFAULT_SEED, 42);
}

#[test]
fn tokenize_request_and_token_count_response_round_trip() {
    let request: Request =
        serde_json::from_str(r#"{"type":"tokenize","text":"hello","context_size":4096}"#).unwrap();
    assert!(matches!(
        request,
        Request::Tokenize { ref text, context_size: Some(4096), model_path: None } if text == "hello"
    ));
    let json = serde_json::to_string(&Response::TokenCount { count: 3 }).unwrap();
    assert_eq!(json, r#"{"type":"token_count","count":3}"#);
}

#[test]
fn generate_request_defaults_penalties_when_omitted() {
    let json =
        r#"{"type":"generate","prompt":"summarize","temperature":0.5,"top_k":20,"top_p":0.8}"#;
    let request: Request = serde_json::from_str(json).unwrap();
    let Request::Generate {
        temperature,
        top_k,
        top_p,
        presence_penalty,
        frequency_penalty,
        repeat_penalty,
        penalty_last_n,
        ..
    } = request
    else {
        panic!("expected generate request");
    };

    let sampling = SamplingConfig::from_request(
        temperature,
        top_k,
        top_p,
        presence_penalty,
        frequency_penalty,
        repeat_penalty,
        penalty_last_n,
    );

    assert_eq!(sampling.presence_penalty, 0.0);
    assert_eq!(sampling.frequency_penalty, 0.0);
    assert_eq!(sampling.repeat_penalty, 1.0);
    assert_eq!(sampling.penalty_last_n, 0);
    assert!(!sampling.uses_penalties());
}

#[test]
fn generate_request_deserializes_qwen_penalties() {
    let json = r#"{"type":"generate","prompt":"summarize","temperature":0.5,"top_k":20,"top_p":0.8,"presence_penalty":0.3,"frequency_penalty":0.0,"repeat_penalty":1.05,"penalty_last_n":256}"#;
    let request: Request = serde_json::from_str(json).unwrap();
    let Request::Generate {
        temperature,
        top_k,
        top_p,
        presence_penalty,
        frequency_penalty,
        repeat_penalty,
        penalty_last_n,
        ..
    } = request
    else {
        panic!("expected generate request");
    };

    let sampling = SamplingConfig::from_request(
        temperature,
        top_k,
        top_p,
        presence_penalty,
        frequency_penalty,
        repeat_penalty,
        penalty_last_n,
    );

    assert_eq!(sampling.temperature, 0.5);
    assert_eq!(sampling.top_k, 20);
    assert_eq!(sampling.top_p, 0.8);
    assert_eq!(sampling.presence_penalty, 0.3);
    assert_eq!(sampling.frequency_penalty, 0.0);
    assert_eq!(sampling.repeat_penalty, 1.05);
    assert_eq!(sampling.penalty_last_n, 256);
    assert!(sampling.uses_penalties());
}

#[test]
fn thread_count_can_be_lowered_per_request() {
    let request: Request =
        serde_json::from_str(r#"{"type":"generate","prompt":"p","n_threads":2}"#).unwrap();
    let Request::Generate { n_threads, .. } = request else {
        panic!("expected generate request");
    };
    assert_eq!(n_threads, Some(2));
}

#[test]
fn small_machines_keep_one_checkpoint_and_a_smaller_cached_context() {
    assert_eq!(cache_limits(Some(8 * 1024 * 1024 * 1024)), (1, Some(4096)));
    assert_eq!(
        cache_limits(Some(16 * 1024 * 1024 * 1024)),
        (MAX_CHECKPOINTS, None)
    );
    assert_eq!(cache_limits(None), (MAX_CHECKPOINTS, None));
}
