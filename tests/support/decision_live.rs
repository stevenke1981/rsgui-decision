//! Live evidence uses only the parent test's owned Win32 fixture.
use super::*;
use serde_json::{Value, json};

fn metadata(output: &Value, key: &str) -> Value {
    output["content"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|block| {
            block["text"]
                .as_str()
                .and_then(|text| serde_json::from_str::<Value>(text).ok())
        })
        .find(|value| value.get(key).is_some())
        .unwrap_or_else(|| panic!("missing {key}: {output}"))
}

fn request(target: &str, capture: &Value, action: Value) -> Value {
    json!({"target":target,"capture_id":capture["capture_id"],"goal":"Operate only the owned test fixture",
        "candidates":[{"id":"fixture_action","description":"The concrete fixture action selected by the test operator","action":action}]})
}

#[test]
#[ignore = "requires AUTOGUI_LIVE=1 and AUTOGUI_MCP_EXE; real input only to its owned test windows"]
fn integrated_decision_native_fixture() {
    assert_eq!(std::env::var("AUTOGUI_LIVE").ok().as_deref(), Some("1"));
    let executable = std::env::var("AUTOGUI_MCP_EXE").expect("built release executable");
    let _dpi = DpiScope::set(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    let mut gui = AutoGui::new().unwrap();
    let pointer = gui.position().unwrap();
    assert!(!gui.settings().failsafe_points.contains(&pointer));
    let title = format!("rsgui decision fixture {}", std::process::id());
    let fixture = Fixture::open(&title, pointer);
    let window = gui.get_windows_with_title(&title).unwrap().remove(0);
    if std::env::var("RSGUI_LIVE_MANUAL_START").ok().as_deref() == Some("1") {
        unsafe {
            SetWindowTextW(fixture.handles.button as HWND, wide("開始測試").as_ptr());
            UpdateWindow(fixture.handles.button as HWND);
        }
        eprintln!(
            "WAIT_FOR_OPERATOR_START {title}: click 開始測試; the fixture remains open for ten minutes"
        );
        let waiting = Instant::now();
        while fixture.clicks.load(Ordering::SeqCst) == 0 || !window.is_active().unwrap() {
            assert!(
                waiting.elapsed() < Duration::from_secs(600),
                "operator start not received: clicks={}, active={}",
                fixture.clicks.load(Ordering::SeqCst),
                window.is_active().unwrap()
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        fixture.clicks.store(0, Ordering::SeqCst);
        unsafe {
            SetWindowTextW(
                fixture.handles.button as HWND,
                wide("辨識測試按鈕 7X").as_ptr(),
            );
            UpdateWindow(fixture.handles.button as HWND);
        }
    } else if let Err(error) = window.activate() {
        eprintln!(
            "WAIT_FOR_FIXTURE_FOCUS {title}: {error}; foreground only this test-owned window through the supported desktop surface"
        );
        let waiting = Instant::now();
        while !window.is_active().unwrap() {
            assert!(
                waiting.elapsed() < Duration::from_secs(60),
                "test fixture needs explicit foreground activation; no focus restrictions were changed"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    // Initial activation/hover animation must settle before a pixel-bound proof.
    let waiting = Instant::now();
    let mut stable_since = Instant::now();
    let mut previous = gui.screenshot_region(window.box_rect().unwrap()).unwrap();
    loop {
        std::thread::sleep(Duration::from_millis(50));
        let current = gui.screenshot_region(window.box_rect().unwrap()).unwrap();
        if current != previous {
            stable_since = Instant::now();
        }
        if stable_since.elapsed() >= Duration::from_millis(250) {
            break;
        }
        assert!(
            waiting.elapsed() < Duration::from_secs(5),
            "fixture startup pixels did not settle"
        );
        previous = current;
    }
    let button = autogui::center(native_rect(fixture.handles.button));
    let primary = autogui::Display::all()
        .unwrap()
        .into_iter()
        .find(autogui::Display::is_primary)
        .unwrap();
    let mut primary_moves_match = true;
    let configured_pause = gui.settings().pause;
    gui.settings_mut().pause = Duration::ZERO;
    for offset in [-6, 0, 6] {
        assert!(
            window.is_active().unwrap(),
            "owned fixture lost focus before pointer diagnostics"
        );
        let expected = Point::new(button.x + offset, button.y + offset);
        gui.move_to_xy(expected.x, expected.y).unwrap();
        let injected = Instant::now();
        let mut default_actual = gui.position().unwrap();
        for sample_ms in [0, 2, 10, 25, 100] {
            std::thread::sleep(Duration::from_millis(sample_ms).saturating_sub(injected.elapsed()));
            default_actual = gui.position().unwrap();
            let mut physical = windows_sys::Win32::Foundation::POINT::default();
            let physical_ok = unsafe { GetPhysicalCursorPos(&mut physical) };
            eprintln!(
                "POINTER_SAMPLE expected={expected:?} requested_ms={sample_ms} elapsed_ms={} actual={default_actual:?} physical_ok={physical_ok} physical=({}, {})",
                injected.elapsed().as_millis(),
                physical.x,
                physical.y
            );
        }
        gui.move_to_display(&primary, expected).unwrap();
        let explicit_actual = gui.position().unwrap();
        eprintln!(
            "POINTER_DIAGNOSTIC expected={expected:?} default={default_actual:?} explicit_display={explicit_actual:?}"
        );
        primary_moves_match &= default_actual == expected;
        assert_eq!(explicit_actual, expected, "explicit physical display move");
    }
    assert!(
        primary_moves_match,
        "cursor was not at the requested point after 100 ms; no click sent"
    );
    gui.settings_mut().pause = configured_pause;
    gui.move_to_xy(button.x, button.y).unwrap();
    let mut client = mcp_client::Client::start(&executable);
    let list = client.tool("windows_list", json!({"title":title}));
    let target = list["windows"][0]["target"].as_str().unwrap().to_owned();
    let bounds = native_rect(fixture.handles.window);
    let click = json!({"type":"click","x":button.x-bounds.left,"y":button.y-bounds.top});
    let captured = client.tool("capture", json!({"target":target}));

    let mut multiple = request(&target, &captured, click.clone());
    multiple["candidates"].as_array_mut().unwrap().push(json!({"id":"other_action","description":"A second unproven action","action":{"type":"hotkey","keys":["f24"]}}));
    let decision = client.tool("decision_evaluate", multiple);
    assert_eq!(decision["decision"]["action"], "observe");
    assert!(decision["decision_id"].is_null());
    assert_eq!(fixture.clicks.load(Ordering::SeqCst), 0);

    let evaluated = client.tool(
        "decision_evaluate",
        request(&target, &captured, click.clone()),
    );
    assert_eq!(evaluated["decision"]["source"], "local");
    let id = evaluated["decision_id"].as_str().unwrap();
    // An execution request cannot replace the saved action, and consumes the token.
    assert_eq!(
        client.raw_tool(
            "decision_act",
            json!({"decision_id":id,"action":{"type":"hotkey","keys":["f24"]}})
        )["isError"],
        true
    );
    assert_eq!(
        client.raw_tool("decision_act", json!({"decision_id":id}))["isError"],
        true
    );
    assert_eq!(fixture.clicks.load(Ordering::SeqCst), 0);

    let output = client.raw_tool(
        "decision_step",
        json!({"request":request(&target,&captured,click.clone())}),
    );
    assert_eq!(output["isError"], false, "{output}");
    let after: Value =
        serde_json::from_str(output["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(after["input_sent"], true);
    assert_eq!(after["outcome"], "settled");
    assert_eq!(after["task_success"], "not_evaluated");
    assert_eq!(after["evaluation"]["decision"]["action"], "choose");
    assert_eq!(output["content"][1]["type"], "image");
    assert_eq!(
        metadata(&output, "capture_id")["capture_id"],
        after["capture_id"]
    );
    wait_until(|| fixture.clicks.load(Ordering::SeqCst) == 1);

    // A decision is tied to the complete observed frame, not just its click patch.
    let evaluated = client.tool("decision_evaluate", request(&target, &after, click.clone()));
    unsafe {
        SetWindowTextW(
            fixture.handles.button as HWND,
            wide("Changed after decision").as_ptr(),
        );
        UpdateWindow(fixture.handles.button as HWND);
    }
    let stale = client.raw_tool(
        "decision_act",
        json!({"decision_id":evaluated["decision_id"]}),
    );
    assert_eq!(stale["isError"], true, "{stale}");
    assert_eq!(fixture.clicks.load(Ordering::SeqCst), 1);

    let fresh = client.tool("decision_observe", json!({"target":target}));
    let evaluated = client.tool("decision_evaluate", request(&target, &fresh, click.clone()));
    window.move_rel(10, 0).unwrap();
    assert_eq!(
        client.raw_tool(
            "decision_act",
            json!({"decision_id":evaluated["decision_id"]})
        )["isError"],
        true
    );
    assert_eq!(fixture.clicks.load(Ordering::SeqCst), 1);

    // Native re-list invalidates old target/decision references.
    let fresh = client.tool("decision_observe", json!({"target":target}));
    let evaluated = client.tool(
        "decision_evaluate",
        request(&target, &fresh, json!({"type":"hotkey","keys":["f24"]})),
    );
    let list = client.tool("windows_list", json!({"title":title}));
    assert_eq!(
        client.raw_tool(
            "decision_act",
            json!({"decision_id":evaluated["decision_id"]})
        )["isError"],
        true
    );
    let target = list["windows"][0]["target"].as_str().unwrap().to_owned();

    // A passive observation refuses to reactivate a background target.
    let other_title = format!("rsgui focus fixture {}", std::process::id());
    let other = Fixture::open(&other_title, pointer);
    let other_window = gui.get_windows_with_title(&other_title).unwrap().remove(0);
    other_window.activate().unwrap();
    assert_eq!(
        client.raw_tool("decision_observe", json!({"target":target}))["isError"],
        true
    );
    assert!(other_window.is_active().unwrap());
    drop(other);
    window.activate().unwrap();

    // Unicode goes only to the fixture's focused edit; no model produces text.
    let edit = autogui::center(native_rect(fixture.handles.edit));
    gui.click_xy(edit.x, edit.y).unwrap();
    wait_until(|| unsafe { SendMessageW(fixture.handles.window as HWND, WM_APP, 0, 0) } != 0);
    let fresh = client.tool("capture", json!({"target":target}));
    let output = client.raw_tool(
        "decision_step",
        json!({"request":request(&target,&fresh,
        json!({"type":"type_text","text":"主人 Rust decision 中文🙂"}))}),
    );
    assert_eq!(output["isError"], false, "{output}");
    wait_until(|| native_text(fixture.handles.edit) == "主人 Rust decision 中文🙂");

    // Cancellation while waiting never repeats the already-delivered action.
    let button = autogui::center(native_rect(fixture.handles.button));
    gui.click_xy(button.x, button.y).unwrap();
    let fresh = client.tool("capture", json!({"target":target}));
    let output = client.cancelled_tool_after("decision_step",json!({"request":request(&target,&fresh,
        json!({"type":"hotkey","keys":["f24"]})),"wait":{"stable_ms":1000,"timeout_ms":2000,"poll_ms":25}}),Duration::from_millis(700));
    assert_eq!(output["isError"], true, "{output}");
    let before = fixture.clicks.load(Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(fixture.clicks.load(Ordering::SeqCst), before);
    let status = client.tool("decision_status", json!({}));
    assert!(status["pending_decision"].is_null());
    assert_eq!(status["mode"], "local");
    println!(
        "PASS integrated decision: immutable single-use actions, native click receipt, full-frame staleness, moved/relisted targets, passive focus, Unicode, cancellation; no provider calls"
    );

    // Same persistent MCP, same window/action/stability policy; no artificial host delay.
    let mut baseline = Vec::new();
    let mut integrated = Vec::new();
    let mut baseline_calls = 0;
    let mut integrated_calls = 0;
    for _ in 0..7 {
        let fresh = client.tool("capture", json!({"target":target}));
        let started = Instant::now();
        client.tool("hotkey", json!({"target":target,"keys":["f24"]}));
        baseline_calls += 1;
        let mut last = String::new();
        let mut unchanged = Instant::now();
        let mut pause = 25;
        loop {
            let frame = client.raw_tool("capture", json!({"target":target}));
            assert_eq!(frame["isError"], false);
            baseline_calls += 1;
            let image = frame["content"][1]["data"].as_str().unwrap();
            if last != image {
                last = image.to_owned();
                unchanged = Instant::now();
                pause = 25;
            }
            if unchanged.elapsed() >= Duration::from_millis(100) {
                break;
            }
            assert!(started.elapsed() < Duration::from_secs(3));
            let remaining = Duration::from_millis(100).saturating_sub(unchanged.elapsed());
            std::thread::sleep(Duration::from_millis(pause).min(remaining));
            pause = (pause * 2).min(100);
        }
        baseline.push(started.elapsed().as_secs_f64() * 1000.0);
        let _ = fresh;
        let fresh = client.tool("capture", json!({"target":target}));
        let started = Instant::now();
        let frame = client.raw_tool(
            "decision_step",
            json!({"request":request(&target,&fresh,
            json!({"type":"hotkey","keys":["f24"]}))}),
        );
        assert_eq!(frame["isError"], false, "{frame}");
        assert_eq!(metadata(&frame, "capture_id")["outcome"], "settled");
        integrated_calls += 1;
        integrated.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    baseline.sort_by(f64::total_cmp);
    integrated.sort_by(f64::total_cmp);
    let evidence = json!({"fixture":"owned Win32 window; local F24 hotkey; stable100ms; no injected host delay",
        "samples_per_path":7,"baseline":{"median_ms":baseline[3],"p95_ms":baseline[6],"calls":baseline_calls},
        "integrated":{"median_ms":integrated[3],"p95_ms":integrated[6],"calls":integrated_calls},
        "note":"Baseline/current integrated native MCP share this source snapshot. Integrated includes full-frame precondition checking. Seven samples describe this host only; p95 is the nearest-rank maximum."});
    println!("DECISION_BENCHMARK {evidence}");
    if let Ok(directory) = std::env::var("RSGUI_EVIDENCE_DIR") {
        let directory = std::path::PathBuf::from(directory);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join("decision-benchmark.json"),
            serde_json::to_vec_pretty(&evidence).unwrap(),
        )
        .unwrap();
    }
}
