# Rust decision desktop integration

## Operation

Run `rsgui-decision.exe mcp` with the matching `autogui_runtime.dll` beside it.
The process keeps one serial input Session; MCP stdout contains JSON-RPC only.
The existing initialization/cancellation/connection lifecycle is retained.

1. Call `windows_list` with a distinctive title; keep the returned session target.
2. Call `capture` for explicit initial activation and inspect its image. Subsequent
   `decision_observe` is passive and refuses to steal focus from another window.
3. Supply the returned `capture_id` and concrete candidates to `decision_evaluate`,
   then call `decision_act` with the returned `decision_id`, OR call `decision_step`
   to evaluate and execute in one call, OR call `decision_select` (hybrid web/app
   selector supporting optional `capture_id` auto-observe/activate, local
   template/single-candidate fast path, Gemini Decision 1 multi-candidate slow path,
   up to 8 bounded `follow_up` `type_text`/`hotkey`/`click`/`scroll`/`click_template`
   steps, and optional `return_image: false` to omit intermediate PNG encoding/payloads
   while still returning a fresh `capture_id`).
4. Inspect the resulting image and use its new `capture_id` for the next step.

Example `decision_step` payload (`decision_evaluate` takes the flat `request`
object without `"request"` or `"wait"`):

```json
{
  "request": {
    "target": "w1-0",
    "capture_id": "c1",
    "goal": "Click the inspected test button",
    "candidates": [{
      "id": "click_button",
      "description": "Click the button selected by the operator",
      "action": {"type": "click", "x": 120, "y": 100}
    }]
  },
  "wait": {"timeout_ms": 1500, "stable_ms": 100, "poll_ms": 25}
}
```

Coordinates are relative to the captured image, never guesses supplied by a
model. Candidate action types are `click`, `scroll`, `type_text` and `hotkey`.
At most one action executes. All candidates and settings are validated before
input. Candidate payloads are saved server-side; execution cannot substitute a
different action. A stale/wrong/consumed decision is rejected. Native calls that
may change focus, pixels, targets or displays invalidate pending decisions.

The execution boundary checks the observed frame, target identity, foreground state
and geometry. `decision_evaluate` preserves `allow_caret_drift: true` internally
(without a `wait` parameter), and `decision_act`, `decision_step`, and
`decision_select` default `wait.allow_caret_drift: true` (requiring exact pixel
matches inside every candidate 32x32 target patch while tolerating small distant
text-caret blink changes outside target patches) and `wait.allow_hover: true`
(retaining pointer-move/cursor/bounds/foreground checks while skipping post-move
target patch abort for browser CSS `:hover` transitions); standalone `click_at`
and `scroll` retain strict 32x32 patch checks. The existing corner fail-safe,
Unicode chunk checks, cancellation and disconnection checks remain. Significant
animation or out-of-patch layout changes may still invalidate an observation;
obtain a new capture rather than blindly retrying a possibly delivered action.

The local wait polls raw frames at 25 -> 50 -> 100 ms by default, resetting on
change, and encodes only the returned frame. `outcome=settled` means stable pixels,
not saved/sent/completed. `timeout` still returns the latest image. If an error
occurs after input, the result states `input_sent=true` or `unknown_or_partial`;
never repeat it without inspecting the target. No automatic action retry exists.

## Provider, discovery hook, and cost

Default `RSGUI_DECISION_MODE=auto` (trimmed, case-insensitive; override with
`local`, `auto`, or `gemini`) prioritizes Google Gemini Decision 1 via the built-in
`PreferGeminiDecisionHook` whenever an API key is configured, and falls back
safely to local `observe` (`source: "auto_fallback"`) on provider failures or
absent keys. Deterministic facts (`Ready` with a sole validated candidate, `Wait`,
`Done`, or `Blocked`) resolve with zero provider calls across `local`, `auto`,
and `gemini` modes unless a custom Rust `DecisionHook` explicitly sets
`evaluate_ready: true` — a sole validated candidate is the caller's deterministic
action, not a claim of machine visual understanding. Unknown facts with multiple
candidates return `observe` in `local` mode (`RSGUI_DECISION_MODE=local` never
reads API keys or makes network calls).

In `auto` and `gemini` modes, `Engine::from_env()` also populates non-secret
`gemini_decision_1` discovery metadata (`api_key_configured`, local `gemini-core`
CLI / `~/.gemini-decision-1` presence, and loopback `127.0.0.1:8792/health`
sidecar probe unless disabled via `RSGUI_DECISION_PROBE_SIDECAR=0`). Alongside the
Rust `DecisionHook` API, `.agents/hooks.json` +
`.agents/hooks/prefer-gemini-decision.ps1` provides Antigravity `SessionStart`
and `PreToolUse` workspace hooks so agents prioritize `decision_select`,
`decision_observe`, `decision_evaluate`, and `decision_step` whenever Gemini
Decision 1 is available.

