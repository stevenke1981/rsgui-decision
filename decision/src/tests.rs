use super::*;
use std::cell::Cell;
use std::io::Write;
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

const KEY: &str = "unit-test-secret";

fn input() -> DecisionInput {
    DecisionInput {
        goal: "Save the open document".to_owned(),
        candidates: vec![
            Candidate {
                id: "save".to_owned(),
                description: "Click the validated Save control".to_owned(),
            },
            Candidate {
                id: "cancel".to_owned(),
                description: "Click the validated Cancel control".to_owned(),
            },
        ],
        facts: Facts::Unknown,
        minimum_confidence: 0.8,
    }
}

fn answer(choice: &str, confidence: f64) -> serde_json::Value {
    json!({
        "answers": {
            "blocked": {"type": "noul", "noul": 0.01, "confidence": 0.99},
            "done": {"type": "noul", "noul": 0.01, "confidence": 0.99},
            "next_action": {"type": "choice", "choice": choice, "confidence": confidence}
        },
        "usage": {"total_input_tokens": 170}
    })
}

/// A loopback-only provider. It captures exactly one request; no test has a live provider URL.
fn mock(
    mode: Mode,
    status: u16,
    body: String,
    extra_headers: &str,
    delay: Duration,
) -> (Engine, Receiver<String>, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/v1beta/decisions", listener.local_addr().unwrap());
    let headers = extra_headers.to_owned();
    let (send, receive) = mpsc::channel();
    let handle = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut request = Vec::new();
        let mut buffer = [0; 8192];
        loop {
            let count = socket.read(&mut buffer).unwrap();
            assert!(count > 0, "request ended before its declared body");
            request.extend_from_slice(&buffer[..count]);
            if let Some(end) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&request[..end]);
                let length = head
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap();
                if request.len() >= end + 4 + length {
                    break;
                }
            }
        }
        send.send(String::from_utf8(request).unwrap()).unwrap();
        thread::sleep(delay);
        let response = format!(
            "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n{body}",
            body.len()
        );
        // A timeout or cancellation can close the socket before this bounded fixture responds.
        let _ = socket.write_all(response.as_bytes());
    });
    let engine =
        Engine::configured(mode, KEY.to_owned(), Duration::from_millis(100), &endpoint).unwrap();
    (engine, receive, handle)
}

fn remote(mode: Mode, body: serde_json::Value) -> Result<Decision, Error> {
    let (mut engine, receive, handle) = mock(mode, 200, body.to_string(), "", Duration::ZERO);
    let result = engine.evaluate(&input(), None, &|| Ok(()));
    receive.recv_timeout(Duration::from_secs(3)).unwrap();
    handle.join().unwrap();
    result
}

#[test]
fn local_default_never_reads_a_key_or_calls_network() {
    let mut engine = Engine::from_configuration(|name| {
        assert!(!name.ends_with("API_KEY"));
        Ok(None)
    })
    .unwrap();
    assert_eq!(engine.mode(), Mode::Local);
    let decision = engine.evaluate(&input(), None, &|| Ok(())).unwrap();
    assert_eq!(decision.action, ActionDecision::Observe);
    assert_eq!(decision.calls, 0);
    assert_eq!(decision.source, "local");
    assert_eq!(decision.input_tokens, None);
    assert_eq!(decision.candidate_id, None);
}

#[test]
fn local_truth_is_deterministic_in_every_mode() {
    for mode in [Mode::Local, Mode::Auto, Mode::Gemini] {
        for (facts, action) in [
            (Facts::Wait, ActionDecision::Wait),
            (Facts::Done, ActionDecision::Done),
            (Facts::Blocked, ActionDecision::Stop),
            (
                Facts::Ready {
                    candidate_id: "save".to_owned(),
                },
                ActionDecision::Choose,
            ),
        ] {
            let mut input = input();
            input.candidates.truncate(1);
            input.facts = facts;
            let mut engine = Engine {
                mode,
                provider: None,
            };
            let decision = engine.evaluate(&input, None, &|| Ok(())).unwrap();
            assert_eq!(decision.action, action);
            assert_eq!(decision.calls, 0);
            assert_eq!(
                decision.candidate_id.as_deref(),
                (action == ActionDecision::Choose).then_some("save")
            );
        }
    }
}

