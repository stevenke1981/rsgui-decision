# Verification status — 2026-11-16

The GUI decision core and persistent MCP integration are implemented. This
checkpoint does not claim complete live desktop acceptance or a speedup result.

| Scope | Observed result |
| --- | --- |
| Rust workspace, all features | 113 tests passed; 4 live desktop tests ignored by default |
| Decision core | 18 offline tests passed, included in the workspace total |
| Format and Clippy | Passed, warnings treated as errors |
| Windows release build | Passed |
| MCP | 19 tools; initialization, protocol errors, lazy DLL and missing-DLL checks passed |
| Local replacement | Staging, checksums, old-process preservation, rollback, tamper refusal and restore passed in temporary directories |
| Lazy old runtime | An old MCP with no DLL loaded before upgrade successfully loaded the new ABI-compatible runtime afterwards |
| Windows runner compatibility | UTF-8 BOM stdin and short TEMP aliases reproduced and fixed; combined local regression passed |
| Independent review | GPT-6.1 Terra xhigh reviewed production changes and focused follow-ups; no concrete blocker found |
| Real desktop workflow | In progress; live click stopped before button input because the cursor moved after reaching its target |
| Performance | Not measured to completion; no claimed speed multiplier |
| Real Gemini API | NOT_RUN; no live provider call or provider charge was made |
| Local production installation | NOT_RUN at this checkpoint |

The green [Windows CI run](https://github.com/stevenke1981/rsgui-decision/actions/runs/43732823445)
tested commit `15a2f1ed7e5b1ab9f344148682902e7a3a8036bf`. Later pointer diagnostics
and operator-gated fixture changes require their own checks; their commit is
visible in the pull request history.

## Interactive fixture and pointer diagnosis

The fixture can wait for an explicit operator click instead of starting merely
because Windows activated it. It sends no test input before that click.

```powershell
$env:AUTOGUI_LIVE = '1'
$env:AUTOGUI_MCP_EXE = (Resolve-Path .\target\release\rsgui-decision.exe).Path
$env:RSGUI_LIVE_MANUAL_START = '1'
$env:RSGUI_POINTER_TRACE = '1'
cargo test --all-features --locked --test live_app_control integrated_decision -- --ignored --nocapture --test-threads=1
```

Select the test-owned `rsgui decision fixture` window and press `開始測試`.
Leave the mouse and keyboard idle while it runs. Diagnostic output compares
default and explicit-display movement, physical cursor readback, and samples at
requested 0/2/10/25/100 ms intervals with actual elapsed times. The final exact
pointer/proof checks remain enabled; a mismatch does not trigger a click retry.

The first timed capture showed exact arrival at 0 ms followed by position
changes within 100 ms; reported dimensions were 1920×1080 and physical cursor
readback agreed. This does not establish a DPI or rounding defect. The source
of the later motion remains unresolved, so live input and performance are not
reported as passing.

The explicit-display method includes its own arrival wait. Its immediate
readback and the default path's later samples are diagnostic observations,
not a controlled comparison proving one normalization formula is wrong.

The existing Computer Use host did not allow access to this newly built test
executable even after chat authorization. Operator activation is therefore used
for this fixture; host app permissions and Windows focus restrictions were not
disabled.
