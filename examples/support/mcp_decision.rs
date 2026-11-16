//! One immutable candidate, one action, then a cancellable local observation loop.
use super::input::{
    CaptureProof, frames_match_with_tolerance, validate_hotkey, validate_type_text,
};
use super::{
    PauseGuard, Session, active, capture_region, decode, encode_decision_png_base64,
    load_validated_template, locate_unique_centers, png, rect, screen_region, send_pointer_at,
    text,
};
use autogui::{AutoGui, Display, Error, Point, Rect, Result, Screenshot, Window};
use rsgui_decision_core::{
    ActionDecision, Candidate, Decision, DecisionInput, Engine, Error as DecisionError, Facts, Mode,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

const MAX_SELECT_FOLLOW_UP: usize = 8;

pub(super) const NAMES: &[&str] = &[
    "decision_observe",
    "decision_evaluate",
    "decision_act",
    "decision_step",
    "decision_select",
    "decision_status",
];

#[derive(Default)]
pub(super) struct State {
    engine: Option<Engine>,
    cached_gemini_cli: Option<bool>,
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

type PendingChoice = (String, String, GuiCandidate);
type EvaluationOutcome = (Evaluation, Option<PendingChoice>);

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
    allow_hover: bool,
    allow_caret_drift: bool,
}
impl Default for WaitPolicy {
    fn default() -> Self {
        Self {
            timeout_ms: 1500,
            stable_ms: 100,
            poll_ms: 25,
            allow_hover: true,
            allow_caret_drift: true,
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
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SelectTemplate {
    path: String,
    confidence: Option<f64>,
    exact: Option<bool>,
    scale: Option<f64>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SelectCandidate {
    id: String,
    description: String,
    action: Option<GuiAction>,
    template: Option<SelectTemplate>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum FollowUpAction {
    TypeText {
        text: String,
        #[serde(default)]
        delay_ms: u64,
    },
    Hotkey {
        keys: Vec<String>,
        #[serde(default)]
        delay_ms: u64,
    },
    Click {
        x: i32,
        y: i32,
        #[serde(default)]
        delay_ms: u64,
    },
    Scroll {
        x: i32,
        y: i32,
        clicks: i32,
        #[serde(default)]
        horizontal: bool,
        #[serde(default)]
        delay_ms: u64,
    },
    ClickTemplate {
        template: SelectTemplate,
        #[serde(default)]
        delay_ms: u64,
    },
}
fn default_return_image() -> bool {
    true
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SelectRequest {
    target: String,
    display: Option<String>,
    capture_id: Option<String>,
    #[serde(default)]
    activate: bool,
    goal: String,
    candidates: Vec<SelectCandidate>,
    #[serde(default)]
    follow_up: Vec<FollowUpAction>,
    #[serde(default = "default_confidence")]
    minimum_confidence: f64,
    #[serde(default = "default_return_image")]
    return_image: bool,
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

    fn validate_shape(&self) -> Result<()> {
        match self {
            Self::Click { .. } => Ok(()),
            Self::Scroll { clicks, .. } => {
                if *clicks == 0 || !(-100..=100).contains(clicks) {
                    return Err(Error::InvalidArgument(
                        "scroll clicks must be nonzero and within -100..100",
                    ));
                }
                Ok(())
            }
            Self::TypeText { text } => validate_type_text(text, 1),
            Self::Hotkey { keys } => validate_hotkey(keys),
        }
    }

    fn local_point(&self) -> Option<Point> {
        match self {
            Self::Click { x, y } | Self::Scroll { x, y, .. } => Some(Point::new(*x, *y)),
            Self::TypeText { .. } | Self::Hotkey { .. } => None,
        }
    }

    fn native(&self, target: &str, capture_id: &str, allow_hover: bool) -> (&'static str, Value) {
        match self {
            Self::Click { x, y } => (
                "click_at_decision",
                json!({"target":target,"capture_id":capture_id,"x":x,"y":y,"allow_hover":allow_hover}),
            ),
            Self::Scroll {
                x,
                y,
                clicks,
                horizontal,
            } => (
                "scroll_decision",
                json!({"target":target,"capture_id":capture_id,"x":x,"y":y,"clicks":clicks,"horizontal":horizontal,"allow_hover":allow_hover}),
            ),
            Self::TypeText { text } => ("type_text_passive", json!({"target":target,"text":text})),
            Self::Hotkey { keys } => ("hotkey_passive", json!({"target":target,"keys":keys})),
        }
    }
}

impl FollowUpAction {
    fn validate(&self) -> Result<Option<(Screenshot, Option<f32>)>> {
        let delay_ms = self.delay_ms();
        if delay_ms > 500 {
            return Err(Error::InvalidArgument("follow_up delay_ms must be 0..500"));
        }
        match self {
            Self::TypeText { text, .. } => {
                validate_type_text(text, 1)?;
                Ok(None)
            }
            Self::Hotkey { keys, .. } => {
                validate_hotkey(keys)?;
                Ok(None)
            }
            Self::Click { x, y, .. } => {
                if *x < 0 || *y < 0 {
                    return Err(Error::InvalidArgument(
                        "follow_up click coordinates must be non-negative",
                    ));
                }
                Ok(None)
            }
            Self::Scroll { x, y, clicks, .. } => {
                if *x < 0 || *y < 0 {
                    return Err(Error::InvalidArgument(
                        "follow_up scroll coordinates must be non-negative",
                    ));
                }
                if *clicks == 0 || !(-100..=100).contains(clicks) {
                    return Err(Error::InvalidArgument(
                        "scroll clicks must be nonzero and within -100..100",
                    ));
                }
                Ok(None)
            }
            Self::ClickTemplate { template, .. } => Ok(Some(load_validated_template(
                &template.path,
                template.confidence,
                template.exact,
                template.scale,
            )?)),
        }
    }

    fn validate_against_proof(&self, proof: &CaptureProof) -> Result<()> {
        match self {
            Self::Click { x, y, .. } | Self::Scroll { x, y, .. } => {
                proof.point(&proof.target, &proof.id, *x, *y)?;
                Ok(())
            }
            Self::TypeText { .. } | Self::Hotkey { .. } | Self::ClickTemplate { .. } => Ok(()),
        }
    }

    fn delay_ms(&self) -> u64 {
        match self {
            Self::TypeText { delay_ms, .. }
            | Self::Hotkey { delay_ms, .. }
            | Self::Click { delay_ms, .. }
            | Self::Scroll { delay_ms, .. }
            | Self::ClickTemplate { delay_ms, .. } => *delay_ms,
        }
    }
}

fn parse_max_image_dim() -> Result<u32> {
    match std::env::var("RSGUI_DECISION_MAX_IMAGE_DIM") {
        Ok(raw) => {
            let trimmed = raw.trim();
            if trimmed == "0" {
                return Ok(0);
            }
            trimmed
                .parse::<u32>()
                .ok()
                .filter(|v| (320..=3840).contains(v))
                .ok_or(Error::InvalidArgument(
                    "RSGUI_DECISION_MAX_IMAGE_DIM must be 0 or 320..3840",
                ))
        }
        Err(std::env::VarError::NotPresent) => Ok(1280),
        Err(std::env::VarError::NotUnicode(_)) => Err(Error::InvalidArgument(
            "RSGUI_DECISION_MAX_IMAGE_DIM must be valid Unicode",
        )),
    }
}

fn validate_goal_and_confidence(goal: &str, count: usize, min_conf: f64) -> Result<()> {
    if goal.trim().is_empty() || goal.chars().count() > 16_000 {
        return Err(Error::InvalidArgument(
            "goal must contain 1..16000 characters",
        ));
    }
    if !(1..=32).contains(&count) {
        return Err(Error::InvalidArgument("there must be 1..32 candidates"));
    }
    if !min_conf.is_finite() || !(0.5..=1.0).contains(&min_conf) {
        return Err(Error::InvalidArgument(
            "minimum_confidence must be finite and 0.5..1",
        ));
    }
    Ok(())
}

fn protected_points(candidates: &[GuiCandidate]) -> Vec<Point> {
    candidates
        .iter()
        .filter_map(|candidate| candidate.action.local_point())
        .collect()
}

fn sleep_checked(delay: Duration, check: &dyn Fn() -> Result<()>) -> Result<()> {
    if delay.is_zero() {
        return Ok(());
    }
    let deadline = Instant::now() + delay;
    loop {
        check()?;
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(());
        }
        std::thread::sleep(remaining.min(Duration::from_millis(25)));
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
                let max_image_dim = parse_max_image_dim()?;
                let probe;
                let engine = if let Some(engine) = self.decisions.engine.as_ref() {
                    engine
                } else {
                    probe = Engine::from_env_runtime().map_err(map_decision_error)?;
                    &probe
                };
                let mode = engine.mode();
                let provider_model = (mode != Mode::Local).then_some("gemini-decision-1");
                let discovery = Engine::probe_discovery_cached(
                    mode,
                    engine.has_provider(),
                    &mut self.decisions.cached_gemini_cli,
                )
                .map_err(map_decision_error)?;
                Ok(text(json!({"mode":mode,
                    "evaluations":self.decisions.evaluations,"action_attempts":self.decisions.action_attempts,
                    "pending_decision":self.decisions.pending.as_ref().map(|pending| &pending.id),
                    "provider_model":provider_model,
                    "provider_ready":engine.has_provider(),
                    "image_resolution":engine.image_resolution(),
                    "max_image_dim":max_image_dim,
                    "hook":engine.hook_name(),
                    "gemini_decision_1":discovery,
                    "automatic_retries":false,
                    "scope":"bounded GUI decisions; auto prioritizes Gemini Decision 1 when available and falls back safely; local single-candidate mode is deterministic"})))
            }
            "decision_observe" => {
                let out = self.call("observe", args, check)?;
                let _ = self.decision_engine();
                Ok(out)
            }
            "decision_evaluate" => {
                self.decisions.pending = None;
                let request: Evaluate = decode(args)?;
                let (value, chosen) = self.evaluate_with_drift(request, true, check)?;
                if let Some((capture_id, target, candidate)) = chosen {
                    self.store_pending(capture_id, target, candidate)?;
                }
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
                let (evaluation, chosen) =
                    self.evaluate_with_drift(request.request, wait.allow_caret_drift, check)?;
                if let Some((capture_id, target, candidate)) = chosen {
                    self.store_pending(capture_id, target, candidate)?;
                    if let Some(pending) = self.decisions.pending.take() {
                        let mut output = self.act_and_observe(pending, wait, false, check)?;
                        merge_step_evaluation(&mut output, &evaluation);
                        return Ok(output);
                    }
                }
                Ok(text(json!({"evaluation":evaluation})))
            }
            "decision_select" => {
                self.decisions.pending = None;
                let request: SelectRequest = decode(args)?;
                self.select(request, check)
            }
            _ => Err(Error::InvalidArgument("unknown decision tool")),
        }
    }

    fn store_pending(
        &mut self,
        capture_id: String,
        target: String,
        candidate: GuiCandidate,
    ) -> Result<()> {
        self.decisions.generation = self
            .decisions
            .generation
            .checked_add(1)
            .ok_or(Error::InvalidArgument("restart decision session"))?;
        let id = format!("d{}", self.decisions.generation);
        self.decisions.pending = Some(Pending {
            id,
            target,
            capture_id,
            candidate,
        });
        Ok(())
    }

    fn decision_engine(&mut self) -> Result<()> {
        if self.decisions.engine.is_none() {
            let engine = Engine::from_env_runtime().map_err(map_decision_error)?;
            engine.warm_up_client();
            self.decisions.engine = Some(engine);
        }
        Ok(())
    }

    fn verify_observation(
        &mut self,
        proof: &CaptureProof,
        protected: &[Point],
        allow_caret_drift: bool,
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
        let current_region = screen_region(guard.gui(), &window, display.as_ref())?;
        if current_region != proof.region {
            return Err(Error::InvalidArgument(
                "observed pixels/display changed; observe again",
            ));
        }
        let current_frame = capture_region(guard.gui(), display.as_ref(), proof.region)?;
        if !frames_match_with_tolerance(&proof.frame, &current_frame, protected, allow_caret_drift)
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

    fn evaluate_with_drift(
        &mut self,
        request: Evaluate,
        allow_caret_drift: bool,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<EvaluationOutcome> {
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
        validate_goal_and_confidence(
            &request.goal,
            request.candidates.len(),
            request.minimum_confidence,
        )?;
        self.decision_engine()?;
        let proof = self.capture.take().ok_or(Error::InvalidArgument(
            "capture/observe the selected window first",
        ))?;
        let protected = protected_points(&request.candidates);
        // Drift/staleness invalidates self.capture by leaving it None.
        self.verify_observation(&proof, &protected, allow_caret_drift, check)?;
        self.evaluate_proof(
            proof,
            request.goal,
            request.candidates,
            request.minimum_confidence,
            &protected,
            allow_caret_drift,
            check,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn evaluate_proof(
        &mut self,
        proof: CaptureProof,
        goal: String,
        gui_candidates: Vec<GuiCandidate>,
        minimum_confidence: f64,
        protected: &[Point],
        allow_caret_drift: bool,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<EvaluationOutcome> {
        let max_dim = match parse_max_image_dim() {
            Ok(dim) => dim,
            Err(error) => {
                self.capture = Some(proof);
                return Err(error);
            }
        };
        let candidates = gui_candidates
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
            goal,
            candidates,
            facts,
            minimum_confidence,
        };
        let needs_image = self
            .decisions
            .engine
            .as_ref()
            .ok_or(Error::InvalidArgument("decision engine unavailable"))?
            .needs_image(&input);
        let image = if needs_image {
            match encode_decision_png_base64(&proof.frame, max_dim) {
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
            self.verify_observation(&proof, protected, allow_caret_drift, check)?;
        }
        let target = proof.target.clone();
        let capture_id = proof.id.clone();
        self.capture = Some(proof);
        self.decisions.evaluations += 1;
        let mut decision_id = None;
        let mut chosen = None;
        if decision.action == ActionDecision::Choose {
            let candidate = gui_candidates
                .into_iter()
                .find(|candidate| Some(&candidate.id) == decision.candidate_id.as_ref())
                .ok_or(Error::InvalidArgument(
                    "decision chose an unknown candidate",
                ))?;
            let next_num = self
                .decisions
                .generation
                .checked_add(1)
                .ok_or(Error::InvalidArgument("restart decision session"))?;
            decision_id = Some(format!("d{next_num}"));
            chosen = Some((capture_id, target, candidate));
        }
        Ok((
            Evaluation {
                decision_id,
                decision,
                note: "Advisory selection only. An execution attempt consumes it. Pixels and target are rechecked before input; settled pixels do not prove task completion.",
            },
            chosen,
        ))
    }

    fn select(&mut self, request: SelectRequest, check: &dyn Fn() -> Result<()>) -> Result<Value> {
        let wait = request.wait.validate()?;
        if request.activate && request.capture_id.is_some() {
            return Err(Error::InvalidArgument(
                "activate cannot be combined with an existing capture_id",
            ));
        }
        validate_goal_and_confidence(
            &request.goal,
            request.candidates.len(),
            request.minimum_confidence,
        )?;
        if request.follow_up.len() > MAX_SELECT_FOLLOW_UP {
            return Err(Error::InvalidArgument(
                "follow_up supports at most 8 actions",
            ));
        }
        let mut preloaded_follow_up = Vec::with_capacity(request.follow_up.len());
        for step in &request.follow_up {
            preloaded_follow_up.push(step.validate()?);
        }
        let mut ids = std::collections::BTreeSet::new();
        let mut preloaded_templates = Vec::with_capacity(request.candidates.len());
        for candidate in &request.candidates {
            let bytes = candidate.id.as_bytes();
            if bytes.is_empty()
                || bytes.len() > 64
                || !bytes[0].is_ascii_lowercase()
                || !bytes
                    .iter()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_')
                || matches!(candidate.id.as_str(), "observe" | "wait" | "stop" | "done")
                || !ids.insert(candidate.id.as_str())
            {
                return Err(Error::InvalidArgument(
                    "candidate IDs must be distinct nonreserved ASCII identifiers",
                ));
            }
            if candidate.description.trim().is_empty()
                || candidate.description.chars().count() > 2048
            {
                return Err(Error::InvalidArgument(
                    "candidate descriptions must contain 1..2048 characters",
                ));
            }
            if candidate.action.is_none() && candidate.template.is_none() {
                return Err(Error::InvalidArgument(
                    "each select candidate must provide action and/or template",
                ));
            }
            if let Some(action) = &candidate.action {
                action.validate_shape()?;
            }
            let preloaded = if let Some(spec) = &candidate.template {
                if !std::path::Path::new(&spec.path).is_absolute() {
                    return Err(Error::InvalidArgument(
                        "template must be an absolute local image path",
                    ));
                }
                Some(load_validated_template(
                    &spec.path,
                    spec.confidence,
                    spec.exact,
                    spec.scale,
                )?)
            } else {
                None
            };
            preloaded_templates.push(preloaded);
        }
        self.decision_engine()?;
        let started = Instant::now();

        let proof = if let Some(cid) = request.capture_id.as_deref() {
            {
                let proof = self.capture.as_ref().ok_or(Error::InvalidArgument(
                    "capture/observe the selected window first",
                ))?;
                if proof.target != request.target || proof.id != cid {
                    return Err(Error::InvalidArgument("observation target/id mismatch"));
                }
                if let Some(expected_display) = request.display.as_deref()
                    && proof.display.as_deref() != Some(expected_display)
                {
                    return Err(Error::InvalidArgument("observation display mismatch"));
                }
                for candidate in &request.candidates {
                    if let Some(action) = &candidate.action {
                        action.validate(proof)?;
                    }
                }
                for step in &request.follow_up {
                    step.validate_against_proof(proof)?;
                }
            }
            let proof = self.capture.take().ok_or(Error::InvalidArgument(
                "capture/observe the selected window first",
            ))?;
            let action_candidates = request
                .candidates
                .iter()
                .filter_map(|c| c.action.as_ref().and_then(GuiAction::local_point))
                .collect::<Vec<_>>();
            self.verify_observation(&proof, &action_candidates, wait.allow_caret_drift, check)?;
            proof
        } else {
            self.capture = None;
            let proof = self.acquire_capture_proof(
                &request.target,
                request.display.as_deref(),
                request.activate,
                check,
            )?;
            for candidate in &request.candidates {
                if let Some(action) = &candidate.action {
                    action.validate(&proof)?;
                }
            }
            for step in &request.follow_up {
                step.validate_against_proof(&proof)?;
            }
            proof
        };

        let plan =
            match resolve_select_plan(&proof, &request.candidates, &preloaded_templates, check) {
                Ok(plan) => plan,
                Err(error) => {
                    self.capture = Some(proof);
                    return Err(error);
                }
            };

        let (selection_path, evaluation, chosen_candidate, proof) = match plan {
            SelectPlan::FastTemplate(candidate) => {
                let eval = Evaluation {
                    decision_id: None,
                    decision: Decision {
                        action: ActionDecision::Choose,
                        candidate_id: Some(candidate.id.clone()),
                        confidence: Some(1.0),
                        source: "fast_path".to_owned(),
                        calls: 0,
                        latency_ms: 0,
                        input_tokens: None,
                        estimated_cost_usd: None,
                        error: None,
                        error_code: None,
                    },
                    note: "Fast-path unique local template selection on observed window frame.",
                };
                self.decisions.evaluations += 1;
                (
                    "fast_path_template".to_owned(),
                    eval,
                    Some(candidate),
                    proof,
                )
            }
            SelectPlan::FastSingle(candidate) => {
                let eval = Evaluation {
                    decision_id: None,
                    decision: Decision {
                        action: ActionDecision::Choose,
                        candidate_id: Some(candidate.id.clone()),
                        confidence: None,
                        source: "fast_path".to_owned(),
                        calls: 0,
                        latency_ms: 0,
                        input_tokens: None,
                        estimated_cost_usd: None,
                        error: None,
                        error_code: None,
                    },
                    note: "Fast-path single validated candidate selection.",
                };
                self.decisions.evaluations += 1;
                ("fast_path_single".to_owned(), eval, Some(candidate), proof)
            }
            SelectPlan::ObserveUnresolved {
                selection_path,
                note,
            } => {
                let eval = Evaluation {
                    decision_id: None,
                    decision: Decision {
                        action: ActionDecision::Observe,
                        candidate_id: None,
                        confidence: None,
                        source: "fast_path".to_owned(),
                        calls: 0,
                        latency_ms: 0,
                        input_tokens: None,
                        estimated_cost_usd: None,
                        error: None,
                        error_code: None,
                    },
                    note,
                };
                self.decisions.evaluations += 1;
                self.capture = Some(proof);
                return Ok(text(json!({
                    "selection_path": selection_path,
                    "evaluation": eval,
                    "capture_id": self.capture.as_ref().map(|p| &p.id)
                })));
            }
            SelectPlan::SlowEvaluate(candidates) => {
                let protected = protected_points(&candidates);
                let (mut eval, chosen) = self.evaluate_proof(
                    proof,
                    request.goal,
                    candidates,
                    request.minimum_confidence,
                    &protected,
                    wait.allow_caret_drift,
                    check,
                )?;
                let proof = self
                    .capture
                    .take()
                    .ok_or(Error::InvalidArgument("observation lost"))?;
                let path = if eval.decision.action == ActionDecision::Choose {
                    eval.decision.source.clone()
                } else {
                    "observe".to_owned()
                };
                eval.decision_id = None;
                let candidate = chosen.map(|(_, _, c)| c);
                (path, eval, candidate, proof)
            }
        };

        let Some(candidate) = chosen_candidate else {
            let cid = proof.id.clone();
            self.capture = Some(proof);
            return Ok(text(json!({
                "selection_path": selection_path,
                "capture_id": cid,
                "evaluation": evaluation
            })));
        };

        self.decisions.generation = self
            .decisions
            .generation
            .checked_add(1)
            .ok_or(Error::InvalidArgument("restart decision session"))?;
        let decision_id = format!("d{}", self.decisions.generation);
        let display = proof.display.clone();
        let target = proof.target.clone();
        let expected_bounds = proof.bounds;
        let expected_region = proof.region;
        let (name, action_args) = candidate
            .action
            .native(&target, &proof.id, wait.allow_hover);
        self.capture = Some(proof);
        self.decisions.action_attempts += 1;
        let primary_receipt = match self.call(name, action_args, check) {
            Ok(value) => value,
            Err(error) => {
                self.capture = None;
                return Ok(
                    json!({"isError":true,"content":[{"type":"text","text":json!({
                    "decision_id": decision_id,
                    "candidate_id": candidate.id,
                    "selection_path": selection_path,
                    "input_attempted": true,
                    "input_sent": "unknown_or_partial",
                    "outcome": "action_error",
                    "error": error.to_string(),
                    "automatic_retry": false
                }).to_string()}]}),
                );
            }
        };

        let mut follow_up_completed = 0usize;
        let follow_up_total = request.follow_up.len();
        for (idx, step) in request.follow_up.iter().enumerate() {
            let preloaded = preloaded_follow_up.get(idx).and_then(Option::as_ref);
            if let Err(error) = self.execute_follow_up_step(
                &target,
                display.as_deref(),
                expected_bounds,
                expected_region,
                step,
                preloaded,
                check,
            ) {
                self.capture = None;
                return Ok(
                    json!({"isError":true,"content":[{"type":"text","text":json!({
                    "decision_id": decision_id,
                    "candidate_id": candidate.id,
                    "selection_path": selection_path,
                    "input_attempted": true,
                    "input_sent": true,
                    "primary_action": primary_receipt,
                    "follow_up_completed": follow_up_completed,
                    "follow_up_total": follow_up_total,
                    "outcome": "action_error",
                    "error": error.to_string(),
                    "automatic_retry": false
                }).to_string()}]}),
                );
            }
            follow_up_completed += 1;
        }

        let pending = Pending {
            id: decision_id,
            target,
            capture_id: String::new(),
            candidate,
        };
        let mut output = self.finish_observation(
            &pending,
            display,
            primary_receipt,
            wait,
            request.return_image,
            started,
            check,
        )?;
        if let Some(content) = output["content"].as_array_mut()
            && let Some(first) = content.first_mut()
            && let Some(raw) = first["text"].as_str()
            && let Ok(mut obj) = serde_json::from_str::<serde_json::Map<String, Value>>(raw)
        {
            obj.insert("selection_path".into(), json!(selection_path));
            obj.insert("follow_up_completed".into(), json!(follow_up_completed));
            obj.insert("follow_up_total".into(), json!(follow_up_total));
            obj.insert("evaluation".into(), json!(evaluation));
            first["text"] = json!(Value::Object(obj).to_string());
        }
        Ok(output)
    }

    #[allow(clippy::too_many_arguments)]
    fn execute_follow_up_step(
        &mut self,
        target: &str,
        display_key: Option<&str>,
        expected_bounds: Rect,
        expected_region: Rect,
        step: &FollowUpAction,
        preloaded_template: Option<&(Screenshot, Option<f32>)>,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<()> {
        sleep_checked(Duration::from_millis(step.delay_ms()), check)?;
        let window = self.target(target)?;
        active(&window)?;
        if window.box_rect()? != expected_bounds {
            return Err(Error::InvalidArgument(
                "window moved during follow_up; inspect before retrying",
            ));
        }
        match step {
            FollowUpAction::TypeText { text, .. } => {
                self.decisions.action_attempts += 1;
                self.call(
                    "type_text_passive",
                    json!({"target": target, "text": text}),
                    check,
                )?;
            }
            FollowUpAction::Hotkey { keys, .. } => {
                self.decisions.action_attempts += 1;
                self.call(
                    "hotkey_passive",
                    json!({"target": target, "keys": keys}),
                    check,
                )?;
            }
            FollowUpAction::Click { x, y, .. } => {
                let point = Point::new(expected_region.left + *x, expected_region.top + *y);
                self.dispatch_follow_up_pointer(
                    &window,
                    display_key,
                    expected_bounds,
                    expected_region,
                    point,
                    None,
                    check,
                )?;
            }
            FollowUpAction::Scroll {
                x,
                y,
                clicks,
                horizontal,
                ..
            } => {
                let point = Point::new(expected_region.left + *x, expected_region.top + *y);
                self.dispatch_follow_up_pointer(
                    &window,
                    display_key,
                    expected_bounds,
                    expected_region,
                    point,
                    Some((*clicks, *horizontal)),
                    check,
                )?;
            }
            FollowUpAction::ClickTemplate { .. } => {
                let (needle, confidence) = preloaded_template
                    .ok_or(Error::InvalidArgument("follow_up click_template missing"))?;
                let display = display_key.map(|key| self.display(key)).transpose()?;
                let live_frame = {
                    let gui = self.gui()?;
                    let mut guard = PauseGuard::zero(gui);
                    verify_follow_up_window(
                        guard.gui(),
                        &window,
                        display.as_ref(),
                        expected_bounds,
                        expected_region,
                        check,
                    )?;
                    capture_region(guard.gui(), display.as_ref(), expected_region)?
                };
                let centers = locate_unique_centers(needle, &live_frame, *confidence)?;
                if centers.is_empty() {
                    return Err(Error::InvalidArgument(
                        "follow_up click_template found no match on current window",
                    ));
                }
                if centers.len() > 1 {
                    return Err(Error::InvalidArgument(
                        "follow_up click_template matched multiple candidates",
                    ));
                }
                let point = Point::new(
                    expected_region.left + centers[0].x,
                    expected_region.top + centers[0].y,
                );
                self.dispatch_follow_up_pointer(
                    &window,
                    display_key,
                    expected_bounds,
                    expected_region,
                    point,
                    None,
                    check,
                )?;
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn dispatch_follow_up_pointer(
        &mut self,
        window: &Window,
        display_key: Option<&str>,
        expected_bounds: Rect,
        expected_region: Rect,
        point: Point,
        scroll: Option<(i32, bool)>,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<()> {
        let display = display_key.map(|key| self.display(key)).transpose()?;
        let _ = self.gui()?;
        self.decisions.action_attempts += 1;
        let gui = self.gui()?;
        send_pointer_at(
            gui,
            display.as_ref(),
            point,
            scroll,
            |gui| {
                verify_follow_up_window(
                    gui,
                    window,
                    display.as_ref(),
                    expected_bounds,
                    expected_region,
                    check,
                )
            },
            |gui| {
                verify_follow_up_window(
                    gui,
                    window,
                    display.as_ref(),
                    expected_bounds,
                    expected_region,
                    check,
                )
            },
            check,
        )
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
            let protected = pending
                .candidate
                .action
                .local_point()
                .into_iter()
                .collect::<Vec<_>>();
            self.verify_observation(&proof, &protected, wait.allow_caret_drift, check)?;
        }
        let display = proof.display.clone();
        let (name, args) =
            pending
                .candidate
                .action
                .native(&pending.target, &proof.id, wait.allow_hover);
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
        self.finish_observation(&pending, display, action, wait, true, started, check)
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_observation(
        &mut self,
        pending: &Pending,
        display: Option<String>,
        action: Value,
        wait: WaitPolicy,
        return_image: bool,
        started: Instant,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Value> {
        let observed = self.wait_for_frame(&pending.target, display.as_deref(), wait, check);
        match observed {
            Ok((frame, settled, samples)) => Ok(self.finalize_observed_frame(
                pending,
                display,
                action,
                frame,
                settled,
                samples,
                return_image,
                started,
            )),
            Err(error) => {
                self.capture = None;
                Ok(observation_error(pending, action, &error))
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn finalize_observed_frame(
        &mut self,
        pending: &Pending,
        display: Option<String>,
        action: Value,
        frame: Frame,
        settled: bool,
        samples: u64,
        return_image: bool,
        started: Instant,
    ) -> Value {
        let Some(next_generation) = self.capture_generation.checked_add(1) else {
            self.capture = None;
            return observation_error(
                pending,
                action,
                &Error::InvalidArgument("restart capture session"),
            );
        };
        let id = format!("c{next_generation}");
        let note = if return_image {
            "Visual stability is not semantic completion. Inspect this fresh capture before the next action."
        } else {
            "Image omitted by caller (return_image=false); capture_id issued for next step. Verify the result before irreversible actions."
        };
        let metadata = json!({
            "target": pending.target,
            "display": display,
            "region": rect(frame.region),
            "capture_id": id,
            "pointer_coordinates": "image-relative pixels; origin at captured image top-left",
            "expires_in_seconds": 60,
            "single_use": true,
            "decision_id": pending.id,
            "candidate_id": pending.candidate.id,
            "input_attempted": true,
            "input_sent": true,
            "image_omitted": !return_image,
            "outcome": if settled { "settled" } else { "timeout" },
            "task_success": "not_evaluated",
            "samples": samples,
            "latency_ms": started.elapsed().as_millis(),
            "action": action,
            "note": note,
        });
        let output = if return_image {
            match png(&frame.pixels, metadata) {
                Ok(output) => output,
                Err(error) => {
                    self.capture = None;
                    return observation_error(pending, action, &error);
                }
            }
        } else {
            text(metadata)
        };
        self.capture_generation = next_generation;
        self.capture = Some(CaptureProof {
            id,
            target: pending.target.clone(),
            display,
            bounds: frame.bounds,
            region: frame.region,
            frame: frame.pixels,
            taken: Instant::now(),
        });
        output
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
        let mut baseline: Option<Frame> = None;
        let mut interval = policy.poll_ms;
        let mut samples = 0;
        loop {
            let frame = self.passive_frame(target, display, check)?;
            samples += 1;
            let changed = baseline.as_ref().is_none_or(|anchor| {
                anchor.bounds != frame.bounds
                    || anchor.region != frame.region
                    || !frames_match_with_tolerance(
                        &anchor.pixels,
                        &frame.pixels,
                        &[],
                        policy.allow_caret_drift,
                    )
            });
            if changed {
                unchanged_since = frame.taken;
                interval = policy.poll_ms;
                if start.elapsed() >= timeout {
                    return Ok((frame, false, samples));
                }
                baseline = Some(frame);
            } else {
                if unchanged_since.elapsed() >= stable {
                    return Ok((frame, true, samples));
                }
                if start.elapsed() >= timeout {
                    return Ok((frame, false, samples));
                }
            }
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

#[derive(Debug)]
enum SelectPlan {
    FastTemplate(GuiCandidate),
    FastSingle(GuiCandidate),
    SlowEvaluate(Vec<GuiCandidate>),
    ObserveUnresolved {
        selection_path: &'static str,
        note: &'static str,
    },
}

fn verify_follow_up_window(
    gui: &mut AutoGui,
    window: &Window,
    display: Option<&Display>,
    expected_bounds: Rect,
    expected_region: Rect,
    check: &dyn Fn() -> Result<()>,
) -> Result<()> {
    check()?;
    active(window)?;
    if window.box_rect()? != expected_bounds {
        return Err(Error::InvalidArgument(
            "window moved during follow_up; inspect before retrying",
        ));
    }
    let current_region = screen_region(gui, window, display)?;
    if current_region != expected_region {
        return Err(Error::InvalidArgument(
            "window display region changed during follow_up; inspect before retrying",
        ));
    }
    Ok(())
}

fn resolve_select_plan(
    proof: &CaptureProof,
    candidates: &[SelectCandidate],
    preloaded_templates: &[Option<(Screenshot, Option<f32>)>],
    check: &dyn Fn() -> Result<()>,
) -> Result<SelectPlan> {
    if candidates.len() == 1
        && candidates[0].template.is_none()
        && let Some(action) = candidates[0].action.clone()
    {
        return Ok(SelectPlan::FastSingle(GuiCandidate {
            id: candidates[0].id.clone(),
            description: candidates[0].description.clone(),
            action,
        }));
    }

    let mut slow_candidates: Vec<GuiCandidate> = Vec::with_capacity(candidates.len());
    let mut unique_template_indices: Vec<usize> = Vec::new();
    let mut ambiguous = false;
    let mut dropped = 0usize;

    for (idx, candidate) in candidates.iter().enumerate() {
        check()?;
        if let Some((needle, confidence)) = preloaded_templates.get(idx).and_then(Option::as_ref) {
            let valid_centers = locate_unique_centers(needle, &proof.frame, *confidence)?
                .into_iter()
                .filter(|pt| proof.point(&proof.target, &proof.id, pt.x, pt.y).is_ok())
                .collect::<Vec<_>>();

            if valid_centers.len() == 1 {
                let pt = valid_centers[0];
                unique_template_indices.push(slow_candidates.len());
                slow_candidates.push(GuiCandidate {
                    id: candidate.id.clone(),
                    description: candidate.description.clone(),
                    action: GuiAction::Click { x: pt.x, y: pt.y },
                });
            } else if valid_centers.len() > 1 {
                ambiguous = true;
            } else if let Some(action) = candidate.action.clone() {
                slow_candidates.push(GuiCandidate {
                    id: candidate.id.clone(),
                    description: candidate.description.clone(),
                    action,
                });
            } else {
                dropped += 1;
            }
        } else if let Some(action) = candidate.action.clone() {
            slow_candidates.push(GuiCandidate {
                id: candidate.id.clone(),
                description: candidate.description.clone(),
                action,
            });
        } else {
            dropped += 1;
        }
    }

    if !ambiguous
        && slow_candidates.len() == 1
        && unique_template_indices.len() == 1
        && unique_template_indices[0] == 0
    {
        return Ok(SelectPlan::FastTemplate(slow_candidates.remove(0)));
    }

    if slow_candidates.is_empty()
        || (ambiguous && slow_candidates.len() < 2)
        || (dropped > 0 && slow_candidates.len() == 1)
    {
        return Ok(SelectPlan::ObserveUnresolved {
            selection_path: if ambiguous {
                "observe_ambiguous_template"
            } else {
                "observe_no_template_match"
            },
            note: if ambiguous {
                "Ambiguous template matches prevented fast-path selection; observe before acting."
            } else {
                "No template matched uniquely and fewer than two concrete fallback candidates remained."
            },
        });
    }

    Ok(SelectPlan::SlowEvaluate(slow_candidates))
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
                poll_ms: 25,
                ..WaitPolicy::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            WaitPolicy {
                timeout_ms: 50,
                stable_ms: 25,
                poll_ms: 0,
                ..WaitPolicy::default()
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
                poll_ms: 25,
                ..WaitPolicy::default()
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
        assert!(session.decisions.cached_gemini_cli.is_some());
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
        let expected_max_dim = parse_max_image_dim().unwrap();
        assert_eq!(status_json["max_image_dim"], expected_max_dim);
        if expected_mode == "local" {
            assert!(status_json["image_resolution"].is_null());
        }

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

        // return_image=false omits image content block while issuing a fresh CaptureProof.
        let omitted = session.finalize_observed_frame(
            &pending,
            None,
            json!({"clicked":true}),
            Frame {
                bounds: Rect::new(10, 10, 100, 100),
                region: Rect::new(10, 10, 100, 100),
                pixels: Screenshot::solid(100, 100, autogui::Rgb::new(1, 2, 3)),
                taken: Instant::now(),
            },
            true,
            2,
            false,
            Instant::now(),
        );
        assert_eq!(omitted["content"].as_array().unwrap().len(), 1);
        let omitted_json: Value =
            serde_json::from_str(omitted["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(omitted_json["image_omitted"], true);
        assert_eq!(omitted_json["capture_id"], "c1");
        assert_eq!(session.capture.as_ref().unwrap().id, "c1");

        let included = session.finalize_observed_frame(
            &pending,
            None,
            json!({"clicked":true}),
            Frame {
                bounds: Rect::new(10, 10, 100, 100),
                region: Rect::new(10, 10, 100, 100),
                pixels: Screenshot::solid(100, 100, autogui::Rgb::new(1, 2, 3)),
                taken: Instant::now(),
            },
            true,
            2,
            true,
            Instant::now(),
        );
        assert_eq!(included["content"].as_array().unwrap().len(), 2);
        assert_eq!(included["content"][1]["type"], "image");
        let included_json: Value =
            serde_json::from_str(included["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(included_json["image_omitted"], false);
        assert_eq!(included_json["capture_id"], "c2");
    }

    #[test]
    fn select_validates_bounds_and_fast_path_templates() {
        let mut session = Session {
            capture: Some(proof()),
            ..Default::default()
        };
        // Reject follow_up > 8, delay > 500, out-of-bounds follow_up pointer, missing template,
        // activate+capture_id, missing action+template, dup ids
        for bad in [
            json!({"target":"w1","capture_id":"c1","activate":true,"goal":"g","candidates":[{"id":"a","description":"d","action":{"type":"hotkey","keys":["enter"]}}]}),
            json!({"target":"w1","capture_id":"c1","goal":"g","candidates":[{"id":"a","description":"d"}]}),
            json!({"target":"w1","capture_id":"c1","goal":"g","candidates":[
                {"id":"a","description":"d1","action":{"type":"hotkey","keys":["enter"]}},
                {"id":"a","description":"d2","action":{"type":"hotkey","keys":["esc"]}}
            ]}),
            json!({"target":"w1","capture_id":"c1","goal":"g","candidates":[{"id":"a","description":"d","action":{"type":"click","x":1,"y":1}}],"follow_up":[{"type":"hotkey","keys":["enter"],"delay_ms":501}]}),
            json!({"target":"w1","capture_id":"c1","goal":"g","candidates":[{"id":"a","description":"d","action":{"type":"click","x":1,"y":1}}],"follow_up":[
                {"type":"hotkey","keys":["enter"]},{"type":"hotkey","keys":["enter"]},{"type":"hotkey","keys":["enter"]},
                {"type":"hotkey","keys":["enter"]},{"type":"hotkey","keys":["enter"]},{"type":"hotkey","keys":["enter"]},
                {"type":"hotkey","keys":["enter"]},{"type":"hotkey","keys":["enter"]},{"type":"hotkey","keys":["enter"]}
            ]}),
            json!({"target":"w1","capture_id":"c1","goal":"g","candidates":[{"id":"a","description":"d","action":{"type":"click","x":1,"y":1}}],"follow_up":[{"type":"click","x":999,"y":1}]}),
            json!({"target":"w1","capture_id":"c1","goal":"g","candidates":[{"id":"a","description":"d","action":{"type":"click","x":1,"y":1}}],"follow_up":[{"type":"scroll","x":-1,"y":1,"clicks":-2}]}),
            json!({"target":"w1","capture_id":"c1","goal":"g","candidates":[{"id":"a","description":"d","action":{"type":"click","x":1,"y":1}}],"follow_up":[{"type":"click_template","template":{"path":"relative/missing.png"}}]}),
            json!({"target":"w1","capture_id":"c1","goal":"g","return_image":"no","candidates":[{"id":"a","description":"d","action":{"type":"click","x":1,"y":1}}]}),
        ] {
            assert!(
                session
                    .decision_call("decision_select", bad, &|| Ok(()))
                    .is_err()
            );
        }
        // Invalid upfront args preserve capture and never construct GUI.
        assert!(session.capture.is_some());
        assert!(session.gui.is_none());

        // Fast-path template resolution on in-memory frame without OS backend calls.
        let mut frame = Screenshot::solid(120, 80, autogui::Rgb::new(10, 10, 10));
        for y in 20..30 {
            for x in 40..56 {
                let idx = (y * 120 + x) * 3;
                frame.pixels[idx] = 240;
                frame.pixels[idx + 1] = 80;
                frame.pixels[idx + 2] = 20;
            }
        }
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("rsgui-select-fast-{unique}"));
        std::fs::create_dir_all(&dir).unwrap();
        let tpl_path = dir.join("button.png");
        let tpl_img = Screenshot::solid(16, 10, autogui::Rgb::new(240, 80, 20));
        tpl_img.save(&tpl_path).unwrap();

        let proof_with_btn = CaptureProof {
            id: "c1".into(),
            target: "w1".into(),
            display: None,
            bounds: Rect::new(0, 0, 120, 80),
            region: Rect::new(0, 0, 120, 80),
            frame,
            taken: Instant::now(),
        };
        let miss_tpl_path = dir.join("blue.png");
        Screenshot::solid(16, 10, autogui::Rgb::new(10, 40, 240))
            .save(&miss_tpl_path)
            .unwrap();
        let huge_tpl_path = dir.join("huge.png");
        Screenshot::solid(200, 10, autogui::Rgb::new(10, 40, 240))
            .save(&huge_tpl_path)
            .unwrap();

        let make_tpl_candidate =
            |id: &str, path: &std::path::Path, action: Option<GuiAction>| SelectCandidate {
                id: id.into(),
                description: format!("Select {id}"),
                action,
                template: Some(SelectTemplate {
                    path: path.to_string_lossy().into_owned(),
                    confidence: None,
                    exact: Some(true),
                    scale: None,
                }),
            };
        let load_pre = |cands: &[SelectCandidate]| -> Vec<Option<(Screenshot, Option<f32>)>> {
            cands
                .iter()
                .map(|c| {
                    c.template.as_ref().map(|spec| {
                        load_validated_template(&spec.path, spec.confidence, spec.exact, spec.scale)
                            .unwrap()
                    })
                })
                .collect()
        };

        // Display mismatch against capture_id is rejected before consuming capture or opening GUI.
        let mut display_session = Session {
            capture: Some(CaptureProof {
                display: Some("d1".into()),
                ..proof()
            }),
            ..Default::default()
        };
        assert!(
            display_session
                .decision_call(
                    "decision_select",
                    json!({"target":"w1","capture_id":"c1","display":"d2","goal":"g","candidates":[{"id":"a","description":"d","action":{"type":"click","x":1,"y":1}}]}),
                    &|| Ok(())
                )
                .is_err()
        );
        assert!(display_session.capture.is_some());
        assert!(display_session.gui.is_none());

        // 1. Sole matching template resolves on fast_path_template.
        let one_tpl = vec![make_tpl_candidate("click_orange", &tpl_path, None)];
        match resolve_select_plan(&proof_with_btn, &one_tpl, &load_pre(&one_tpl), &|| Ok(()))
            .unwrap()
        {
            SelectPlan::FastTemplate(resolved) => {
                assert_eq!(resolved.id, "click_orange");
                assert!(matches!(resolved.action, GuiAction::Click { x: 48, y: 25 }));
            }
            other => panic!("expected FastTemplate, got {other:?}"),
        }

        // 2. Mixed unique template + action-only candidate must escalate BOTH to SlowEvaluate (never bypass Gemini).
        let mixed = vec![
            make_tpl_candidate("click_orange", &tpl_path, None),
            SelectCandidate {
                id: "scroll_down".into(),
                description: "Scroll down".into(),
                action: Some(GuiAction::Scroll {
                    x: 10,
                    y: 10,
                    clicks: -2,
                    horizontal: false,
                }),
                template: None,
            },
        ];
        match resolve_select_plan(&proof_with_btn, &mixed, &load_pre(&mixed), &|| Ok(())).unwrap() {
            SelectPlan::SlowEvaluate(cands) => {
                assert_eq!(cands.len(), 2);
                assert_eq!(cands[0].id, "click_orange");
                assert!(matches!(cands[0].action, GuiAction::Click { x: 48, y: 25 }));
                assert_eq!(cands[1].id, "scroll_down");
            }
            other => panic!("expected SlowEvaluate for mixed template+action, got {other:?}"),
        }

        // 3. Unmatched template + action-only candidate must NOT collapse to 1-candidate Facts::Ready.
        let miss_mixed = vec![
            make_tpl_candidate("click_blue", &miss_tpl_path, None),
            SelectCandidate {
                id: "scroll_down".into(),
                description: "Scroll down".into(),
                action: Some(GuiAction::Scroll {
                    x: 10,
                    y: 10,
                    clicks: -2,
                    horizontal: false,
                }),
                template: None,
            },
        ];
        assert!(matches!(
            resolve_select_plan(
                &proof_with_btn,
                &miss_mixed,
                &load_pre(&miss_mixed),
                &|| Ok(())
            )
            .unwrap(),
            SelectPlan::ObserveUnresolved {
                selection_path: "observe_no_template_match",
                ..
            }
        ));

        // 4. Oversized template treats as 0 hits cleanly (no hard error).
        let huge = vec![make_tpl_candidate("huge_tpl", &huge_tpl_path, None)];
        assert!(matches!(
            resolve_select_plan(&proof_with_btn, &huge, &load_pre(&huge), &|| Ok(())).unwrap(),
            SelectPlan::ObserveUnresolved {
                selection_path: "observe_no_template_match",
                ..
            }
        ));

        // 5. Two uniquely matching templates escalate both located coordinates to SlowEvaluate.
        let mut two_btn_frame = proof_with_btn.frame.clone();
        for y in 40..50 {
            for x in 10..26 {
                let idx = (y * 120 + x) * 3;
                two_btn_frame.pixels[idx] = 20;
                two_btn_frame.pixels[idx + 1] = 200;
                two_btn_frame.pixels[idx + 2] = 60;
            }
        }
        let green_tpl_path = dir.join("green.png");
        Screenshot::solid(16, 10, autogui::Rgb::new(20, 200, 60))
            .save(&green_tpl_path)
            .unwrap();
        let two_btn_proof = CaptureProof {
            frame: two_btn_frame,
            ..proof_with_btn.clone()
        };
        let two_tpls = vec![
            make_tpl_candidate("click_orange", &tpl_path, None),
            make_tpl_candidate("click_green", &green_tpl_path, None),
        ];
        match resolve_select_plan(&two_btn_proof, &two_tpls, &load_pre(&two_tpls), &|| Ok(()))
            .unwrap()
        {
            SelectPlan::SlowEvaluate(cands) => {
                assert_eq!(cands.len(), 2);
                assert!(matches!(cands[0].action, GuiAction::Click { x: 48, y: 25 }));
                assert!(matches!(cands[1].action, GuiAction::Click { x: 18, y: 45 }));
            }
            other => panic!("expected SlowEvaluate for two unique templates, got {other:?}"),
        }

        // 6. Duplicate identical matches defer to ObserveUnresolved (even alongside another unique template).
        let mut dup_frame = proof_with_btn.frame.clone();
        for y in 50..60 {
            for x in 80..96 {
                let idx = (y * 120 + x) * 3;
                dup_frame.pixels[idx] = 240;
                dup_frame.pixels[idx + 1] = 80;
                dup_frame.pixels[idx + 2] = 20;
            }
        }
        for y in 40..50 {
            for x in 10..26 {
                let idx = (y * 120 + x) * 3;
                dup_frame.pixels[idx] = 20;
                dup_frame.pixels[idx + 1] = 200;
                dup_frame.pixels[idx + 2] = 60;
            }
        }
        let dup_proof = CaptureProof {
            frame: dup_frame,
            ..proof_with_btn
        };
        assert!(matches!(
            resolve_select_plan(&dup_proof, &two_tpls, &load_pre(&two_tpls), &|| Ok(())).unwrap(),
            SelectPlan::ObserveUnresolved {
                selection_path: "observe_ambiguous_template",
                ..
            }
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
