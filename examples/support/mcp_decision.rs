//! One immutable candidate, one action, then a cancellable local observation loop.
use super::input::CaptureProof;
use super::{Session, active, capture_region, decode, png, rect, screen_region, text};
use autogui::{Error, Rect, Result, Screenshot};
use base64::Engine as _;
use rsgui_decision_core::{ActionDecision, Candidate, Decision, DecisionInput, Engine, Facts};
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
            || self.stable_ms > self.timeout_ms
        {
            return Err(Error::InvalidArgument(
                "wait: timeout 50..5000, stable 25..1000, poll 5..100 milliseconds; poll <= stable <= timeout",
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
            Self::Click { x, y } | Self::Scroll { x, y, .. } => {
                proof.point(&proof.target, &proof.id, *x, *y)?;
            }
            Self::TypeText { text } if text.is_empty() || text.chars().count() > 4096 => {
                return Err(Error::InvalidArgument(
                    "text must contain 1..4096 characters",
                ));
            }
            Self::Hotkey { keys }
                if keys.is_empty()
                    || keys.len() > 8
                    || keys.iter().any(|key| key.is_empty() || key.len() > 32) =>
            {
                return Err(Error::InvalidArgument(
                    "hotkey requires 1..8 canonical keys",
                ));
            }
            _ => {}
        }
        if let Self::Scroll { clicks, .. } = self
            && (*clicks == 0 || !(-100..=100).contains(clicks))
        {
            return Err(Error::InvalidArgument(
                "scroll clicks must be nonzero and within -100..100",
            ));
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
                self.decision_engine()?;
                Ok(text(
                    json!({"mode":self.decisions.engine.as_ref().map(Engine::mode),
                    "evaluations":self.decisions.evaluations,"action_attempts":self.decisions.action_attempts,
                    "pending_decision":self.decisions.pending.as_ref().map(|pending| &pending.id),
                    "provider_model":"gemini-decision-1","automatic_retries":false,
                    "scope":"bounded GUI decisions; local single-candidate mode is deterministic, not visual understanding"}),
                ))
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
                let meta = json!({"evaluation":evaluation});
                if let Some(pending) = self.decisions.pending.take() {
                    let mut output = self.act_and_observe(pending, wait, false, check)?;
                    if let Some(content) = output["content"].as_array_mut() {
                        content.insert(0, json!({"type":"text","text":meta.to_string()}));
                    }
                    Ok(output)
                } else {
                    Ok(text(meta))
                }
            }
            _ => Err(Error::InvalidArgument("unknown decision tool")),
        }
    }

    fn decision_engine(&mut self) -> Result<()> {
        if self.decisions.engine.is_none() {
            self.decisions.engine =
                Some(Engine::from_env().map_err(|e| Error::Input(e.to_string()))?);
        }
        Ok(())
    }

    fn verify_observation(
        &mut self,
        proof: &CaptureProof,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<()> {
        check()?;
        proof.point(&proof.target, &proof.id, 0, 0)?;
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
        if screen_region(gui, &window, display.as_ref())? != proof.region
            || capture_region(gui, display.as_ref(), proof.region)? != proof.frame
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
        let proof = self
            .capture
            .as_ref()
            .ok_or(Error::InvalidArgument(
                "capture/observe the selected window first",
            ))?
            .clone();
        if proof.target != request.target || proof.id != request.capture_id {
            return Err(Error::InvalidArgument("observation target/id mismatch"));
        }
        for candidate in &request.candidates {
            candidate.action.validate(&proof)?;
        }
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
        self.decision_engine()?;
        let image = if input.candidates.len() > 1 {
            let rgb = image::RgbImage::from_raw(
                proof.frame.width,
                proof.frame.height,
                proof.frame.pixels.clone(),
            )
            .ok_or(Error::InvalidArgument("invalid observation frame"))?;
            let mut bytes = std::io::Cursor::new(Vec::new());
            rgb.write_to(&mut bytes, image::ImageFormat::Png)
                .map_err(|e| Error::Screenshot(e.to_string()))?;
            if bytes.get_ref().len() > 12 * 1024 * 1024 {
                return Err(Error::InvalidArgument(
                    "decision image exceeds 12 MiB; resize the window",
                ));
            }
            Some(base64::engine::general_purpose::STANDARD.encode(bytes.into_inner()))
        } else {
            None
        };
        let decision = self
            .decisions
            .engine
            .as_mut()
            .ok_or(Error::InvalidArgument("decision engine unavailable"))?
            .evaluate(&input, image.as_deref(), &|| {
                check().map_err(std::io::Error::other)
            })
            .map_err(|e| Error::Input(e.to_string()))?;
        // Inference is advisory; changed pixels during the request invalidate its result.
        if decision.calls > 0 {
            self.verify_observation(&proof, check)?;
        }
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
                target: proof.target,
                capture_id: proof.id,
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
        let region = screen_region(gui, &window, display.as_ref())?;
        if i64::from(region.width) * i64::from(region.height) > 16_777_216 {
            return Err(Error::InvalidArgument(
                "capture exceeds 16 megapixels; resize window",
            ));
        }
        let pixels = capture_region(gui, display.as_ref(), region)?;
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
                self.capture_generation = self
                    .capture_generation
                    .checked_add(1)
                    .ok_or(Error::InvalidArgument("restart capture session"))?;
                let id = format!("c{}", self.capture_generation);
                let metadata = json!({"target":pending.target,"display":display,"region":rect(frame.region),
                    "capture_id":id,"pointer_coordinates":"image-relative pixels; origin at captured image top-left",
                    "expires_in_seconds":60,"single_use":true,"decision_id":pending.id,
                    "candidate_id":pending.candidate.id,"input_attempted":true,"input_sent":true,
                    "outcome":if settled {"settled"} else {"timeout"},"task_success":"not_evaluated",
                    "samples":samples,"latency_ms":started.elapsed().as_millis(),"action":action,
                    "note":"Visual stability is not semantic completion. Inspect this fresh capture before the next action."});
                let output = png(frame.pixels.clone(), metadata)?;
                self.capture = Some(CaptureProof {
                    id,
                    target: pending.target,
                    display,
                    bounds: frame.bounds,
                    region: frame.region,
                    frame: frame.pixels,
                    taken: frame.taken,
                });
                Ok(output)
            }
            Err(error) => {
                self.capture = None;
                Ok(
                    json!({"isError":true,"content":[{"type":"text","text":json!({
                    "decision_id":pending.id,"candidate_id":pending.candidate.id,"input_attempted":true,
                    "input_sent":true,"outcome":"observation_error","error":error.to_string(),
                    "action":action,"automatic_retry":false}).to_string()}]}),
                )
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
                unchanged_since = Instant::now();
                interval = policy.poll_ms;
            }
            if unchanged_since.elapsed() >= stable {
                return Ok((frame, true, samples));
            }
            if start.elapsed() >= timeout {
                return Ok((frame, false, samples));
            }
            previous = Some(frame);
            check()?;
            std::thread::sleep(
                Duration::from_millis(interval).min(timeout.saturating_sub(start.elapsed())),
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
            GuiAction::TypeText {
                text: String::new()
            }
            .validate(&proof())
            .is_err()
        );
    }
}
