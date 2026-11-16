//! One immutable candidate, one action, then a cancellable local observation loop.
use super::input::{CaptureProof, validate_hotkey, validate_type_text};
use super::{
    PauseGuard, Session, active, capture_region, decode, encode_png_base64, png, rect,
    screen_region, text,
};
use autogui::{Error, Rect, Result, Screenshot};
use rsgui_decision_core::{
    ActionDecision, Candidate, Decision, DecisionInput, Engine, Error as DecisionError, Facts, Mode,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

pub(super) const NAMES: &[&str] = &[
    "decision_observe",
    "decision_evaluate",
    "decision_act",
    "decision_step",
    "decision_status",
];

#[derive(Default)]
pub(super) struct State {
    engine: Option<Engine>,
    pub(super) pending: Option<Pending>,
    generation: u64,
    evaluations: u64,
    action_attempts: u64,
}

pub(super) struct Pending {
    id: String,
    target: String,
    capture_id: String,
    candidate: GuiCandidate,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum GuiAction {
    Click {
        x: i32,
        y: i32,
    },
    Scroll {
        x: i32,
        y: i32,
        clicks: i32,
        #[serde(default)]
        horizontal: bool,
    },
    TypeText {
        text: String,
    },
    Hotkey {
        keys: Vec<String>,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct GuiCandidate {
    id: String,
    description: String,
    action: GuiAction,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Evaluate {
    target: String,
    capture_id: String,
    goal: String,
    candidates: Vec<GuiCandidate>,
    #[serde(default = "default_confidence")]
    minimum_confidence: f64,
}
fn default_confidence() -> f64 {
    0.85
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct WaitPolicy {
    timeout_ms: u64,
    stable_ms: u64,
    poll_ms: u64,
}
impl Default for WaitPolicy {
    fn default() -> Self {
        Self {
            timeout_ms: 1500,
            stable_ms: 100,
            poll_ms: 25,
        }
    }
}
impl WaitPolicy {
    fn validate(self) -> Result<Self> {
        if !(50..=5000).contains(&self.timeout_ms)
            || !(25..=1000).contains(&self.stable_ms)
            || !(5..=100).contains(&self.poll_ms)
            || self.poll_ms > self.stable_ms
            || self.stable_ms >= self.timeout_ms
        {
            return Err(Error::InvalidArgument(
                "wait: timeout 50..5000, stable 25..1000, poll 5..100 milliseconds; poll <= stable < timeout",
            ));
        }
        Ok(self)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Act {
    decision_id: String,
    #[serde(default)]
    wait: WaitPolicy,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Step {
    request: Evaluate,
    #[serde(default)]
    wait: WaitPolicy,
}
#[derive(Serialize)]
struct Evaluation {
    decision_id: Option<String>,
    decision: Decision,
    note: &'static str,
}

struct Frame {
    bounds: Rect,
    region: Rect,
    pixels: Screenshot,
    taken: Instant,
}

impl GuiAction {
    fn validate(&self, proof: &CaptureProof) -> Result<()> {
        match self {
            Self::Click { x, y } => {
                proof.point(&proof.target, &proof.id, *x, *y)?;
            }
            Self::Scroll { x, y, clicks, .. } => {
                proof.point(&proof.target, &proof.id, *x, *y)?;
                if *clicks == 0 || !(-100..=100).contains(clicks) {
                    return Err(Error::InvalidArgument(
                        "scroll clicks must be nonzero and within -100..100",
                    ));
                }
            }
            Self::TypeText { text } => validate_type_text(text, 1)?,
            Self::Hotkey { keys } => validate_hotkey(keys)?,
        }
        Ok(())
    }

    fn native(&self, target: &str, capture_id: &str) -> (&'static str, Value) {
        match self {
            Self::Click { x, y } => (
                "click_at",
                json!({"target":target,"capture_id":capture_id,"x":x,"y":y}),
            ),
            Self::Scroll {
                x,
                y,
                clicks,
                horizontal,
            } => (
                "scroll",
                json!({"target":target,"capture_id":capture_id,"x":x,"y":y,"clicks":clicks,"horizontal":horizontal}),
            ),
            Self::TypeText { text } => ("type_text_passive", json!({"target":target,"text":text})),
            Self::Hotkey { keys } => ("hotkey_passive", json!({"target":target,"keys":keys})),
        }
    }
}

fn map_decision_error(error: DecisionError) -> Error {
    match error {
        DecisionError::Refused => {
            Error::PermissionDenied("GEMINI_REFUSED: Gemini refused to answer a decision question")
        }
        other => Error::Input(format!("{}: {other}", other.code())),
    }
}

fn merge_step_evaluation(output: &mut Value, evaluation: &Evaluation) {
    let Some(content) = output["content"].as_array_mut() else {
        return;
    };
    if let Some(first) = content.first_mut()
        && let Some(raw) = first["text"].as_str()
        && let Ok(mut obj) = serde_json::from_str::<serde_json::Map<String, Value>>(raw)
    {
        obj.insert("evaluation".into(), json!(evaluation));
        first["text"] = json!(Value::Object(obj).to_string());
    } else {
        content.push(json!({"type":"text","text":json!({"evaluation":evaluation}).to_string()}));
    }
}

fn observation_error(pending: &Pending, action: Value, error: &Error) -> Value {
    json!({"isError":true,"content":[{"type":"text","text":json!({
        "decision_id":pending.id,"candidate_id":pending.candidate.id,"input_attempted":true,
        "input_sent":true,"outcome":"observation_error","error":error.to_string(),
        "action":action,"automatic_retry":false}).to_string()}]})
}

impl Session {
    pub(super) fn decision_call(
        &mut self,
        name: &str,
        args: Value,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Value> {
        match name {
            "decision_status" => {
                let _: super::Empty = decode(args)?;
                let probe;
                let engine = if let Some(engine) = self.decisions.engine.as_ref() {
                    engine
                } else {
                    probe = Engine::from_env().map_err(map_decision_error)?;
                    &probe
                };
                let mode = engine.mode();
                let provider_model = (mode != Mode::Local).then_some("gemini-decision-1");
                Ok(text(json!({"mode":mode,
                    "evaluations":self.decisions.evaluations,"action_attempts":self.decisions.action_attempts,
                    "pending_decision":self.decisions.pending.as_ref().map(|pending| &pending.id),
                    "provider_model":provider_model,
                    "provider_ready":engine.has_provider(),
                    "hook":engine.hook_name(),
                    "gemini_decision_1":engine.discovery(),
                    "automatic_retries":false,
                    "scope":"bounded GUI decisions; auto prioritizes Gemini Decision 1 when available and falls back safely; local single-candidate mode is deterministic"})))
            }
            "decision_observe" => self.call("observe", args, check),
            "decision_evaluate" => {
                self.decisions.pending = None;
                let request: Evaluate = decode(args)?;
                let value = self.evaluate(request, check)?;
                Ok(text(
                    serde_json::to_value(value).map_err(|e| Error::Input(e.to_string()))?,
                ))
            }
            "decision_act" => {
                // Even a malformed execution attempt consumes the previous authorization.
                let pending = self.decisions.pending.take();
                let request: Act = decode(args)?;
                let wait = request.wait.validate()?;
                let pending = pending.ok_or(Error::InvalidArgument(
                    "no unused decision; evaluate a fresh capture",
                ))?;
                if request.decision_id != pending.id {
                    return Err(Error::InvalidArgument(
                        "unknown/stale decision id; evaluate again",
                    ));
                }
                self.act_and_observe(pending, wait, true, check)
            }
            "decision_step" => {
                self.decisions.pending = None;
                let request: Step = decode(args)?;
                let wait = request.wait.validate()?;
                let evaluation = self.evaluate(request.request, check)?;
                if let Some(pending) = self.decisions.pending.take() {
                    let mut output = self.act_and_observe(pending, wait, false, check)?;
                    merge_step_evaluation(&mut output, &evaluation);
                    Ok(output)
                } else {
                    Ok(text(json!({"evaluation":evaluation})))
                }
            }
            _ => Err(Error::InvalidArgument("unknown decision tool")),
        }
    }

    fn decision_engine(&mut self) -> Result<()> {
        if self.decisions.engine.is_none() {
            self.decisions.engine = Some(Engine::from_env().map_err(map_decision_error)?);
        }
        Ok(())
    }

    fn verify_observation(
        &mut self,
        proof: &CaptureProof,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<()> {
        check()?;
        proof.fresh()?;
        let window = self.target(&proof.target)?;
        let display = proof
            .display
            .as_deref()
            .map(|key| self.display(key))
            .transpose()?;
        active(&window)?;
        if window.box_rect()? != proof.bounds {
            return Err(Error::InvalidArgument(
                "window moved since observation; observe again",
            ));
        }
        let gui = self.gui()?;
        let mut guard = PauseGuard::zero(gui);
        if screen_region(guard.gui(), &window, display.as_ref())? != proof.region
            || capture_region(guard.gui(), display.as_ref(), proof.region)? != proof.frame
        {
            return Err(Error::InvalidArgument(
                "observed pixels/display changed; observe again",
            ));
        }
        active(&window)?;
        if window.box_rect()? != proof.bounds {
            return Err(Error::InvalidArgument(
                "window moved during verification; observe again",
            ));
        }
        check()?;
        Ok(())
    }

    fn evaluate(
        &mut self,
        request: Evaluate,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Evaluation> {
        {
            let proof = self.capture.as_ref().ok_or(Error::InvalidArgument(
                "capture/observe the selected window first",
            ))?;
            if proof.target != request.target || proof.id != request.capture_id {
                return Err(Error::InvalidArgument("observation target/id mismatch"));
            }
            for candidate in &request.candidates {
                candidate.action.validate(proof)?;
            }
        }
        if request.goal.trim().is_empty() || request.goal.chars().count() > 16_000 {
            return Err(Error::InvalidArgument(
                "goal must contain 1..16000 characters",
            ));
        }
        if !(1..=32).contains(&request.candidates.len()) {
            return Err(Error::InvalidArgument("there must be 1..32 candidates"));
        }
        if !request.minimum_confidence.is_finite()
            || !(0.5..=1.0).contains(&request.minimum_confidence)
        {
            return Err(Error::InvalidArgument(
                "minimum_confidence must be finite and 0.5..1",
            ));
        }
        self.decision_engine()?;
        let proof = self.capture.take().ok_or(Error::InvalidArgument(
            "capture/observe the selected window first",
        ))?;
        // Drift/staleness invalidates self.capture by leaving it None.
        self.verify_observation(&proof, check)?;
        let candidates = request
            .candidates
            .iter()
            .map(|candidate| Candidate {
                id: candidate.id.clone(),
                description: candidate.description.clone(),
            })
            .collect::<Vec<_>>();
        // One caller-selected action is a deterministic request, not an inferred visual label.
        let facts = if let [candidate] = candidates.as_slice() {
            Facts::Ready {
                candidate_id: candidate.id.clone(),
            }
        } else {
            Facts::Unknown
        };
        let input = DecisionInput {
            goal: request.goal,
            candidates,
            facts,
            minimum_confidence: request.minimum_confidence,
        };
        let needs_image = self
            .decisions
            .engine
            .as_ref()
            .ok_or(Error::InvalidArgument("decision engine unavailable"))?
            .needs_image(&input);
        let image = if needs_image {
            match encode_png_base64(&proof.frame) {
                Ok(data) => Some(data),
                Err(error) => {
                    self.capture = Some(proof);
                    return Err(error);
                }
            }
        } else {
            None
        };
        let decision = match self
            .decisions
            .engine
            .as_mut()
            .ok_or(Error::InvalidArgument("decision engine unavailable"))?
            .evaluate(&input, image.as_deref(), &|| {
                check().map_err(std::io::Error::other)
            }) {
            Ok(decision) => decision,
            Err(error) => {
                self.capture = Some(proof);
                return Err(map_decision_error(error));
            }
        };
        // Inference is advisory; changed pixels during the request invalidate its result.
        if decision.calls > 0 {
            self.verify_observation(&proof, check)?;
        }
        let target = proof.target.clone();
        let capture_id = proof.id.clone();
        self.capture = Some(proof);
        self.decisions.evaluations += 1;
        let mut decision_id = None;
        if decision.action == ActionDecision::Choose {
            let candidate = request
                .candidates
                .into_iter()
                .find(|candidate| Some(&candidate.id) == decision.candidate_id.as_ref())
                .ok_or(Error::InvalidArgument(
                    "decision chose an unknown candidate",
                ))?;
            self.decisions.generation = self
                .decisions
                .generation
                .checked_add(1)
                .ok_or(Error::InvalidArgument("restart decision session"))?;
            let id = format!("d{}", self.decisions.generation);
            decision_id = Some(id.clone());
            self.decisions.pending = Some(Pending {
                id,
                target,
                capture_id,
                candidate,
            });
        }
        Ok(Evaluation {
            decision_id,
            decision,
            note: "Advisory selection only. An execution attempt consumes it. Pixels and target are rechecked before input; settled pixels do not prove task completion.",
        })
    }

    fn passive_frame(
        &mut self,
        target: &str,
        display_key: Option<&str>,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Frame> {
        check()?;
        let window = self.target(target)?;
        let display = display_key.map(|key| self.display(key)).transpose()?;
        active(&window)?;
        let bounds = window.box_rect()?;
        let gui = self.gui()?;
        let mut guard = PauseGuard::zero(gui);
        let region = screen_region(guard.gui(), &window, display.as_ref())?;
        if i64::from(region.width) * i64::from(region.height) > 16_777_216 {
            return Err(Error::InvalidArgument(
                "capture exceeds 16 megapixels; resize window",
            ));
        }
        let pixels = capture_region(guard.gui(), display.as_ref(), region)?;
        let taken = Instant::now();
        active(&window)?;
        if window.box_rect()? != bounds {
            return Err(Error::InvalidArgument("window moved during observation"));
        }
        check()?;
        Ok(Frame {
            bounds,
            region,
            pixels,
            taken,
        })
    }

    fn act_and_observe(
        &mut self,
        pending: Pending,
        wait: WaitPolicy,
        recheck: bool,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Value> {
        let proof = self.capture.take().ok_or(Error::InvalidArgument(
            "observation was invalidated; observe/evaluate again",
        ))?;
        if proof.id != pending.capture_id || proof.target != pending.target {
            return Err(Error::InvalidArgument(
                "decision observation no longer matches",
            ));
        }
        if recheck {
            self.verify_observation(&proof, check)?;
        }
        let display = proof.display.clone();
        let (name, args) = pending.candidate.action.native(&pending.target, &proof.id);
        self.capture = Some(proof);
        self.decisions.action_attempts += 1;
        let started = Instant::now();
        let action = match self.call(name, args, check) {
            Ok(value) => value,
            Err(error) => {
                self.capture = None;
                return Ok(
                    json!({"isError":true,"content":[{"type":"text","text":json!({
                    "decision_id":pending.id,"candidate_id":pending.candidate.id,
                    "input_attempted":true,"input_sent":"unknown_or_partial","outcome":"action_error",
                    "error":error.to_string(),"automatic_retry":false}).to_string()}]}),
                );
            }
        };
        let observed = self.wait_for_frame(&pending.target, display.as_deref(), wait, check);
        match observed {
            Ok((frame, settled, samples)) => {
                let Some(next_generation) = self.capture_generation.checked_add(1) else {
                    self.capture = None;
                    return Ok(observation_error(
                        &pending,
                        action,
                        &Error::InvalidArgument("restart capture session"),
                    ));
                };
                let id = format!("c{next_generation}");
                let metadata = json!({"target":pending.target,"display":display,"region":rect(frame.region),
                    "capture_id":id,"pointer_coordinates":"image-relative pixels; origin at captured image top-left",
                    "expires_in_seconds":60,"single_use":true,"decision_id":pending.id,
                    "candidate_id":pending.candidate.id,"input_attempted":true,"input_sent":true,
                    "outcome":if settled {"settled"} else {"timeout"},"task_success":"not_evaluated",
                    "samples":samples,"latency_ms":started.elapsed().as_millis(),"action":action,
                    "note":"Visual stability is not semantic completion. Inspect this fresh capture before the next action."});
                let output = match png(&frame.pixels, metadata) {
                    Ok(output) => output,
                    Err(error) => {
                        self.capture = None;
                        return Ok(observation_error(&pending, action, &error));
                    }
                };
                self.capture_generation = next_generation;
                self.capture = Some(CaptureProof {
                    id,
                    target: pending.target,
                    display,
                    bounds: frame.bounds,
                    region: frame.region,
                    frame: frame.pixels,
                    taken: Instant::now(),
                });
                Ok(output)
            }
            Err(error) => {
                self.capture = None;
                Ok(observation_error(&pending, action, &error))
            }
        }
    }

    fn wait_for_frame(
        &mut self,
        target: &str,
        display: Option<&str>,
        policy: WaitPolicy,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<(Frame, bool, u64)> {
        let start = Instant::now();
        let timeout = Duration::from_millis(policy.timeout_ms);
        let stable = Duration::from_millis(policy.stable_ms);
        let mut unchanged_since = Instant::now();
        let mut previous: Option<Frame> = None;
        let mut interval = policy.poll_ms;
        let mut samples = 0;
        loop {
            let frame = self.passive_frame(target, display, check)?;
            samples += 1;
            if previous.as_ref().is_none_or(|old| {
                old.bounds != frame.bounds
                    || old.region != frame.region
                    || old.pixels != frame.pixels
            }) {
                unchanged_since = frame.taken;
                interval = policy.poll_ms;
            }
            if previous.is_some() && unchanged_since.elapsed() >= stable {
                return Ok((frame, true, samples));
            }
            if start.elapsed() >= timeout {
                return Ok((frame, false, samples));
            }
            previous = Some(frame);
            check()?;
            let remaining_stable = stable.saturating_sub(unchanged_since.elapsed());
            let remaining_timeout = timeout.saturating_sub(start.elapsed());
            std::thread::sleep(
                Duration::from_millis(interval)
                    .min(remaining_stable)
                    .min(remaining_timeout),
            );
            interval = (interval * 2).min(100);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proof() -> CaptureProof {
        CaptureProof {
            id: "c1".into(),
            target: "w1".into(),
            display: None,
            bounds: Rect::new(10, 10, 100, 100),
            region: Rect::new(10, 10, 100, 100),
            frame: Screenshot::solid(100, 100, autogui::Rgb::new(1, 2, 3)),
            taken: Instant::now(),
        }
    }

    #[test]
    fn reject_coordinates_without_input() {
        let proof = proof();
        for (x, y) in [(-1, 0), (100, 0), (0, 100), (i32::MAX, 0)] {
            assert!(GuiAction::Click { x, y }.validate(&proof).is_err());
        }
        assert!(GuiAction::Click { x: 0, y: 0 }.validate(&proof).is_ok());
        let expired = CaptureProof {
            taken: Instant::now() - Duration::from_secs(61),
            ..proof
        };
        assert!(GuiAction::Click { x: 1, y: 1 }.validate(&expired).is_err());
    }

    #[test]
    fn execution_attempt_consumes_authorization_before_decoding() {
        let mut session = Session::default();
        session.decisions.pending = Some(Pending {
            id: "d1".into(),
            target: "w1".into(),
            capture_id: "c1".into(),
            candidate: GuiCandidate {
                id: "click".into(),
                description: "test".into(),
                action: GuiAction::Click { x: 1, y: 1 },
            },
        });
        assert!(
            session
                .decision_call(
                    "decision_act",
                    json!({"decision_id":"d1","extra":true}),
                    &|| Ok(())
                )
                .is_err()
        );
        assert!(session.decisions.pending.is_none());
        assert_eq!(session.decisions.action_attempts, 0);
    }

    #[test]
    fn changing_native_targets_invalidates_pending_decision_even_on_failure() {
        let mut session = Session::default();
        session.decisions.pending = Some(Pending {
            id: "d1".into(),
            target: "w1".into(),
            capture_id: "c1".into(),
            candidate: GuiCandidate {
                id: "click".into(),
                description: "test".into(),
                action: GuiAction::Click { x: 1, y: 1 },
            },
        });
        assert!(
            session
                .call("windows_list", json!({"invalid":true}), &|| Ok(()))
                .is_err()
        );
        assert!(session.decisions.pending.is_none());
        assert_eq!(session.decisions.action_attempts, 0);
    }

    #[test]
    fn stale_observation_and_unknown_decision_are_rejected_before_desktop_access() {
        let mut session = Session {
            capture: Some(proof()),
            ..Default::default()
        };
        let args = json!({"target":"w1","capture_id":"old","goal":"test", "candidates":[{
            "id":"click","description":"test","action":{"type":"click","x":1,"y":1}}]});
        assert!(
            session
                .decision_call("decision_evaluate", args, &|| Ok(()))
                .is_err()
        );
        assert!(
            session
                .decision_call("decision_act", json!({"decision_id":"unknown"}), &|| Ok(()))
                .is_err()
        );
        assert!(
            session
                .decision_call(
                    "decision_evaluate",
                    json!({"target":"w1","capture_id":"c1","goal":"   ","candidates":[{
                        "id":"click","description":"test","action":{"type":"click","x":1,"y":1}}]}),
                    &|| Ok(())
                )
                .is_err()
        );
        assert!(
            session
                .decision_call(
                    "decision_evaluate",
                    json!({"target":"w1","capture_id":"c1","goal":"test","candidates":[{
                        "id":"hotkey","description":"test","action":{"type":"hotkey","keys":["not_a_key"]}}]}),
                    &|| Ok(())
                )
                .is_err()
        );
        assert!(session.gui.is_none());
        assert!(session.capture.is_some());
        assert_eq!(session.decisions.action_attempts, 0);
    }

    #[test]
    fn wait_limits_and_closed_action_schema() {
        assert!(WaitPolicy::default().validate().is_ok());
        assert!(
            WaitPolicy {
                timeout_ms: 100,
                stable_ms: 1000,
                poll_ms: 25
            }
            .validate()
            .is_err()
        );
        assert!(
            WaitPolicy {
                timeout_ms: 50,
                stable_ms: 25,
                poll_ms: 0
            }
            .validate()
            .is_err()
        );
        assert!(
            serde_json::from_value::<GuiAction>(json!({"type":"shell","command":"x"})).is_err()
        );
        assert!(
            serde_json::from_value::<GuiAction>(json!({"type":"click","x":1,"y":1,"command":"x"}))
                .is_err()
        );
        assert!(
            WaitPolicy {
                timeout_ms: 100,
                stable_ms: 100,
                poll_ms: 25
            }
            .validate()
            .is_err()
        );
        assert!(
            GuiAction::TypeText {
                text: String::new()
            }
            .validate(&proof())
            .is_err()
        );
        assert!(
            GuiAction::TypeText {
                text: "hello\0world".into()
            }
            .validate(&proof())
            .is_err()
        );
        assert!(
            GuiAction::Hotkey {
                keys: vec!["ctrl".into(), "not_a_key".into()]
            }
            .validate(&proof())
            .is_err()
        );
        assert!(
            GuiAction::Hotkey {
                keys: vec!["ctrl".into(), "insert".into()]
            }
            .validate(&proof())
            .is_ok()
        );
    }

    #[test]
    fn step_merges_evaluation_into_content_zero_and_status_stays_read_only() {
        let mut session = Session::default();
        let status = session
            .decision_call("decision_status", json!({}), &|| Ok(()))
            .unwrap();
        assert!(session.decisions.engine.is_none());
        let status_json: Value =
            serde_json::from_str(status["content"][0]["text"].as_str().unwrap()).unwrap();
        let expected_mode = match std::env::var("RSGUI_DECISION_MODE")
            .ok()
            .map(|value| value.trim().to_ascii_lowercase())
            .as_deref()
        {
            Some("local") => "local",
            Some("gemini") => "gemini",
            _ => "auto",
        };
        assert_eq!(status_json["mode"], expected_mode);
        if expected_mode == "local" {
            assert!(status_json["provider_model"].is_null());
        } else {
            assert_eq!(status_json["provider_model"], "gemini-decision-1");
        }
        assert_eq!(status_json["hook"], "prefer_gemini_decision_1");
        assert!(status_json["gemini_decision_1"]["available"].is_boolean());

        let pending = Pending {
            id: "d1".into(),
            target: "w1".into(),
            capture_id: "c1".into(),
            candidate: GuiCandidate {
                id: "click".into(),
                description: "test".into(),
                action: GuiAction::Click { x: 1, y: 1 },
            },
        };
        let mut out = observation_error(
            &pending,
            json!({"clicked":true}),
            &Error::InvalidArgument("boom"),
        );
        let eval = Evaluation {
            decision_id: Some("d1".into()),
            decision: Decision {
                action: ActionDecision::Choose,
                candidate_id: Some("click".into()),
                confidence: None,
                source: "local".into(),
                calls: 0,
                latency_ms: 0,
                input_tokens: None,
                estimated_cost_usd: None,
                error: None,
                error_code: None,
            },
            note: "note",
        };
        merge_step_evaluation(&mut out, &eval);
        let merged: Value =
            serde_json::from_str(out["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(merged["outcome"], "observation_error");
        assert_eq!(merged["input_sent"], true);
        assert_eq!(merged["evaluation"]["decision_id"], "d1");
    }
}
