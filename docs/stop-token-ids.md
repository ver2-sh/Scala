# Exact generated-token termination

`stop_strings` matches decoded text sequences. The OpenAI standard `stop` field
continues to select this independent feature.

`stop_token_ids` is a Norted extension accepting a non-empty JSON numeric array
of distinct unsigned 32-bit vocabulary IDs. The API and internal request use
integers, never token strings. Settings use the reusable
`UnsignedIntegerList` kind/value with per-element bounds. Negative numbers,
floats, quoted numbers, duplicates, empty lists, and out-of-range values are
errors. Input order is retained for deterministic profile identity. Inherit
removes the local override; it is not an empty list.

`required_stop_token_ids` belongs to the model's generation contract. These are
mandatory artifact semantics, separate from optional user controls and runtime
observations. Multiple terminal IDs are normal and work identically for any
model family. **Do not infer stop semantics from special-token names alone.** A
channel/message separator need not terminate generation. No terminal-name or
model-family registry is used.

## Resolution and ownership

Configured values follow runtime defaults → per-engine Settings → Model Profile
→ session-local load overrides → request overrides (where supported). A request
replaces the optional configured list. The backend receives a stable union of
mandatory artifact IDs followed by configured IDs not already present. Request
omission or replacement never removes mandatory IDs. Neither that union nor
runtime observations enter persisted override maps. Effective generation facts
retain mandatory and configured lists separately.

Supported settings use the common definition under `llama.cpp.stop_token_ids`,
`ninfer.stop_token_ids`, and `q27.stop_token_ids`. Runtime schemas gate support;
unsupported definitions remain visible/removable. The TUI accepts JSON integer
arrays and previews required values with the `Artifact` source.

Authoritative sources:

* Explicit `serving.required_stop_token_ids` in an attributable Norted package.
  Existing package bindings must match the manifest digest and payload size and
  SHA256 before those IDs are attributed.
* Norted deployment `manifest.json` with schema
  `norted.grep-student-gguf.v1`: exact target filename, sealed manifest and target
  and conversion identities, consistent parent/conversion lineage, and matching payload size/SHA256.
  Import preserves its complete package closure. An unrelated sibling manifest
  is not attribution. Seals follow the producer's sorted UTF-8 canonical JSON,
  including round-trip floats and Python exponent spelling.
* Hugging Face acquisition metadata in the primary file's directory at the
  selected immutable repository revision. `generation_config.json.eos_token_id`
  accepts a scalar or list; absent/null EOS falls back to `config.json` EOS.
  Metadata digests and required IDs are retained in the acquisition receipt.
  Arbitrary local sibling HF JSON is not implicitly trusted for a raw GGUF.
* Native `tokenizer.ggml.eos_token_id` remains separate GGUF native terminal
  metadata. It does not invent additional mandatory IDs.

