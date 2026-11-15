# Rust decision desktop integration

## Operation

Run `rsgui-decision.exe mcp` with the matching `autogui_runtime.dll` beside it.
The process keeps one serial input Session; MCP stdout contains JSON-RPC only.
The existing initialization/cancellation/connection lifecycle is retained.

1. Call `windows_list` with a distinctive title; keep the returned session target.
2. Call `capture` for explicit initial activation and inspect its image. Subsequent
   `decision_observe` is passive and refuses to steal focus from another window.
3. Supply the returned `capture_id` and concrete candidates to `decision_evaluate`.
4. Review the returned decision, then call `decision_act` with only its decision ID.
   Alternatively call `decision_step` to evaluate and execute in one call.
5. Inspect the resulting image and use its new `capture_id` for the next step.

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

The execution boundary checks the full observed frame, target identity,
foreground state and geometry. Native pointer input additionally checks its
local pixel patch before and after moving the pointer. The existing corner
fail-safe, Unicode chunk checks, cancellation and disconnection checks remain.
Hover effects, caret blinks and animation may invalidate an observation; obtain
a new capture rather than blindly retrying a possibly delivered action.

The local wait polls raw frames at 25 -> 50 -> 100 ms by default, resetting on
change, and encodes only the returned frame. `outcome=settled` means stable pixels,
not saved/sent/completed. `timeout` still returns the latest image. If an error
occurs after input, the result states `input_sent=true` or `unknown_or_partial`;
never repeat it without inspecting the target. No automatic action retry exists.

## Provider and cost

Default `RSGUI_DECISION_MODE=local` makes no provider calls. A sole validated
candidate is the caller's deterministic action, not a claim of machine visual
understanding. Multiple candidates return observe in local mode.

Explicit `RSGUI_DECISION_MODE=auto` or `gemini` opts into Google's fixed
`https://generativelanguage.googleapis.com/v1beta/decisions` endpoint and selected
window PNG, goal and candidate descriptions. Set `GEMINI_API_KEY` (preferred)
or `GOOGLE_API_KEY` in the invoking environment; keys never belong in a plan or
Git. Unrelated windows are not intentionally captured, but overlapping windows
and sensitive content visible in the selected region can appear in screenshots.
Review the visible target before opting in. There is no screenshot redaction claim.

One request batches independent blocked/done/choice questions against shared
text and image input. The provider returns only bounded choices/probabilities.
Minimum accepted choice confidence defaults to 0.85. Auto falls back to observe
on an unavailable/malformed/uncertain provider; Gemini reports transport or
malformed failures. Neither path falls back to the first candidate.
`RSGUI_DECISION_TIMEOUT_MS` defaults to 1500 and permits 100..10000 ms. Requests
are not automatically retried; timeout/auth/rate errors engage a bounded cooldown.

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
