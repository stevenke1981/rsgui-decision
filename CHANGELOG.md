# Changelog

## Unreleased

- Accelerated web and app UI selection by combining `rsgui-decision-core` (Gemini Decision 1) with `rsautogui` and exposed a 20th MCP tool `decision_select`:
  - Added fast remote inference PNG encoding (`RSGUI_DECISION_MAX_IMAGE_DIM`, default `1280`, preserving native full-resolution `CaptureProof` coordinates) using PNG `FilterType::Sub` and bilinear/triangle downscaling, plus HTTP Keep-Alive connection pooling (`pool_idle_timeout`, `pool_max_idle_per_host`, `tcp_keepalive`, `tcp_nodelay`) and in-process `warm_up_client()`.
  - Calibrated default `ImageResolution` to `Medium` (matching live `gemini-decision-1` confidence behavior) and raised `MAX_AMBIGUOUS_NOUL` to `0.22`; surfaced `image_resolution` and `max_image_dim` in `decision_status`.
  - Added web/app UI resilience on `WaitPolicy`: `allow_caret_drift` (`true` default: exact match inside candidate 32x32 patches with bounded `max(64, min(2500, total_pixels / 1000))` out-of-patch caret-blink tolerance) and `allow_hover` (`true` default on `decision_*` actions via internal hover-tolerant pointer dispatch, keeping standalone `click_at`/`scroll` strict).
  - Implemented `decision_select` combining optional `capture_id` auto-observe/`activate`, local `fast_path_single` / in-memory `fast_path_template` unique-match resolution via `AutoGui::locate_all`, Gemini Decision 1 multi-candidate escalation, up to 8 validated `follow_up` (`type_text`, `hotkey`, `click`, `scroll`, and live-matched `click_template`; `delay_ms <= 500`) inputs, and optional `return_image` (`true` default; `false` omits PNG encoding/payload while issuing a fresh `capture_id`).
  - Optimized MCP cold/warm path latency: switched host PNG return encoding (`encode_png_base64`) to lossless `FilterType::Sub`, cached `tools/list` JSON definitions via `OnceLock`, introduced `Engine::from_env_runtime()` so operational `decision_*` calls skip `PATH` scans and TCP sidecar probes, tightened `SIDECAR_PROBE_TIMEOUT` to `10 ms`, and cached `gemini-core` CLI discovery per MCP session in `decision_status`.

- Changed default `RSGUI_DECISION_MODE` from `local` to `auto`, added the Rust `DecisionHook` / `PreferGeminiDecisionHook` extension and non-secret `GeminiDiscovery` (`GEMINI_API_KEY`/`GOOGLE_API_KEY`, `gemini-core` CLI/`~/.gemini-decision-1`, and bounded loopback `127.0.0.1:8792/health` probe surfaced in `decision_status`), and added Antigravity workspace hooks (`.agents/hooks.json` + `.agents/hooks/prefer-gemini-decision.ps1`) with automated tests (`tests/hooks.ps1`, CI) so Gemini Decision 1 is prioritized whenever present.

- Fixed virtual-desktop pointer coordinate normalization (`MOUSEEVENTF_VIRTUALDESK` over combined monitor bounds, preserving primary-monitor coordinates), scoped Windows DPI guards outside window move/resize/wait loops, eliminated duplicate display enumerations/pixel copies on recognition and capture paths, preserved `settings.pause` / `SPEC.md` read-heavy pauses on `AutoGui` library calls while batching `typewrite` fail-safe checks, and exported `autogui::normalize` plus `Key::Insert` support on the OS backend.
- Hardened the launcher/runtime ABI boundary (`ModuleGuard` `FreeLibrary` on `GetProcAddress`/ABI mismatch, distinct 1 MiB request vs 20 MiB response overflow diagnostics, status code `2` on oversized input, zero-clone JSON argument move, matching binary names in `--help`/`--version`) and fixed `--no-default-features` clippy/test cleanliness across `autogui`, `examples`, and `app_control`.
- Strengthened `rsgui-decision-core` and bounded MCP decision tools: lazy `reqwest::blocking::Client` construction, `RSGUI_DECISION_IMAGE_RESOLUTION` (`low` default), direct JSON byte serialization with concise rubric, broader secret/PEM/credential redaction + immutable credential rejection, HTTP/cooldown error telemetry (`CooldownReason`, `Error::code()`, `error_code`, `estimated_cost_usd`), structured `decision_step` `evaluation` + `observation_error` metadata, pre-consumption action validation (`\0` / hotkey normalization), `decision_status` `provider_model: null` in local mode, `PauseGuard::zero` on internal MCP verification/poll/pointer paths without skipping fail-safe checks, zero-clone PNG Base64 encoding (skipped when unnecessary), and `WaitPolicy` `stable_ms < timeout_ms` validation.
- Hardened packaging, installer, local-replacement, Skills, CI, and documentation: `install-local.ps1` now verifies `decision_status` (forcing `RSGUI_DECISION_MODE=local`) with runtime DLL mapping, cleans up staging dirs if pre-manifest probing fails, supports cross-volume `-AllowInUseRename`, and fixes drive-root prefix checks; `package.ps1` canonicalizes trailing slashes and checks existing `.zip` archives; `installer`/`tests` enforce `-Encoding UTF8` JSON reads, clean up `.agent` test fixtures, cover both `rsgui-decision.exe` and `autogui-control.exe` with `decision_*` validation, and run `--no-default-features` + `tests/installer.ps1` in CI; updated README, `docs/agent-installation.md`, and `skills/autogui-control` for all 19 MCP tools and `decision-tools.json`.
- Fixed installation with existing empty/whitespace MCP JSON while preserving byte-exact uninstall backups. Antigravity skills now default to the shared `.gemini/config/skills`; `-AgySkillRoot` supports legacy/custom locations.
- Fixed restore timing out when a minimized window returns to its previous maximized state; move/resize and display placement still explicitly normalize the window first.
- Added MCP `click_at` and vertical/horizontal `scroll`, bound to the latest single-use window capture for 60 seconds. Input checks target identity, foreground, geometry, display, cursor and a local pixel patch before dispatch; invalid attempts also consume the proof.
- Batched MCP `type_text` into at most 32 Unicode scalar values per foreground/cancellation check, retaining library fail-safe checks and the configured pause per batch. Clipboard paste and UI Automation remain outside this change.