HF's EOS configuration supports scalar/list token IDs; see the
[upstream generation contract](https://huggingface.co/docs/transformers/main_classes/text_generation).

## Runtime contracts

| Runtime | Exact source/variant | Support |
| --- | --- | --- |
| llama.cpp | `b10786`, `de8656bd94f1163188125542534e4bcbc9f9fb1f`; `managed-portable-exact-stop-v1` and `managed-portable-cuda13-exact-stop-v1` | Norted source overlay |
| NInfer | `863aa8a5f1e866db74f29f8999b83b4021398dee`; `ninfer-serve-exact-stop-v1-sm120a` | Norted source overlay, speculation off |
| q27 | reviewed `0.10.0`, `4770e053656af9aababdc49c81f280ad21b74986` | Unsupported; blocker below |
| Official/unpatched binaries | no matching reviewed overlay provenance | Unsupported |

The installer verifies immutable upstream commit/tree and original build/source
contracts before applying the owned patch in its private staging checkout.
There is no mutation of a shared pristine checkout. The complete patch digest is
in both source recipe and build provenance (`source_overlay_sha256`). Recipe
generations are compared only within a functional variant. Patched and unpatched
runtimes have distinct functional variants.
Changing the overlay requires a new recipe generation and a fresh source review.
The exact-stop recipes are pinned to their reviewed source revisions; they are
capability choices, not claims of globally latest upstream source.

Ordinary moving source runtimes remain available through the same providers:

* llama.cpp `managed-portable-v4` (CUDA 12) and
  `managed-portable-cuda13-v2` (CUDA 13) inspect current nightly releases and
  select the newest source admitted by the ordinary managed-source contract.
* NInfer `ninfer-serve-v2-sm120a` follows the canonical repository's
  default-branch HEAD.

Ordinary recipes have `source_overlay_sha256 = None` and do not advertise exact
token stops. Exact-stop recipes carry the owned overlay digest. Their functional
update families append `-exact-stop` to the ordinary family (NInfer uses
`managed-linux-x86_64-cuda-sm120a-exact-stop`) and start at recipe generation 1.
An exact-stop candidate cannot be a generic update for an ordinary installation,
even when the ordinary source is newer. Future reviewed source/overlay changes
receive a new recipe generation within the exact-stop functional variant.

Both installed and available runtime compatibility inspect mandatory artifact
IDs and the resolved Settings/Model Profile `stop_token_ids` key, including an
explicit empty list. Such profiles reject ordinary runtimes before installation
or load. They accept the reviewed exact-stop variant subject to other runtime
compatibility requirements; NInfer additionally rejects enabled speculation or
an enabled speculative backend. Profiles without token stops retain the ordinary
update stream and may also use an otherwise compatible exact-stop runtime.

Overlay SHA256:

* llama.cpp: `b6eb1527b0ee0305d0bc3b6cde7f3e45c580564c934e1fe39e86f78e0b18720e`
* NInfer: `5bf963935fbb1ce853a76f2759771d0f8c947071c82f92865435e69378b4a868`

### llama.cpp

The reviewed upstream server has text stops and native EOS/EOG but no arbitrary
request token-stop array. The overlay validates every integer against the loaded
vocabulary and stores the list in task/slot generation state. It compares the
newly sampled token before publishing its decoded text, ends the slot, and
reports Stop. The sampled terminal contributes one completion token, matching
native EOS accounting. A distinct native `stop_type: "token_id"` permits exact
manual verification, including when the selected token is also native EOS.

Explicit token-stop requests disable per-slot speculative sampling: the runtime
must not sample later tokens and trim them afterward. Text stops, native EOG,
structured output, and tool decoding retain their independent paths. Explicit
token termination reports Stop even after tool content. `ignore_eos` cannot turn
off the explicit token comparison. Because native-EOS suppression masks EOG
tokens during sampling, combining `ignore_eos` with an explicit native EOG ID
is rejected before generation. Other explicit IDs remain enforceable.

### NInfer

The original frontend `StopPolicy` already supports vocabulary-validated exact
IDs, union with model defaults, and exclusion of the terminal from output.
The reviewed HTTP parser exposed only text stops. The overlay carries integer
IDs from the HTTP request to that native policy and keeps explicit terminal
finish reasons as Stop even when tool calls preceded the terminal.

The original output policy previews accepted token spans. To satisfy the stricter
no-later-sampling contract, explicit token IDs require speculation off. The
settings schema rejects configured speculative modes for token stops, and the
backend independently rejects the combination. This is an explicit restriction,
not silently changing the selected speculative policy. Existing v2 source
variants retain their previously reviewed features but do not gain token stops.
`REVIEWED_SOURCE_BLOBS` still describes pristine upstream material; the Norted
patch has its own digest. The modified C++ units were syntax-checked against both
the installed legacy snapshot and the current reviewed snapshot; no NInfer
model execution is claimed by the manual llama.cpp evidence.

### q27 blocker

This integration's reviewed engine always uses NextN/MTP for greedy decoding;
there is no proved MTP-disable control. `Engine::decode_step` dispatches greedy
work to `spec_round`; `post_round` folds accepted rows, advances state, then
checks its single native EOS while emitting the already-produced block. The
conductor also has fused round/commit paths. Replacing that final scalar check
with a set would prevent exposure but would not prevent later sampling or state
commit. `Q27_SAMPLE_PLAIN` applies to sampled requests only, and cannot supply the
required generic greedy/conductor contract. A correct implementation needs a
strict single-token generation path across those execution modes, beyond a
small request/termination overlay. Both source and release variants therefore
reject exact token stops; required artifacts cannot run through q27. Text-stop
fail-closed behavior and unrelated structured-output limitations are unchanged.

## API example

```json
{
  "model": "my-profile",
  "messages": [{"role": "user", "content": "Hello"}],
  "stop_token_ids": [1, 2],
  "stop": ["END"]
}
```

Either independent condition terminates output. Numeric IDs must exist in the
selected model vocabulary; there is no allowed-ID lookup table.

## Manual evidence

See [validation evidence](stop-token-ids-validation.json). On the local Grep 4B
Q4_K_M artifact, IDs 248044 and 248046 were forced independently from the same
configured list; arbitrary token 42 was also forced. All native streaming and
aggregate calls ended with `stop_type: token_id`, one generated token, and no
content. OpenAI calls returned `finish_reason: stop`; streams terminated cleanly.
Without that third token configured, the forced-token control returned `KKK`
and length termination. Text `stop: ["K"]` still stopped independently. Invalid
lists and vocabulary IDs returned HTTP 400. Explicit third-token stopping also
worked with native EOS ignored; an explicit native EOG combined with
`ignore_eos` was rejected. Both text and token controls also passed when supplied
together. No model bytes were modified.

The imported temporary profile was loaded with the requested Grep generation
values and numeric stops, then reloaded with its stop-ID override unset. A
loopback capture scoped to that diagnostic backend proved the wire union:
`[248044,248046,11]` for request override `[11]`, and `[248044,248046]` when the
request omitted IDs. Both gateway streaming and aggregate requests stopped after
one token with no content. Chat Completions and Responses also accepted the
extension. Malformed arrays were rejected by the public parser. The profile used
no text stop strings.

Selecting the original unpatched CUDA-13 v2 runtime for that inherited profile
failed during runtime selection, before any model generation.

The manual binary was built from the private patched source view using the full
managed CUDA-13 recipe (real-code targets 75, 80, 86, 89, 90, 120a), then loaded
through an isolated Server profile/runtime installation. Diagnostic calls used
`logit_bias: [[ID, 10000]]` on `/completion` and `logit_bias: {"ID": 10000}` on
`/v1/chat/completions`, with greedy sampling and a 1024-token output budget.
Local raw request/response evidence and validation logs are retained at
`/srv/norted/scratch/multi-stop-evidence`; copied model bytes and diagnostic
services were cleaned up afterward.

Existing validation passed: `cargo fmt --all --check`, `cargo check --workspace`,
`cargo clippy --workspace --all-targets --all-features -- -D warnings`, and
`cargo test --workspace` (214 passed, 0 failed, 1 existing ignored). No new tests
were added. NInfer's four modified C++ translation units passed syntax checks;
its overlay has not been exercised with a running model.
