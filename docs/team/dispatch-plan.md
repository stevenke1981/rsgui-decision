# rsgui-decision delivery

Owner: root implementer. Requested by the owner on 2026-11-15.

## Scope and acceptance

- Reuse rsautogui native library, CLI compatibility, persistent MCP, cancellation,
  single-use screenshot proof and corner fail-safe.
- Rewrite the GUI-relevant Gemini Decision contract in Rust: bounded choices,
  noul/choice probabilities, image input, strict validation, local safe fallback,
  request deadlines and cooldown. The coding-agent skills/retrieval/memory/
  compression domains are not desktop execution features and are not ported.
- Add observe, evaluate, act and step tools. Each execution consumes one stored
  candidate, checks the target and captured frame, sends at most one action and
  waits locally for a stable frame. Settled pixels never imply task success.
- Validate offline provider failures, MCP compatibility, cancellation, stale
  observations and real input on a test-owned Windows fixture.
- Compare the current persistent MCP path and integrated path on the same host;
  disclose timings and invocation counts without promising a speed multiplier.
- Commit, push a PR to a new private stevenke1981/rsgui-decision repository,
  independently review, pass CI, merge main, release and replace the identified
  local installation with a checked, recoverable artifact.

## Roles and write scopes

| Instance | Model | Role | Scope |
| --- | --- | --- | --- |
| root | host model | implementation, integration and delivery | project |
| source_inventory | GPT-6 Luna medium | source inventory | read-only upstream |
| architecture_adviser | GPT-6 Astra high | bounded architecture advice | read-only |
| decision_core | GPT-6.1 Sol xhigh | Rust provider/contract | isolated worktree, decision/ |
| final_reviewer | GPT-6.1 Terra xhigh | independent focused review | read-only candidate |

The adviser and source inventory have returned source-grounded findings, not
implementation or runtime passes. No external judging model is used.

## Dependencies and gates

Plan -> native integration and isolated core implementation -> local verification
-> PR -> CI and one independent focused review -> repairs of actual findings
-> merge -> build/package -> staged install/upgrade/rollback validation -> delivery.

## Source provenance

- rsautogui: https://github.com/urtiger101-tw/rsautogui,
  `1f7b58e4ffdd61e0e3788c7980a142c9b485897a`, MIT.
- gemini-decision-1: https://github.com/stevenke1981/gemini-decision-1,
  `e724a3bee6dec9d91635c3798400e96b4f0a5e85`, MIT.
- The optional Gemini share returned an application shell without the shared
  conversation; its design contents are NOT_RUN, not used as evidence.
- Installed Agent Team Workflow 1.0.0 is available under the current steven
  profile. The referenced eda verification kit/router is absent; use the actual
  project's Cargo checks and fresh command evidence without claiming that kit ran.

## Advisory decisions

Preserve the working native execution Session rather than building a new input
stack. Avoid an action cache and blind multi-mutation batches. Send independent
image questions in one provider request; a model may choose only candidate IDs.
On failure or uncertainty observe/replan, never choose the first action silently.
Default to no provider requests. The official Decisions API supports inline PNG
input and costs $0.10 per million input tokens on its paid tier as checked on
2026-11-15; free-tier eligibility is account-specific. Live requests need an
explicitly configured key/mode and are reported separately from mock tests.

## Local replacement target

Observed existing installation:
`%LOCALAPPDATA%/Programs/AutoGui/autogui-control.exe` with
`autogui_runtime.dll`. Preserve the pair and the original install metadata in a
timestamped backup before replacement. Add rsgui-decision.exe to the existing
user-local bin directory. Do not change host approvals or browser policies.
