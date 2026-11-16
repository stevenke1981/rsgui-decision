# Decision MCP & Gemini Decision 1 Runbook

## 1. Credential & Mode Preflight (`decision_status`)

- Call `decision_status {}` before relying on multi-candidate Gemini Decision 1 evaluation (`RSGUI_DECISION_MODE` defaults to `auto`).
- `autogui-control.exe mcp` / `rsgui-decision.exe` reads **only** its invoking `Process` environment (`GEMINI_API_KEY` prioritized over `GOOGLE_API_KEY`; `.env` files and live Windows Registry changes are not polled). Workspace hooks may detect `User`-scoped keys even while the live MCP process lacks them.
- If `provider_ready` is `false` despite a configured Windows User/System variable:
  - **Preferred (no plaintext on disk)**: Cold-restart the host Agent from a newly launched OS shell/Explorer process that already holds the updated variable (`[bool]$env:GOOGLE_API_KEY`).
  - **Scoped MCP override**: Add `"env": { "GOOGLE_API_KEY": "..." }` under `mcpServers.autogui` in the host MCP config (e.g., `~/.gemini/config/mcp_config.json`) and restart the MCP server. Note that host tools creating `mcp_config.json.backup_*` backups will snapshot plaintext entries.
- If no local loopback sidecar runs on `127.0.0.1:8792`, set `"RSGUI_DECISION_PROBE_SIDECAR": "0"` in `mcpServers.autogui.env` to skip the TCP connect probe on `decision_status`.

## 2. Hybrid `decision_select` Strategy (Web & App UI)

- **Know the exact single target or template**: Always pass 1 candidate (`fast_path_single`) or static `template` paths (`fast_path_template` via `locate_all`) for zero remote calls. Never add dummy/distractor candidates to force Gemini evaluation on an already-known control—the ~1 s remote wait increases pixel-drift aborts on dynamic web pages.
- **Ambiguous multi-candidate selection (Gemini Decision 1)**: `decision_evaluate` / `decision_select` strips `(x, y)` coordinates from model prompts, sending only the downscaled inference PNG, `goal`, and each candidate's `id` and `description`.
  - Write concrete visual descriptions naming **exact visible text/icon** and **relative position inside its container** (e.g., `"The 'x1' button (leftmost option in the 'x1 x2 x3 x4' count control inside the settings popover)"`). Avoid generic descriptions like `"settings button"`, which trigger `GEMINI_LOW_CONFIDENCE`.

## 3. Dynamic Web Overlays, Focus & `capture_id` Reuse

- Dynamic web UIs (CSS hover fades, popover animations, post-typing reflows) can exceed `allow_caret_drift` tolerances while inspecting a screenshot, causing `observed pixels/display changed; observe again` on a prior `capture_id`.
- **Switching a background window to foreground before `decision_select`**: Avoid `activate: true` combined with multi-candidate Gemini calls on background browsers—activating paints focus rings, carets, and hover fades after the initial `0 ms` capture and triggers `observed pixels/display changed; observe again`. Instead, call `window_control {action: "activate"}` (or `capture`) first so focus repaints settle, then call `decision_select {activate: false}` with `capture_id` omitted.
- Omit `capture_id` in `decision_select` when acting on dynamic web overlays so an atomic baseline frame is captured immediately before execution.
- **Multi-step chaining in 1 roundtrip**: Chain up to 8 `follow_up` steps (`type_text`, `hotkey`, `click`, `scroll`, and live-matched `click_template`) inside `decision_select`, and set `return_image: false` on intermediate steps (keeping `true` on the final verification call) to skip PNG encoding/offload and extra `read_file` vision turns while preserving `capture_id` issuance.
- Clicking outside an open web modal/popover may dismiss the backdrop without focusing an underlying textarea; capture/inspect caret focus after dismissal before running `type_text`.

## 4. Traditional Chinese / CJK IME Input Hygiene (`type_text`)

- Under an active Windows Bopomofo (`注音`) or CJK IME, full-width punctuation (`，` / `、`) inside `type_text` can trigger dotted-underline preedit composition and reorder text at the caret in browser textareas.
- Prefer spaces over `，` / `、` (or use English/punctuation-free Unicode phrases) for web prompt inputs when CJK IME may be active.
- To recover cleanly if IME preedit occurred, chain `hotkey ["escape"]` -> `hotkey ["ctrl", "a"]` -> `hotkey ["backspace"]` -> `type_text` in `follow_up`, and visually verify CJK text in a returned screenshot before submitting.
