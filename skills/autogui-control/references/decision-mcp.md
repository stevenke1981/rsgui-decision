# Decision MCP & Gemini Decision 1 Runbook

## 1. Credential & Mode Preflight (`decision_status`)

- Call `decision_status {}` before relying on multi-candidate Gemini Decision 1 evaluation (`RSGUI_DECISION_MODE` defaults to `auto`).
- `autogui-control.exe mcp` / `rsgui-decision.exe` reads **only** its invoking `Process` environment (`GEMINI_API_KEY` prioritized over `GOOGLE_API_KEY`; `.env` files and live Windows Registry changes are not polled). Workspace hooks may detect `User`-scoped keys even while the live MCP process lacks them.
- If `provider_ready` is `false` despite a configured Windows User/System variable:
  - **Preferred (no plaintext on disk)**: Cold-restart the host Agent from a newly launched OS shell/Explorer process that already holds the updated variable (`[bool]$env:GOOGLE_API_KEY`).
  - **Scoped MCP override**: Add `"env": { "GOOGLE_API_KEY": "..." }` under `mcpServers.autogui` in the host MCP config (e.g., `~/.gemini/config/mcp_config.json`) and restart the MCP server. Note that host tools creating `mcp_config.json.backup_*` backups will snapshot plaintext entries.

## 2. Hybrid `decision_select` Strategy (Web & App UI)

- **Know the exact single target or template**: Pass 1 candidate (`fast_path_single`) or static `template` paths (`fast_path_template` via `locate_all`) for zero remote calls.
- **Ambiguous multi-candidate selection (Gemini Decision 1)**: `decision_evaluate` / `decision_select` strips `(x, y)` coordinates from model prompts, sending only the downscaled inference PNG, `goal`, and each candidate's `id` and `description`.
  - Write concrete visual descriptions naming **exact visible text/icon** and **relative position inside its container** (e.g., `"The 'x1' button (leftmost option in the 'x1 x2 x3 x4' count control inside the settings popover)"`). Avoid generic descriptions like `"settings button"`, which trigger `GEMINI_LOW_CONFIDENCE`.

## 3. Dynamic Web Overlays, Focus & `capture_id` Reuse

- Dynamic web UIs (CSS hover fades, popover animations, post-typing reflows) can exceed `allow_caret_drift` tolerances while inspecting a screenshot, causing `observed pixels/display changed; observe again` on a prior `capture_id`.
- Omit `capture_id` in `decision_select` (`activate: true`, or `false` if already focused) when acting on dynamic web overlays so an atomic baseline frame is captured immediately before execution.
- Clicking outside an open web modal/popover may dismiss the backdrop without focusing an underlying textarea; capture/inspect caret focus after dismissal before running `type_text`.
