# Supported controls with unknown defaults

Validated on 2026-09-06 on `fix/runtime-settings-inheritance`, after fetching the branch and confirming that its latest remote commit was still `b4a980ec2c46f37e0f0ead357cdc9385bc2b71e1`.

Removed `finalize_llama_exact_schema`: it changed every supported definition without a `default_preview` into an unsupported definition. Exact flag availability, executable adapter paths, value validation and model requirements still determine support; absence of a reported baseline now leaves the optional preview absent. No TUI, core resolver, q27 or ninfer implementation changed. Inspection found no equivalent default-to-support gate in q27 or ninfer. Two expectations in an existing llama.cpp test encoded the removed behavior and were corrected; no tests were added.

## Exact runtime and CLI validation

The installed llama.cpp b10786 / CUDA / managed-portable-cuda13-v2 executable (revision `de8656bd94f1163188125542534e4bcbc9f9fb1f`, executable SHA-256 `4dbd83f77938ac25f4f8bdd4b300b07264c94ba36210b8d4edc3de5580332754`) advertises `--cpu-range`, `--cpu-range-batch` and `--n-cpu-ffn` without default values in its actual `--help` output. All three now report supported with null baseline values. Their existing translations use those exact flags. Affinity definitions validate non-empty strings; FFN layers validate unsigned integers. A negative FFN value was rejected. No new affinity syntax or host-index validator was introduced.

Using isolated copied configuration and runtime state under `/tmp/norted-unknown-default-review`, with port 18744:

- Saved Settings affinity ranges `0-3` and FFN layers `0`; effective inspection showed each explicit value with Settings source.
- Saved profile affinity `4-7`; it won with Profile source. Removing it exposed Settings `0-3`. Removing the Settings keys left no effective value for any of the three controls.
- Each CLI inspection reopened persisted state. After the sequence, Settings and profile JSON matched the original copied state structurally, with no unknown placeholders, pinned inherited values or invocation values.
- q27 `top_k`: Settings `40`, profile `20`, compatibility `recommended`; profile Inherit restored Settings `40`, then Settings Inherit removed the key. No blanket runtime-compatibility failure occurred.

## Live execution and provenance

Two real loads of the existing Gemma 4 31B Q6_K GGUF reached `running` on the RTX 5090. CPU indices 0–19 were available to the validation process. Temporary context length was 4096.

The first load used temporary affinity `8-11`. Reading the actual backend process command line confirmed `--cpu-range 8-11 --cpu-range-batch 0-3 --n-cpu-ffn 0`. Launch provenance recorded affinity source `invocation`, and batch affinity/FFN source `settings_override`. Reopening the profile still showed its persisted `4-7` value. After both layers inherited, the second process command line omitted all three flags and its effective provenance omitted all three values. Both backends were unloaded. No inference request was made in this follow-up; q27 validation here was configuration/compatibility only.

## TUI and genuine unsupported controls

Actual 160×45 PTY captures show the existing layout and details unchanged:

- [Settings unknown default](160x45-unknown-default-settings.txt).
- [Profile unknown default after Inherit](160x45-unknown-default-profile.txt).
- [Profile saved affinity with value and source](160x45-unknown-default-override.txt).
- [Unsupported local expert override](160x45-experts-unsupported-local.txt) and [after removal](160x45-experts-unsupported-inherit.txt).

The Settings editor saved affinity `0-3` with Settings source; the profile editor saved `4-7` with Profile source. Inherit removed each local key. Number of active experts remained unavailable because the GGUF does not prove an architecture-specific `expert_used_count` override. CLI setting was rejected with that explanation. A stale override seeded only in the isolated profile remained visible as invalid and was removable through TUI Inherit. Runtime identity and other controls remained available.

Missing flags, absent adapter paths, expert metadata checks, accelerator checks, multi-argument/dependency validation and template/artifact identity checks were left intact. Models, templates and runtime binaries were not modified.

`./validate.sh` passed formatting, workspace check, workspace clippy with warnings denied, and all existing workspace tests: 214 passed, 1 existing ignored. `cargo build -p scala` and `git diff --check` passed. No historical runtime-version matrix was run.