#[test]
fn ready_cannot_bypass_the_single_validated_candidate_contract() {
    let mut value = input();
    value.facts = Facts::Ready {
        candidate_id: "save".to_owned(),
    };
    assert!(matches!(
        Engine::local().evaluate(&value, None, &|| Ok(())),
        Err(Error::InvalidInput(_))
    ));
    value.candidates.truncate(1);
    value.facts = Facts::Ready {
        candidate_id: "invented".to_owned(),
    };
    assert!(validate_input(&value, None).is_err());
}

#[test]
fn input_boundary_rejects_invalid_ids_sizes_confidence_and_images() {
    for id in [
        "", "observe", "wait", "done", "stop", "Save", "a b", "儲存", "1save",
    ] {
        let mut value = input();
        value.candidates[0].id = id.to_owned();
        assert!(validate_input(&value, None).is_err(), "accepted {id}");
    }
    let mut value = input();
    value.candidates[1].id = "save".to_owned();
    assert!(validate_input(&value, None).is_err());
    for confidence in [f64::NAN, f64::INFINITY, 0.49, 1.01] {
        value = input();
        value.minimum_confidence = confidence;
        assert!(validate_input(&value, None).is_err());
    }
    value = input();
    value.goal = "字".repeat(16_000);
    assert!(validate_input(&value, None).is_ok());
    value.goal.push('字');
    assert!(validate_input(&value, None).is_err());
    value = input();
    value.candidates[0].description = "x".repeat(2049);
    assert!(validate_input(&value, None).is_err());
    for image in ["", "bad!", "aGVsbG8="] {
        assert!(validate_input(&input(), Some(image)).is_err());
    }
    assert!(validate_input(&input(), Some(&"A".repeat(MAX_IMAGE_BASE64 + 1))).is_err());
    value = input();
    value.candidates.clear();
    assert!(validate_input(&value, None).is_err());
    value.candidates = (0..33)
        .map(|index| Candidate {
            id: format!("candidate_{index}"),
            description: String::new(),
        })
        .collect();
    assert!(validate_input(&value, None).is_err());
}

#[test]
fn configuration_is_explicit_bounded_and_uses_key_precedence() {
    for (name, invalid) in [
        ("RSGUI_DECISION_MODE", "remote"),
        ("RSGUI_DECISION_TIMEOUT_MS", "99"),
        ("RSGUI_DECISION_TIMEOUT_MS", "10001"),
        ("RSGUI_DECISION_TIMEOUT_MS", "-1"),
        ("RSGUI_DECISION_TIMEOUT_MS", "NaN"),
    ] {
        let result =
            Engine::from_configuration(|key| Ok((key == name).then(|| invalid.to_owned())));
        assert!(matches!(result, Err(Error::InvalidConfiguration(_))));
    }
    for timeout in ["100", "10000"] {
        assert!(
            Engine::from_configuration(|key| Ok(
                (key == "RSGUI_DECISION_TIMEOUT_MS").then(|| timeout.to_owned())
            ))
            .is_ok()
        );
    }
    let auto = Engine::from_configuration(|name| {
        Ok((name == "RSGUI_DECISION_MODE").then(|| "auto".to_owned()))
    })
    .unwrap();
    assert_eq!(auto.mode(), Mode::Auto);
    assert!(auto.provider.is_none());
    assert!(matches!(
        Engine::from_configuration(|name| Ok(
            (name == "RSGUI_DECISION_MODE").then(|| "gemini".to_owned())
        )),
        Err(Error::MissingApiKey)
    ));
    let engine = Engine::from_configuration(|name| {
        Ok(match name {
            "RSGUI_DECISION_MODE" => Some("gemini".to_owned()),
            "GEMINI_API_KEY" => Some(KEY.to_owned()),
            "GOOGLE_API_KEY" => panic!("lower priority key must not be read"),
            _ => None,
        })
    })
    .unwrap();
    assert_eq!(engine.provider.unwrap().endpoint, ENDPOINT);
    let fallback = Engine::from_configuration(|name| {
        Ok(match name {
            "RSGUI_DECISION_MODE" => Some("auto".to_owned()),
            "GEMINI_API_KEY" => Some(" ".to_owned()),
            "GOOGLE_API_KEY" => Some(KEY.to_owned()),
            _ => None,
        })
    })
    .unwrap();
    assert_eq!(fallback.provider.unwrap().key, KEY);
}

