# Settings and Model Profiles terminal review

Redesign based on `ab06f27` on `fix/runtime-settings-inheritance`. The checkout was clean at that commit before editing. Changes are confined to Norted-Server's TUI; no configuration resolver, engine adapter, model artifact, runtime selection, or user preference was changed by the implementation.

## Layout

Both editors use column rectangles calculated once by `UiLayout` for Setting, Value, Source, Action, and (when width permits) Status. Rendering and mouse hit testing consume those same rectangles. Every row reserves the Action column. Category rows are separate, scalar rows occupy one terminal line, and identifiers live in the selected-setting details.

Profiles use a labelled summary, a full-width Model field, version/accelerator/variant runtime summary, dedicated actions, and one settings toolbar. Wide terminals have a profile list. Compact terminals have previous/next selector controls and a focused action menu (`a`). Details (`i`, with `[`/`]` scrolling) retain full identifiers, model paths, values, baseline policy, layer values, source, constraints, support and validation explanations. The editor names the scope and setting, gives the value its own line, and separates Save/Cancel from field errors.

At the supported 46×13 minimum, metadata and details use the focused view and status explanations are in details; the main view retains a selector, toolbar, headings, category and setting row. The captures are text exported from the running crossterm application in a PTY and decoded with pyte, not manually composed mockups. Text exports do not preserve theme colours.

## Captures

| Size | Settings | Model Profiles |
| --- | --- | --- |
| 46×13 | [Settings](46x13-settings.txt) | [Profiles](46x13-profiles.txt), [actions](46x13-profile-actions.txt) |
| 100×30 | [Settings](100x30-settings.txt) | [Profiles](100x30-profiles.txt) |
| 160×45 | [Settings](160x45-settings.txt) | [Profiles](160x45-profiles.txt) |

Additional captures show [validation](100x30-validation.txt), [unsupported local override](100x30-unsupported-local.txt), [unsupported after Inherit](100x30-unsupported-inherit.txt), [mouse Save](100x30-mouse-save.txt), and [mouse Inherit](100x30-mouse-inherit.txt).

## Exercised

- Actual Settings and populated Model Profiles at 46×13, 100×30 and 160×45; inspected their terminal output.
- Server, llama.cpp, ninfer and q27 scope navigation; q27 and llama.cpp profiles; full model paths/runtime IDs in scrollable details.
- Adjacent inherited and overridden profile values, unsupported fields, selected rows, search and overrides-only filtering.
- Server edit validation error, Save/Cancel, explicit override followed by Inherit, scoped reset confirmation and execution.
- q27 `top_k=20` saved through the TUI while runtime identity and unrelated schema fields remained available; filtered Inherit removed the row without a stale target.
- An unsupported `q27.reasoning_effort=high` override was seeded only in an isolated copy: editing stayed disabled and Inherit successfully removed it. The inherited unsupported row remained visible.
- Keyboard navigation, mouse navigation, mouse value editing/Save/Inherit, mouse wheel scrolling followed by keyboard selection, focused details scrolling and live resize from 100×30 to 46×13.
- A long duplicated profile ID stayed distinguishable in the compact selector; [profile details](100x30-long-profile-details.txt) exposed the complete ID, model path and runtime ID. Details scrolling is capped using ratatui's wrapped line count so it cannot disappear into a blank panel.
- A long q27 system prompt was entered through the value editor, saved, truncated in its table cell, and retained in details. See [long editor](100x30-long-editor.txt) and [saved value/details](100x30-long-details.txt).
- The mouse pass found and fixed hidden toolbar hit targets intercepting Save; covered targets are now removed while an editor is open.

Validation used copied configuration/profile/runtime state under `/tmp/norted-ui-review` with separate XDG directories and port 18743. Original user configuration and runtime/model files were not modified. No new test code was added.

## Live precedence smoke

The RTX 5090 was free during this run. The isolated q27 profile loaded successfully with Settings top-k 40, profile top-k 20, temperature 0.8 and context 4096. Two real API requests completed successfully. A packet capture restricted to that backend's private loopback port recorded:

```json
{"max_tokens":8,"messages":[{"content":"Reply with one word: hello.","role":"user"}],"model":"qwen38-27b-q27-q6","stream":false,"temperature":0.8,"top_k":10}
{"max_tokens":8,"messages":[{"content":"Reply with one word: hello.","role":"user"}],"model":"qwen38-27b-q27-q6","stream":false,"temperature":0.8,"top_k":20}
```

Both returned `hello`. Settings and profile files were byte-for-byte unchanged across those requests. After unloading, profile Inherit resolved to 40 with Settings provenance; changing Settings to 60 resolved to 60 with Settings provenance. The isolated backend and serving process were stopped. This verifies the requested execution sequence on the installed q27 runtime, not every engine or runtime variant.

## Checks

- `cargo test --workspace`: 214 passed, 0 failed, 1 existing ignored test.
- Final TUI library tests: 20 passed.
- `cargo clippy -p norted-tui --all-targets -- -D warnings`, `cargo fmt --all --check`, `cargo build -p norted-server`, and `git diff --check`: passed.

The original screenshots were described in the task but were not attached as image files in this session. The described per-row column movement and paragraph header were confirmed in the audited source. No live inference claim is made for llama.cpp or ninfer, and no exhaustive matrix of historical runtime versions was run.
