# Native serving architecture audit — 2026-09-10

Starting commits: Norted-Server `0d545d969c32a543d7d6b6de5124d786bbecb57e`;
Norted `6b05f0cf32a001c17ccf8e25a84b846afc262dd2`.
Local branches: `fix/native-runtime-serving` and
`fix/portable-generation-semantics`. No push, merge, training or publication.
No tests added. Published model payloads and manifests were not edited.

## Removed serving extensions

Removed llama.cpp `overlays/exact-stop-token-ids.patch` and
`overlays/exact-stop-token-ids-v2.patch`, and NInfer's
`overlays/exact-stop-token-ids.patch`. Removed installer patch application,
overlay digests/provenance fields, fixed stop revisions, their catalog entries,
and all four CUDA 12/13 llama.cpp V1/V2 exact-stop variants plus
`ninfer-serve-exact-stop-v1-sm120a`. No migrations or aliases replace them.

Removed `ModelGenerationContract`, package/HF-derived required stops, API and
adapter `stop_token_ids`, settings definitions, persisted list values used only
for those settings, load/request merging, schema previews and stop admission
checks. Package discovery, seal/hash/size checks and lineage remain; serving
fields in historical manifests are not interpreted as inference instructions.

No supported **serving adapter** has a native arbitrary token-ID stop consumer.
NInfer's upstream CLI/internal C++ stop options do exist, but its served HTTP
request API does not expose them (checked at `b88c0f6fc7e999f13eb2fcf7fc9105ed79a91868`).
An unused engine-neutral API therefore does not remain. Native EOS/EOG and
supported ordinary textual stop controls remain authoritative.

## Upstream student generation semantics

Exact checkpoint inventories:

- [Qwen3.5-4B](https://huggingface.co/api/models/Qwen/Qwen3.5-4B/revision/851bf6e806efd8d0a36b00ddf55e13ccb7b8cd0a)
  at `851bf6e806efd8d0a36b00ddf55e13ccb7b8cd0a`.
- Qwen3.5-9B at `c202236235762e1c871ad0ccb60c8ee5ba337b9a`.

Neither pinned snapshot contains `generation_config.json`; direct retrieval of
that filename returns 404. The pinned model `text_config.eos_token_id` is
`248044`; the pinned tokenizer declares `<|im_end|>` as EOS, resolving to
`248046`. The complete evaluation terminal set is `[248044, 248046]`.
This is reconstructed from upstream model/tokenizer metadata, not attributed to
a nonexistent upstream generation-config file and not a Grep token convention.

The evaluator now loads the entire pinned `GenerationConfig` when provided.
When the snapshot lacks it, it constructs the model generation configuration
and retains both the declared model EOS and tokenizer EOS. Metadata hashes and
vocabulary bounds are checked; an untracked generation file fails closed.
The exact local Transformers configuration-only validation produced both IDs
for both checkpoints, without loading weights or generating new science.
Greedy evaluation and the existing protocol remain unchanged.

Evaluation stage v3 owns the new implementation. Exact historical v2 code hashes
remain verifiable; v2 results cannot be reused as v3 evaluation results. The
frozen policy dictionary remains an expected scientific descriptor, not the
source of generation stop IDs. Historical diagnostics and model publications
remain unchanged.

Future GGUF manifests omit `required_stop_token_ids`, `required_stop_tokens`,
and the claim that a special runtime is required. They describe native
model-format/runtime termination. Historical descriptors are admitted only for
read-only verification/status of their sealed recipes; new builds use the new
portable descriptor. No historical artifact was rewritten.

## Stock portability and parity

Fresh unpatched upstream llama.cpp:
`434ddbbc0e30522e897670681e503b797c12b7c1`, built with CMake Release,
CUDA enabled and architecture `120a`. Source `git status --short` is empty.
No Norted source overlay or special runtime family was used.

Standalone `Norted-Grep-Qwen3.5-4B-Q4_K_M.gguf`:
2,783,446,400 bytes; SHA-256
`aff8ac41d95fd53343abfb28015752c0c1fb912f14e79044050828ece1bc81f0`.
Its directory contains only the GGUF. The hash matches the immutable published
payload. The stock server loaded it and used its embedded template, SHA-256
`a4aee8afcf2e0711942cf848899be66016f8d14a889ff9ede07bca099c28f715`.
Ordinary chat completed normally.

Separate stock `/completion` probes used ordinary `logit_bias` to make each
terminal token observable. Each returned exactly one predicted token, empty
visible content, `stop: true`, and `stop_type: "eos"`:

| Sampled token | Native result | Request stop controls |
| --- | --- | --- |
| 248044 (`<\|endoftext\|>`) | EOS after 1 token | No stop IDs or stop strings |
| 248046 (`<\|im_end\|>`) | EOS after 1 token | No stop IDs or stop strings |

Stock `src/llama-vocab.cpp` classifies both spellings as EOG;
`tools/server/server-context.cpp` terminates via `llama_vocab_is_eog()`.
The probes force sampling, not a replacement termination implementation.

Through Norted-Server, the external stock executable proved ToolCalling,
parallel calls and StructuredOutput for standalone Grep, packaged Grep, and
non-Norted `llmfan46/Gemma-4-Queen-31B-it-uncensored-heretic-Q6_K.gguf`.
The same capability algorithm applies to each. The package's historical
required-stop metadata is ignored for inference. Grep and Gemma both passed
aggregate/streaming tool and schema requests, named tool choice, assistant call
history, tool result IDs and the unchanged Retrieval finalization schema.
Explicit generic `reasoning=off` and context 4096 were used for the bounded
manual chat checks; these were validation profiles, not producer defaults.

## Capability and update policy

The proof is local to the verified executable, model identity/metadata, selected
configuration and live process. `/props`, `/apply-template`, a valid forced-tool
response, and a completed constrained JSON object establish support. There is
no managed-source, revision, model-family or model-origin allowlist. Missing
proof fails closed. Both runtime-default Jinja and explicitly enabled Jinja can
qualify. The model-info structured-output flag now follows adapter serving
features rather than the mere presence of a configurable setting. Configuration
views remain distinct from running observations.

The current upstream `--log-jsonl` / `--no-log-jsonl` options were classified as
operational controls; their environment alias is isolated to preserve textual
stderr for startup progress and diagnostics. They are not admission requirements.

Live catalog refresh returned no provider errors: ordinary official and managed
llama.cpp candidates follow b10883 (`91f6a6cf361385700bbe15981f0f39909df77498`),
including the unchanged CUDA 12 v4 and CUDA 13 v2 recipes. NInfer follows
`b88c0f6fc7e999f13eb2fcf7fc9105ed79a91868` on its normal default-branch stream.
There are no exact-stop candidates and no b10786 pin.

Retrieval still gates on the loaded adapter's ToolCalling and StructuredOutput.
Its tasks, scoring, fixtures, tool definitions and protocol have no diff from the
starting commit. Manual finalization used its existing schema successfully.
No score was rerun or tuned. The prior patched-runtime 0/100 Q4 result remains
historical model/deployment-quality evidence, not a capability verdict.

## Validation and limits

- `cargo fmt --all --check`: passed.
- `cargo check --workspace`: passed.
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`: passed.
- `cargo test --workspace`: 214 passed, 1 pre-existing ignored; doc tests passed.
- Norted frozen unittest discovery: 91 passed.
- Norted `compileall` and both repositories' `git diff --check`: passed.
- Norted offline student-GGUF status: both sizes and all four targets current.
- Norted deep offline verification: passed (exit 0), both student sources and
  all four GGUFs; preserved trace replay, merge verification, hashes and
  structural checks. Command: `uv run --project . --frozen python
  scripts/build_grep_student_gguf.py --grep --student-gguf all --verify --offline`.

Evidence: [native-serving-validation.json](native-serving-validation.json).
Full local command logs are in `/tmp/norted-*-final.log` and
`/tmp/norted-gguf-verify.log`; manual runtime records are under
`/srv/norted/scratch/native-portability/`.

No portability or producer-capability blocker remains. Capability probes are
bounded (30 seconds per generation probe); an engine/template that cannot prove
support within those bounds remains unadvertised. These observations establish
technical serving support, not Retrieval quality, complete support for every
possible JSON Schema keyword, or a new scientific evaluation score.

## Forward-compatible help admission follow-up — 2026-09-10

Starting branch `fix/native-runtime-serving`, head
`e0510c92aec34317f9c2914ca66b42d90349eee7`.

Removed the fatal `unclassified_llama_help_option` gate and its exhaustive
classification helper. Unknown upstream options are ignored, never added to
setting definitions or forwarded as native arguments. Required `--model`,
`--alias`, `--host`, and `--port` controls still gate admission, now using the
existing option-header boundary matcher rather than substring matching.
Configured settings still require their individual advertised controls and
semantics; unsupported features remain unadvertised without live proof.

Updated the existing ownership test, without adding tests, to exercise normal
help, the same help plus `--brand-new-control VALUE`, missing required flags
(including misleading longer option names), unchanged setting IDs, and rejection
of arbitrary native arguments. Existing setting-contract tests retain rejection
of configured temperature when `--temp` is absent. Argument/environment guards,
identity/hash checks, model compatibility and managed source provenance remain.

Reviewed `MANAGED_NATIVE_ARGUMENTS`: no entries removed. The entries still guard
operational controls and collisions; the list no longer classifies runtime help.
In particular, stock `--log-jsonl` moves logging to JSONL on stdout, affecting the
adapter's stderr progress/diagnostic consumer. Its guards and environment
isolation remain for that operational reason, with the comment clarified.
Removed stale “exact runtime/template/Jinja tuple” capability rejection wording.

Validation: all four requested cargo commands passed; workspace tests report
214 passed, zero failed, one pre-existing ignored. Test/build logs include
incremental-cache cleanup warnings; the requested clippy run with `-D warnings`
passed. Logs: `/tmp/norted-admission-{check,clippy,test,build}.log`.

The rebuilt server loaded standalone Grep with clean, unpatched stock llama.cpp
`434ddbbc0e30522e897670681e503b797c12b7c1` (the branch's existing current-stock
build). The existing bounded history probe passed named tool choice, assistant
call/tool-result history, and constrained JSON finalization. Load evidence:
`/tmp/norted-admission-grep-load.json`; request/response evidence:
`/srv/norted/scratch/native-portability/grep-stock-history.json`.

Managed source discovery still scans moving upstream nightly releases. No
source-discovery, patches, runtime variants, stop-token-ID abstractions,
producer-specific serving paths, Retrieval protocol/tasks/scoring, or Norted
files changed. No Retrieval score rerun. No push or merge.

The same rebuilt adapter and stock executable also loaded non-Norted Gemma and
passed the same named-tool/history/StructuredOutput probes. Evidence:
`/tmp/norted-admission-gemma-load.json` and
`/srv/norted/scratch/native-portability/non-norted-stock-history.json`.
ToolCalling and StructuredOutput qualification remains runtime/model/template
behavior-based, independent of producer or package provenance. No remaining
Blocking or Material finding was identified in this scoped follow-up.