#[test]
fn known_remote_choice_preserves_only_candidate_id_and_usage() {
    let decision = remote(Mode::Gemini, answer("cancel", 0.96)).unwrap();
    assert_eq!(decision.action, ActionDecision::Choose);
    assert_eq!(decision.candidate_id.as_deref(), Some("cancel"));
    assert_eq!(decision.source, "gemini");
    assert_eq!(decision.calls, 1);
    assert_eq!(decision.input_tokens, Some(170));
    assert_eq!(decision.confidence, Some(0.96));
    assert!((170.0 * USD_PER_INPUT_TOKEN - 0.000017).abs() < 1e-12);
}

#[test]
fn image_and_independent_questions_are_in_one_bounded_request() {
    let image = base64::engine::general_purpose::STANDARD.encode(b"\x89PNG\r\n\x1a\nfixture");
    let (mut engine, receive, handle) = mock(
        Mode::Gemini,
        200,
        answer("save", 0.9).to_string(),
        "",
        Duration::ZERO,
    );
    let mut value = input();
    value.goal = format!("Save {KEY}");
    value.candidates[0].description = format!("Click Save {KEY}");
    engine.evaluate(&value, Some(&image), &|| Ok(())).unwrap();
    let request = receive.recv_timeout(Duration::from_secs(3)).unwrap();
    handle.join().unwrap();
    let (headers, body) = request.split_once("\r\n\r\n").unwrap();
    assert!(headers.starts_with("POST /v1beta/decisions HTTP/1.1"));
    assert!(headers.contains(&format!("x-goog-api-key: {KEY}")));
    assert!(!body.contains(KEY));
    let body: serde_json::Value = serde_json::from_str(body).unwrap();
    assert_eq!(body["model"], MODEL);
    assert_eq!(body["questions"].as_object().unwrap().len(), 3);
    assert_eq!(body["questions"]["blocked"]["type"], "noul");
    assert_eq!(body["questions"]["done"]["type"], "noul");
    assert_eq!(
        body["questions"]["next_action"]["criteria"]
            .as_object()
            .unwrap()
            .len(),
        6
    );
    assert_eq!(body["input"][0]["type"], "text");
    assert_eq!(
        body["input"][1],
        json!({"type":"image", "data":image, "mime_type":"image/png", "resolution":"low"})
    );
    assert!(body.get("tools").is_none());
    assert!(body.get("generation_config").is_none());
}