- Added unattended/monitor-less readiness diagnostics: CLI `session status [--json]` (non-zero exit on blockers) and read-only MCP `session_status`. Reports session/console/RDP state, lock, input desktop, cursor access, active displays and a primary-monitor capture probe, with blocker codes such as `no-active-display`, `remote-not-rendering`, `session-not-active`, `locked` and `service-session`. Decision logic is unit-tested on all platforms.
- Added `scripts/rdp-to-console.ps1` (administrator, `-WhatIf`/confirmation) to move an RDP session to the console with `tscon` so the desktop keeps rendering after disconnect, plus `docs/headless.md`.

- Added explicit Windows display snapshots, real display/region PNG capture, display-targeted recognition/clicking with virtual-desktop SendInput coordinates, and Win32 window placement on physical or IDD virtual monitors. Existing primary-monitor defaults and corner fail-safe remain.
- Added three MCP tools (`displays_list`, `display_capture`, `window_to_display`), optional display references for capture/locate/click_image, CLI display commands, and updated Skills. Display identity/geometry is revalidated and stale references are rejected.
- Added an optional pinned VDD/NefCon preparation and administrator installation helper with ZIP hashes, signature checks, one-monitor configuration and owned-device recovery records. Driver downloads/installations are separate from the lazy Rust EXE/DLL and are never triggered by MCP.

- Added a small Windows launcher with an on-demand `autogui_runtime.dll`; help and MCP negotiation/listing run without loading the DLL. Kept all image codecs and library APIs. Release uses size optimization, Thin LTO and symbol stripping while recognition/FFT retain opt-level 3 and unwind cleanup remains enabled.
- Added stdio MCP with eight tools, session window references, PNG image content, checked input, cooperative cancellation and per-connection screensaver pause leases restored on disconnect.
- Exposed Win32 inspect/activate/move/resize/minimize/maximize/restore/hide/show/close in CLI and MCP, including honest WM_CLOSE request semantics.
- Added a bundled `autogui-control` Skill, selectable Codex/Claude/OpenCode/AGY Skill and MCP installation, JSONC-preserving edits, per-file manifests/backups and conservative uninstall/rollback. Added isolated installer, lazy DLL/protocol, and native MCP fixture coverage.

- Added Windows CLI `screensaver status/pause/resume/stop`, configurable pause duration, Ctrl+C/timer/manual restoration, per-logon crash recovery and concurrent-pause protection. No password, timeout or persistent screen-lock setting changes.
- Screensaver commands execute before the input backend is initialized. Stop verifies `.scr` process identity on the dedicated desktop and confirms shutdown; pause reports Windows power/lock transition errors and restores prior state on failure.
- Completed native screen-recognition, click, Unicode input and CLI acceptance after dismissing the screen saver. The disposable classic EDIT fixture now owns its Ctrl+A accelerator; its packed accelerator buffer is explicitly DWORD-aligned for Win32.

- Fixed grayscale arithmetic overflow and false positives on constant-color templates; confidence search now uses per-channel normalization, integer integral images, FFT candidate search and direct candidate verification.
- Replaced quadratic overlap suppression with spatial buckets, and capture only the requested region during screen searches while preserving absolute coordinates.
- Added cooperative fail-safe checks during recognition, drag and repeated input; key/button cleanup still runs at fail-safe corners. `KeyHold` now permits operations through its guard.
- Added scoped physical-pixel DPI handling on Windows, process/thread identity validation for window handles, current-title reads, asynchronous window state requests and state/foreground confirmation.
- Added `app_control` with window/title disambiguation, image revalidation, explicit template scaling, window capture and guarded input. Non-OS builds refuse to operate the CLI.
- Added recognition benchmarks and an ignored native fixture test covering real clicks, Unicode input and CLI operations. Interactive execution is subject to an unlocked Windows desktop.

- `AutoGui::new()` now uses real desktop input through Enigo and primary-monitor capture through XCap; `with_fake` remains for tests.
- Added common image-file loading/saving, including PNG and JPEG, for `Screenshot` and screen locate APIs.
- Implemented normalized cross-correlation for confidence matching behind the `opencv` feature flag.
- Tightened input release handling, screen-region clipping, fail-safe checks after screen reads, and Win32 window validation.
- Reworked the cheat-sheet example and documentation to avoid sending real input when copied or run.

## 0.1.0

- AutoGui instance API: mouse, keyboard, fail-safe, pause, tweens
- FakeBackend for deterministic tests
- Exact template locate on RGB buffers and common raster image files
- Optional normalized cross-correlation matching via the `opencv` feature flag
- Window API: full Win32 module behind `cfg(windows)`; other OS return UnsupportedPlatform
- Examples: cheat_sheet (read-only), spiral_drag (warns before moving)