Google's fixed `https://generativelanguage.googleapis.com/v1beta/decisions`
endpoint is contacted in `auto` or `gemini` modes for remote decisions, sending
a fast downscaled inference PNG (preserving native full-resolution coordinates in
`CaptureProof`; controlled by `RSGUI_DECISION_MAX_IMAGE_DIM`, default `1280`,
`0` disables downscaling or `320..=3840`, PNG `1..=16,777,216` base64 bytes /
up to 12 MiB decoded), redacted `goal`, and redacted candidate descriptions.
`reqwest::blocking::Client` enables HTTP keep-alive connection pooling and is
warmed in-process on `decision_observe`, `decision_evaluate`, `decision_step`,
and `decision_select` without network I/O (`decision_status` remains read-only).
Credentials come only from the invoking process environment (`GEMINI_API_KEY`
preferred over `GOOGLE_API_KEY`, trimmed; `.env` files are not loaded); keys
never belong in a plan or Git. Before remote requests, common secret patterns
and any literal key value are redacted from `goal` and `candidate.description`,
whereas immutable `candidate.id` values or PNG payloads matching the configured
key are rejected as `InvalidInput` (without fallback, including in `auto` mode).
Unrelated windows are not intentionally captured, but overlapping windows and
sensitive content visible in the selected region can appear in screenshots.
Review the visible target before opting in. There is no screenshot redaction claim.

One request batches independent `blocked`/`done`/`next_action` choice questions
against shared text and image input (`RSGUI_DECISION_IMAGE_RESOLUTION` accepts
`low`, `medium` default, `high`, or `ultra_high`, trimmed and case-insensitive).
The provider returns only bounded choices/probabilities. `minimum_confidence`
permits `0.5..=1.0` (default `0.85`), `MAX_AMBIGUOUS_NOUL` rejects ambiguous
multi-candidate choices when `noul > 0.22`, `blocked`/`done` semantics require
`>= 0.85`, and an explicit provider `"observe"` choice returns `observe`. `auto`
falls back to `observe` (with `error`, `error_code`, and any returned
`input_tokens`/`estimated_cost_usd`) on provider failures, refusals, malformed
responses, or low confidence; `gemini` returns the failure directly. Neither
path falls back to the first candidate, and `InvalidInput` always propagates.
`RSGUI_DECISION_TIMEOUT_MS` defaults to `1500` and permits `100..=10000` ms
(trimmed). Requests are not automatically retried; `401`/`403` auth errors
engage a `600s` cooldown (ignoring `Retry-After`), `429` rate limits honor
`Retry-After` clamped to `1..=600s` (default `60s`), and timeouts engage a
`30s` cooldown.

Checked 2026-11-15: Gemini Decision 1 is free on the eligible free tier; paid
input costs USD $0.10/million tokens, with zero output tokens. Actual tier and
quota depend on the account. Any returned input-token estimate is not a billing
receipt. See [official Decisions API](https://aistudio.google.com/docs/decisions-api)
and [official pricing](https://aistudio.google.com/docs/pricing#gemini-decision).
The Rust core ports GUI-relevant typed decisions, not the original coding-agent
skills/retrieval/memory/compression features.

## Verification and installation

```powershell
cargo test --workspace --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo fmt --all -- --check
.\tests\mcp-protocol.ps1 -Executable .\target\release\rsgui-decision.exe
.\scripts\package.ps1 -SkipBuild -OutputDir .\dist\release
.\tests\local-install.ps1 -PackageDir .\dist\release
```

Live tests are ignored by default. Run only against the test-owned fixture with
an unlocked rendered desktop; do not run against a user's application:
If Windows denies foreground activation, the test waits up to 60 seconds for
explicit activation of its own fixture window. It never changes foreground-lock
settings or attempts to bypass them.

```powershell
$env:AUTOGUI_LIVE='1'
$env:AUTOGUI_MCP_EXE=(Resolve-Path .\target\release\rsgui-decision.exe).Path
cargo test --all-features --test live_app_control integrated_decision -- --ignored --nocapture --test-threads=1
```

Run `install-local.ps1 -PackageDir <verified-package>` to upgrade the existing
AutoGui EXE/DLL pair and user-local rsgui-decision EXE/DLL pair. It stages and
smokes both loaders, preserves old files and outputs a recovery manifest.
Use `restore-local.ps1 -Manifest <that-path>` to restore that exact transaction;
changed destination files are protected. Host approval policies, browser URL
verification, accounts, MCP configuration and PATH are outside this installer.
The default upgrade refuses running/locked files. After verifying the package,
`-AllowInUseRename` may preserve running old images by moving them into the
transaction's displaced directory before placing new files at their original
paths. Old processes keep their old code/tools until restarted; new processes
use the new pair. This does not disconnect existing MCP sessions. Restore still
requires new processes to exit before replacing their mapped files. These
artifacts are unsigned.