#[test]
fn malformed_response_and_unknown_choices_are_strict_or_safe_fallback() {
    let mut cases = vec![json!({}), json!({"answers": []}), answer("invented", 0.99)];
    for probabilities in [
        json!({"invented":1.0}),
        json!({"save":-0.1}),
        json!({"save":1.1}),
        json!({"save":0.0}),
        json!({}),
        json!({"save":0.8,"cancel":0.8}),
        json!({"cancel":1.0}),
        json!(null),
    ] {
        let mut body = answer("save", 0.99);
        body["answers"]["next_action"]["probabilities"] = probabilities;
        cases.push(body);
    }
    for value in [json!(null), json!(-0.1), json!(1.1), json!("0.99")] {
        let mut body = answer("save", 0.99);
        body["answers"]["next_action"]["confidence"] = value;
        cases.push(body);
    }
    for value in [json!(null), json!(-0.1), json!(1.1)] {
        let mut body = answer("save", 0.99);
        body["answers"]["blocked"]["noul"] = value;
        cases.push(body);
    }
    for body in cases {
        assert!(matches!(
            remote(Mode::Gemini, body.clone()),
            Err(Error::Malformed(_))
        ));
        let decision = remote(Mode::Auto, body).unwrap();
        assert_eq!(decision.action, ActionDecision::Observe);
        assert_eq!(decision.candidate_id, None);
        assert_eq!(decision.calls, 1);
        assert!(decision.error.is_some());
    }
    let (mut engine, receive, handle) = mock(
        Mode::Gemini,
        200,
        answer("save", 0.99).to_string().replace("0.99", "NaN"),
        "",
        Duration::ZERO,
    );
    assert!(matches!(
        engine.evaluate(&input(), None, &|| Ok(())),
        Err(Error::Malformed(_))
    ));
    receive.recv_timeout(Duration::from_secs(3)).unwrap();
    handle.join().unwrap();
}

#[test]
fn provider_metadata_is_extensible_without_expanding_action_scope() {
    let mut body = answer("save", 0.99);
    body["usage"]["total_output_tokens"] = json!(20);
    body["usage"]["input_tokens_by_modality"] = json!({"text":100,"image":70});
    body["model_version"] = json!("future_revision");
    body["answers"]["future_question"] = json!({"type":"new_type"});
    body["answers"]["next_action"]["command"] = json!("execute arbitrary code");
    let decision = remote(Mode::Gemini, body).unwrap();
    assert_eq!(decision.action, ActionDecision::Choose);
    assert_eq!(decision.candidate_id.as_deref(), Some("save"));
    assert_eq!(decision.input_tokens, Some(170));
    assert!(
        !serde_json::to_string(&decision)
            .unwrap()
            .contains("execute arbitrary code")
    );
}

#[test]
fn low_confidence_and_disagreement_never_choose_the_first_candidate() {
    let mut cases = vec![answer("cancel", 0.79)];
    let mut body = answer("save", 0.99);
    body["answers"]["next_action"]["probabilities"] = json!({"save":0.1,"cancel":0.9});
    cases.push(body);
    let mut body = answer("save", 0.99);
    body["answers"]["done"]["noul"] = json!(0.5);
    cases.push(body);
    let mut body = answer("save", 0.99);
    body["answers"]["next_action"]
        .as_object_mut()
        .unwrap()
        .remove("confidence");
    cases.push(body);
    for body in cases {
        assert_eq!(
            remote(Mode::Gemini, body.clone()).unwrap_err(),
            Error::LowConfidence
        );
        let decision = remote(Mode::Auto, body).unwrap();
        assert_eq!(decision.action, ActionDecision::Observe);
        assert_eq!(decision.candidate_id, None);
    }
    let mut body = answer("cancel", 0.99);
    body["answers"]["next_action"]
        .as_object_mut()
        .unwrap()
        .remove("confidence");
    body["answers"]["next_action"]["probabilities"] = json!({"save":0.05,"cancel":0.95});
    assert_eq!(
        remote(Mode::Gemini, body).unwrap().candidate_id.as_deref(),
        Some("cancel")
    );
}

#[test]
fn semantic_completion_and_blocking_require_conservative_evidence() {
    for (name, action) in [
        ("done", ActionDecision::Done),
        ("blocked", ActionDecision::Stop),
    ] {
        let mut body = answer("save", 0.99);
        body["answers"][name]["noul"] = json!(0.9);
        let decision = remote(Mode::Gemini, body.clone()).unwrap();
        assert_eq!(decision.action, action);
        assert_eq!(decision.candidate_id, None);
        body["answers"][name]["confidence"] = json!(0.79);
        assert_eq!(
            remote(Mode::Gemini, body).unwrap_err(),
            Error::LowConfidence
        );
    }
    assert_eq!(
        remote(Mode::Gemini, answer("done", 0.99)).unwrap_err(),
        Error::LowConfidence
    );
    for (choice, action) in [
        ("observe", ActionDecision::Observe),
        ("wait", ActionDecision::Wait),
        ("stop", ActionDecision::Stop),
    ] {
        let decision = remote(Mode::Gemini, answer(choice, 0.9)).unwrap();
        assert_eq!(decision.action, action);
        assert_eq!(decision.candidate_id, None);
    }
    let mut body = answer("save", 0.99);
    body["answers"]["done"]["noul"] = json!(0.9);
    body["answers"]["blocked"]["noul"] = json!(0.9);
    assert_eq!(
        remote(Mode::Gemini, body).unwrap().action,
        ActionDecision::Stop
    );

    let mut body = answer("save", 0.99);
    body["answers"]["done"]["noul"] = json!(0.9);
    let (mut engine, receive, handle) =
        mock(Mode::Gemini, 200, body.to_string(), "", Duration::ZERO);
    let mut value = input();
    value.minimum_confidence = 0.95;
    assert_eq!(
        engine.evaluate(&value, None, &|| Ok(())).unwrap_err(),
        Error::LowConfidence
    );
    receive.recv_timeout(Duration::from_secs(3)).unwrap();
    handle.join().unwrap();
}

#[test]
fn unavailable_transport_and_absent_keys_have_safe_auto_fallback() {
    for mode in [Mode::Auto, Mode::Gemini] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/v1beta/decisions", listener.local_addr().unwrap());
        let handle = thread::spawn(move || {
            let (socket, _) = listener.accept().unwrap();
            socket.shutdown(std::net::Shutdown::Both).unwrap();
        });
        let mut engine =
            Engine::configured(mode, KEY.to_owned(), Duration::from_millis(100), &endpoint)
                .unwrap();
        let result = engine.evaluate(&input(), None, &|| Ok(()));
        if mode == Mode::Auto {
            let decision = result.unwrap();
            assert_eq!(decision.action, ActionDecision::Observe);
            assert_eq!(decision.calls, 1);
            assert_eq!(decision.candidate_id, None);
            assert!(decision.error.is_some());
        } else {
            assert_eq!(result.unwrap_err(), Error::Transport);
        }
        handle.join().unwrap();
    }
    let mut engine = Engine {
        mode: Mode::Auto,
        provider: None,
    };
    let decision = engine.evaluate(&input(), None, &|| Ok(())).unwrap();
    assert_eq!(decision.action, ActionDecision::Observe);
    assert_eq!(decision.calls, 0);
    assert_eq!(
        decision.error.as_deref(),
        Some("Google API key is not configured")
    );
}

#[test]
fn timeout_auth_and_rate_limit_start_cooldown_without_retries() {
    for (status, delay, expected) in [
        (200, Duration::from_millis(250), Error::Timeout),
        (401, Duration::ZERO, Error::Authentication),
        (403, Duration::ZERO, Error::Authentication),
        (429, Duration::ZERO, Error::RateLimited),
    ] {
        let (mut engine, receive, handle) = mock(
            Mode::Gemini,
            status,
            answer("save", 0.99).to_string(),
            "Retry-After: 600\r\n",
            delay,
        );
        assert_eq!(
            engine.evaluate(&input(), None, &|| Ok(())).unwrap_err(),
            expected
        );
        receive.recv_timeout(Duration::from_secs(3)).unwrap();
        let remaining = engine
            .provider
            .as_ref()
            .unwrap()
            .cooldown_until
            .unwrap()
            .duration_since(Instant::now());
        assert!(remaining <= Duration::from_secs(60));
        assert_eq!(
            engine.evaluate(&input(), None, &|| Ok(())).unwrap_err(),
            Error::Cooldown
        );
        engine.mode = Mode::Auto;
        let decision = engine.evaluate(&input(), None, &|| Ok(())).unwrap();
        assert_eq!(decision.action, ActionDecision::Observe);
        assert_eq!(decision.calls, 0);
        handle.join().unwrap();
    }
}

#[test]
fn cancellation_before_and_after_network_always_wins_in_auto_mode() {
    let mut local = Engine::local();
    assert_eq!(
        local
            .evaluate(&input(), None, &|| Err(std::io::Error::other("cancel")))
            .unwrap_err(),
        Error::Cancelled
    );
    let (mut engine, receive, handle) = mock(
        Mode::Auto,
        200,
        answer("save", 0.99).to_string(),
        "",
        Duration::ZERO,
    );
    let count = Cell::new(0);
    assert_eq!(
        engine
            .evaluate(&input(), None, &|| {
                count.set(count.get() + 1);
                if count.get() >= 3 {
                    Err(std::io::Error::other("cancel"))
                } else {
                    Ok(())
                }
            })
            .unwrap_err(),
        Error::Cancelled
    );
    receive.recv_timeout(Duration::from_secs(3)).unwrap();
    handle.join().unwrap();
}

#[test]
fn provider_error_bodies_and_malformed_fields_cannot_leak_secrets() {
    for status in [400, 401, 429, 500] {
        let (mut engine, receive, handle) = mock(
            Mode::Auto,
            status,
            format!("{{\"error\":\"{KEY}\"}}"),
            "",
            Duration::ZERO,
        );
        let decision = engine.evaluate(&input(), None, &|| Ok(())).unwrap();
        assert!(!serde_json::to_string(&decision).unwrap().contains(KEY));
        receive.recv_timeout(Duration::from_secs(3)).unwrap();
        handle.join().unwrap();
    }
    let error = remote(Mode::Gemini, answer(KEY, 0.99)).unwrap_err();
    assert!(!format!("{error:?} {error}").contains(KEY));
}

#[test]
fn response_redirects_are_not_followed() {
    let (mut engine, receive, handle) = mock(
        Mode::Gemini,
        302,
        String::new(),
        "Location: https://example.invalid/steal\r\n",
        Duration::ZERO,
    );
    assert_eq!(
        engine.evaluate(&input(), None, &|| Ok(())).unwrap_err(),
        Error::Http(302)
    );
    receive.recv_timeout(Duration::from_secs(3)).unwrap();
    handle.join().unwrap();
}

#[test]
fn serde_boundaries_reject_unknown_fields_and_duplicate_answers() {
    assert!(
        serde_json::from_value::<Candidate>(
            json!({"id":"save","description":"save","command":"bad"})
        )
        .is_err()
    );
    assert!(
        serde_json::from_value::<Facts>(json!({"ready":{"candidate_id":"save","extra":true}}))
            .is_err()
    );
    let mut value = serde_json::to_value(input()).unwrap();
    value["extra"] = json!(true);
    assert!(serde_json::from_value::<DecisionInput>(value).is_err());
    let text = answer("save", 0.99)
        .to_string()
        .replace("\"done\":", "\"blocked\":");
    assert!(serde_json::from_str::<Response>(&text).is_err());
}

#[test]
fn retry_after_handles_seconds_dates_and_bad_values() {
    assert_eq!(retry_after_duration("600"), Some(Duration::from_secs(60)));
    assert_eq!(retry_after_duration("0"), Some(Duration::from_secs(1)));
    let future = httpdate::fmt_http_date(SystemTime::now() + Duration::from_secs(600));
    assert_eq!(retry_after_duration(&future), Some(Duration::from_secs(60)));
    assert_eq!(retry_after_duration("not a date"), None);
}
