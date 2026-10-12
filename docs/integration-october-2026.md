# October 2026 inference-runtime integration

## Review state and qualification boundary

Starting checkout: clean `master` and `origin/master` at
`3e38c0108039f0c821940e1a9efed7976db158ea`, workspace version **0.1.19**.
Implementation branch: `integration/october-2026-inference`. Upstream state and
branch were rechecked before changes and before the local review commit. The
workspace version and application installation/update contracts are preserved.

The implementation qualifies adapter compatibility from immutable upstream source,
release metadata, actual package bytes and cheap synthetic contracts. It is **not
live-model validation**. No GPU runtime was built or activated, no model was loaded,
no inference/benchmark/conversion was run, and no production service, Settings or
Model Profile was changed. Only downloaded official CPU llama-server `--help` was
executed. Packages were inspected without executing Q27 or Windows NInfer.
Installed-runtime integrity/probes, host requirements and exact loaded-pair
qualification still govern actual operation. No runtime is implicitly selected,
replaced or upgraded. No artifact conversion, telemetry, push, release or deployment.

## Frozen upstream identities

| Runtime | Tag/object | Source commit | Source tree |
| --- | --- | --- | --- |
| llama.cpp stable | `v0.6.0`, annotated object `69ecae922d2b60c1fa8c590279f4e2099af75111` | `d81235049384534c167caea52b85a694f6103d14` | `a47b4d437e9769fc34c0116371412b5af5de67d1` |
| llama.cpp official nightly | `b11565` | `f9a95aeb2096d0c0218e0cd6e6d901cd37be798a` | `52ab51caa5c43d40486ad4158caa3665200a424f` |
| Q27 | `v0.14.5`, annotated object `31cbde4a79bfc909be7956f9761393b51abb5f2d` | `2945e35bc82b052cd147618bbaa8de435cfe73c0` | `e5124b6e045904ee6b08cee5964df9bc9c32447b` |
| Canonical NInfer Linux v3 | exact source, no tagged binary release | `81c8ce093b2c1646a87566a8e59d807fcf0ec95c` | `8b42cb632277161d47aa0a67db55ca8400bf7118` |
| NInfer native Windows v3 | `natpate/ninfer-windows` `v0.9.1` | `fe115cdecd16acc96df72206053e94f5c8f25214` | `35c0b544bb109311f9c0244b985d052aee4cb379` |
| Preserved canonical NInfer v2 | exact source | `d49296868dcc17bd478ec185f0d3a801bcc0bf56` | `8e2f0275fc533cf11fe05a4ac3ac85f00eb91c72` |
| Preserved Windows v2 | `v0.7.1` | `aa64ee5353b3c79a0a4301a0d60f1716fb0451be` | `fd35700dbbf797b6e46114efd46bbc569e151844` |

Source links: [stable llama.cpp](https://github.com/ggml-org/llama.cpp/tree/d81235049384534c167caea52b85a694f6103d14),
[nightly llama.cpp](https://github.com/ggml-org/llama.cpp/tree/f9a95aeb2096d0c0218e0cd6e6d901cd37be798a),
[Q27](https://github.com/signalnine/q27/tree/2945e35bc82b052cd147618bbaa8de435cfe73c0),
[Linux NInfer](https://github.com/Neroued/ninfer/tree/81c8ce093b2c1646a87566a8e59d807fcf0ec95c),
[Windows NInfer](https://github.com/natpate/ninfer-windows/tree/fe115cdecd16acc96df72206053e94f5c8f25214).
Moving HEAD was not chased after freezing these revisions.

### Release packages and SHA-256

The stable llama.cpp release **403937899** is a pointer release. Its asset
**613151064**, `nightly-tag.txt`, is 7 bytes, SHA-256
`5677c5a4561fad44f2a58fc14187dd9cde253874584e958b37094c723ea8e8a5`,
and contains `b11429` plus newline. The actual b11429 tag resolves to the stable
commit/tree above. Release `target_commitish` is stale and is no longer used as
immutable source authority; tag/commit/tree resolution is verified separately.

| Package | Release / asset ID | Bytes | Archive SHA-256 |
| --- | --- | ---: | --- |
| llama b11429 Ubuntu x64 CPU | stable pointer / `613152023` | 17693462 | `f6d25dde8f51133143d1453da4fd5f73b145127177612a283bf7995957af3392` |
| llama b11565 Ubuntu x64 CPU | `410418472` / `633281040` | 17712716 | `c571742335d8b3d9a7a15773907d1b0c7d948ece274e88fa17bd77176ad52638` |
| Q27 `q27-v0.14.5-linux-x86_64.tar.gz` | `409422648` / `629808670` | 27647648 | `ae86e782264b8f578d224c2fee3defc440cb1bda402059f7cfd2e17038ca0de5` |
| NInfer `ninfer-windows-0.9.1-win64-cuda131.zip` | `398720301` / `596739681` | 450215373 | `d05f9df73feb43b47de23cb09ee9b9e79b040afb8d73ae30a0f4c93dea379060` |

These four archives were downloaded and their bytes hashed; extraction/entrypoint
layout was inspected. Entrypoint SHA-256:

| Entrypoint | SHA-256 |
| --- | --- |
| `llama-b11429/llama-server` | `5faf410bbbc00a1b94b09ee01623699f255365628dfa73b1ee552d9e52a2231d` |
| `llama-b11565/llama-server` | `624ae25af57decf0b92b59ba6242e23a6f60b61f7de6fa03da4428b599dd23d4` |
| `q27-v0.14.5/server` (W12) | `b8f578cc4e00450559a4b925811334861d310900f287df06c977a9fa0adfdb21` |
| `q27-v0.14.5/server-w8` | `48871deb0650ce6c116831f3d18b168925af134d1836d5d826a29589500a302d` |
| `q27-v0.14.5/server-w16` | `1a91796e5160ce518542ce8ab4ecbb0b49b3b874b2f52b5a126dbda2dba6561e` |
| `q27-v0.14.5/server-12g` | `7418c7984d6265d49cfbd6b0207e6709bd006a1101269105d7130f9a11830996` |
| `ninfer-windows-0.9.1-win64-cuda131/ninfer-serve.exe` | `1b5f99b08a9569fc9a92014b09be1d1e9a828666d5cd4a006880c48572944b61` |

Official b11565 CUDA asset metadata was reviewed; installers still verify actual
asset bytes and companion closure. These larger CUDA archives were not executed
or downloaded for validation:

| Asset / ID | Bytes | Published SHA-256 |
| --- | ---: | --- |
| Ubuntu CUDA 12.8 x64 / `633278863` | 171559844 | `61b8cf4e49739afe4008b06a92390dff999e706e9e7e9749c015544a9f241627` |
| Matching CUDA 12.8 cudart / `633272940` | 594377834 | `e6177074761d8be20b76ce165944f16b7a338c0b477d1ecbb240e38c743c1b90` |
| Ubuntu CUDA 13.4 x64 / `633279531` | 152410875 | `7ae73b7a4d0abd299c3c5fc0101cbb41fc71a88ef7625b48880d3d9d7811aac6` |
| Matching CUDA 13.4 cudart / `633275578` | 440236731 | `a4b21c7ccfb362f0f318e17193f9afda01030498e03f08c400ae3c8e5f97dd3a` |

Preserved Windows v0.7.1 archive SHA-256:
`d9bb7b4c4f5ded7de9f42ba25635a64d9d4f3412310701ff37f66743e8e940b5`.
Existing release/catalog identities remain independent; application updates do not
replace runtime selection.

## Preserved behavior and new API behavior

Existing native System One owners, named questions, noul/choice/score, immutable
provenance, source-native engine, Model Profile routing, JIT admission, leases and
Link owner authority are preserved. No decisions-through-chat fallback was added.
Candidate metadata remains distinct from exact loaded-pair execution proof.

`POST /v1/decisions` implements the published OpenAI ordered request/answer shape
through bounded conversion to that existing executor. It retains omitted/null
names, boolean/string values, descriptions, score labels and original question
order. It requires actual native model identity, predicate P(true), normalized
native distributions/confidence and compatible score observations. Refusal is a
native typed outcome, not inferred from abstention. Missing required observations
fail with an actionable structured error. Complete native usage is required and never synthesized. Every successful
response includes all published usage counters; incomplete observations yield
HTTP 502 `native_decision_contract_mismatch`, including successful responses from
older Link owners. Hosted model names or limits are not Scala defaults.

The current bounds are 32 MiB public body, 1–1024 questions, 2–255 choices/levels
before native limits, and llama.cpp score levels 2–10. User messages only; text and
qualified native images only. Images are at most two 8 MiB PNG/JPEG/WebP payloads,
4096x4096 each, bounded full decoding. HTTP(S) acquisition excludes local/private
addresses, credentials, redirects and proxies, pins validated DNS results and
bounds ports, bytes and time. Ordered media and original bytes reach native System One. Image capability
requires native vision observations and a bound projector. At both frozen llama
revisions, embedded images are extracted into a shared list: original inline
text/image positions and message associations do not reach rendering. Text-only
messages, a sole image-only message and explicit shared System One `images` remain
admitted. Embedded text/image mixtures and images in multiple messages fail
field-specifically before inference; labelled comparisons cannot silently lose
their associations. No Scala prompt markers or chat emulation are added.

OpenAI Decisions has its own output-contract qualification. Both frozen llama.cpp
revisions provide only native input/output usage counts, with no cache,
cache-write or reasoning counts. They retain candidate, native Decision and proven
vision capability but advertise `openai_decisions: false` (false flags are omitted on the wire). Their OpenAI route
returns HTTP 400 `unsupported_capability` before inference, even after native
Decision qualification. Synthetic complete-usage protocol tests prove the
schema-complete success path without granting that contract to a production
llama.cpp pair. Merely exposing System One or recognizing an upstream revision
grants neither OpenAI compatibility nor image capability.
See [native Decision documentation](native-decision.md) for details.

Qualification binds runtime/executable/probe, artifact/native identity, profile,
resolved settings, auxiliary identity and physical fingerprints. Changed evidence
invalidates it. Discovery, CLI/TUI inspection, Model Profile metadata, JIT follow-up
and owner-provided Link aliases expose candidate/verified states separately. Link
uses a backwards-compatible read-only capability inventory extension, retaining
old inventory/control shapes for older peers. No federation-specific inference.

Decision activity distinguishes admission/loading, ProcessingDecision and bounded
Completed/Failed/Cancelled terminals without reporting invented text tokens.
Disconnect/timeout/unload/shutdown cancel the corresponding native HTTP future,
release its lease and use upstream request-stop handling. Cancellation is scoped
per request/owned JIT load; another concurrent request continues. Recent terminal
observations are limited to 16 with 30-second retention. No public protocol was
invented. No prompt/question/image/credential content is logged as diagnostics.

## Engine integration

### llama.cpp

Official stable-pointer and nightly acquisition resolve exact Git source identity;
verified main/companion assets and existing side-by-side installation remain in
place. Actual stable/nightly CPU help fixtures drive full native-option inventory
classification. New typed controls are `llama.cpp.moe_cache_mib` and
`llama.cpp.draft_sampling`; unknown headers fail closed. Explicit projector path
and SHA-256 bind native `--mmproj`. New native architectures/modalities are adopted
through the qualified executable, not Scala kernels or model-name classifiers.

Managed CUDA recipes remain `managed-portable-v5` (CUDA 12.8 to below 13) and
`managed-portable-cuda13-v3` (13 to below 14), with reviewed CCCL guard handling.
Upstream retains CUDA/Metal/Vulkan/GGML, scheduling, cache and speculative
implementation. Old runtime identities/settings remain usable. Native System One
media routes and cancellation were source-reviewed at both frozen revisions;
actual model/image execution was not run.

The audit correction re-reviewed the exact native usage owner and image
parser/rendering together:

| Revision | Native response owner | Image parser/rendering owner |
| --- | --- | --- |
| v0.6.0 / b11429 | [server-context.cpp, post_systemone](https://github.com/ggml-org/llama.cpp/blob/d81235049384534c167caea52b85a694f6103d14/tools/server/server-context.cpp#L5573-L5666) | [server-decision.cpp, parse_state/render/fill_task_joint](https://github.com/ggml-org/llama.cpp/blob/d81235049384534c167caea52b85a694f6103d14/tools/server/server-decision.cpp#L225-L272) |
| b11565 | [server-context.cpp, post_systemone](https://github.com/ggml-org/llama.cpp/blob/f9a95aeb2096d0c0218e0cd6e6d901cd37be798a/tools/server/server-context.cpp#L5760-L5844) | [server-decision.cpp, parse_state/render/fill_task_joint](https://github.com/ggml-org/llama.cpp/blob/f9a95aeb2096d0c0218e0cd6e6d901cd37be798a/tools/server/server-decision.cpp#L256-L312) |

The native output includes an observed `output_tokens: 0`; that observation does
not establish zeros for the absent detailed counters. Rendering creates a
separate marker list from the media count and passes the stripped state to the
model's native template. Retained media order therefore does not establish
retained inline positions or per-message associations. The imported `--help`
fixtures remain exact upstream captures, including upstream trailing whitespace.

### Q27 v0.14.5

Exact release and independent `q27-upstream-make-v4-2945e35` source admission were
added while preserving v0.14.3 and earlier contracts. Actual package ELF symbols
require GLIBC 2.38 and GLIBCXX 3.4.32; native binaries use static CUDA 13.2 and
upstream documents r580+. The reviewed Make recipe requires CUDA 13.2 for PF4,
even though generic upstream documentation mentions 12.8+. Main serving images
contain sm86/sm89/sm120; server-12g remains a distinct sm86/W_MAX=8/prefill-256
Bonsai 2 slim T2/T3 route. Scala does not borrow a different Docker recipe's proof.

Exact source capability-owner SHA-256:

| Owner | SHA-256 |
| --- | --- |
| `Makefile` | `2770e46694572d1e6cba843db06bea4700c186d7ff55a4d1083805e920bc8c14` |
| `README.md` | `09f0fe58a89c877c760d04fc5420a6f12fa0c87bbb7b69317cbb122418768904` |
| `src/server.cu` | `2c3ccdd924b8a1e374142cfa0d47056536cd040459b1c30eed62ae9164d2334f` |
| `src/engine.cuh` | `43246288d69462e41c4676f7a674cff1da9ea9ef08ecbb7a4c2e92486fd5acc4` |

Typed `q27.draft_vocab` / `q27.draft_vocab_context` are explicit opt-in, requiring
solo non-continuously-batched serving, no DFlash2, actual NextN projection and
supported packed-head inventory, and matching native initialization evidence.
Unverifiable prerequisites produce a specific unsupported reason, not guessed
metadata. Dynamic `Q27_*` scrubbing remains intact. Native prefix cache, sampler,
tool parser, MTP and CUDA optimization remain executable-owned. Ignored stop,
previous_response_id, response_format, logprobs and penalty guarantees are gated;
strict tools lack proof and remain unavailable. Native MTP-off is not promised.
Invalid token budgets, n>1 and unsupported images remain deterministic errors.

### NInfer v3 Linux and native Windows

The Linux exact-source recipe `ninfer-serve-v3` targets Linux x64/sm_120a with
CUDA 13.1, CMake 3.28, Ninja and C++20. The current recipe expects
`/usr/local/cuda/bin/nvcc`. The source requires libcurl 7.85; the managed FFmpeg
contract checks compatible library generations. Exact build-definition SHA-256:
`e6878298480e49528bbeedeaeb3d53f645b6d8052862adb14f6271df8be6ecbc`.
All 196 recorded source-owner hashes were checked against the frozen snapshot
with zero mismatches. The independent 196-owner SHA-256 closure is tracked in
`crates/scala-engine-ninfer/src/v3/owners.rs`. It covers the restructured successor
owners rather than crediting old v2 capability blobs.

Windows v0.9.1 is a native Windows 11 x64 portable CUDA 13.1/RTX 5090 package,
with bundled runtime/media/MSVC DLLs and its own immutable package/executable
identity. It uses schema 21; Linux uses schema 25; old v2 uses schema 20. The
portable route needs a compatible driver, not a local CUDA toolkit or WSL.
Windows v3 and Linux v3 share the reviewed container generation, but their server
capabilities are not interchangeable. V2 and v3 remain separately selectable;
wrong-generation pairs fail even with empty Settings or similar architecture IDs.

The cold v3 inspector validates native header, unique bounded directory, object
ranges/alignment/packing, part IDs and sizes, architecture and quantized bindings,
frontend resources and selected vision/speculative component inventories. Bare
artifacts remain first-class. Local import preserves continuation parts. Neither
Norted manifests nor filenames grant extra capability. No automatic conversion.
Changed settings/log schemas are independently admitted; conditional defaults
remain unknown until observed. Schema 25 uses a single native host-context budget.

Linux v3 constraints are native JSON object/JSON Schema via Chat response_format
and Responses text.format, GBNF/choice/regex via structured_outputs, and strict
function-tool arguments. Ordinary/MTP/DFlash/DFlash2 streaming and nonstreaming
requests use native constraints. The admitted assertion subset and dialects are
listed in [NInfer documentation](ninfer.md#native-constrained-output-on-qualified-linux-v3).
Unsupported assertions/references fail field-specifically; native malformed,
unsupported, unsatisfiable, grammar/regex/choice/tool and dead-end error categories
remain distinct. Actual native completion evidence is required; missing proof or
incomplete output cannot masquerade as a successful strict result. No Scala
sampler/compiler was added.

## Blocking and material limitations

- **Windows NInfer v0.9.1 constrained output:** verified native incompatibility.
  The pinned [serving contract](https://github.com/natpate/ninfer-windows/blob/fe115cdecd16acc96df72206053e94f5c8f25214/docs/serving.md)
  and [request parser](https://github.com/natpate/ninfer-windows/blob/fe115cdecd16acc96df72206053e94f5c8f25214/src/serve/openai_chat_request.cpp)
  reject absent response-format/structured-output/strict-tool contracts (notably
  parser lines 134–143, 205–220 and 625–633). Scala withholds those capabilities;
  Linux schema-25 source proof cannot grant them to this schema-21 package.
- **Native observations:** the OpenAI route cannot represent Lev's nine-level noul
  expectation as predicate P(true), nor invent required distributions/confidence,
  refusal, native identity or complete usage. Those particular unsupported pairs
  or incomplete outputs fail explicitly; existing System One semantics survive.
  Neither frozen llama.cpp revision meets mandatory complete usage, so both
  withhold OpenAI Decisions capability while retaining independent native Decision
  and proven vision. Their native shared media list cannot express embedded
  text/image positions or per-message associations; Scala rejects those forms
  explicitly and preserves accurately representable shared/image-only input.
- **Validation scope:** all new native integration is source/package/static and
  synthetic qualified, without live-model or production-GPU validation. Actual
  host/probe/pair qualification remains mandatory. This is not a claim that every
  Decision artifact supports vision or every runtime supports identical schemas.

No remaining implementation blocker was encountered for the admitted contracts.

## Authoritative local validation

All commands completed successfully, with no warnings from check/Clippy/tests:

| Command | Result |
| --- | --- |
| `cargo fmt --check` | passed |
| `cargo check --workspace` | passed |
| `cargo clippy --workspace` | passed |
| `cargo test --workspace` | passed: 416 tests, 0 failures, 1 existing ignored test; 22 result suites including doc tests |
| `git diff --check` | passed |

Tests cover both Decision APIs and conversion/output errors, boolean identity,
omitted names, refusal, missing/complete required usage and Link success validation,
ordered raster bytes versus native position/association limits, image-only/text-only,
shared single/multiple images and labelled interleaving across user messages;
SSRF bounds; candidate/pair
qualification invalidation; public disconnect/native cancellation and concurrent
lease cleanup; JIT/unload/restart; Link owner routing/capability inventories;
actual llama help/options and asset companions; Q27 source/version/packed-head
checks and old gates; v2/v3 native container framing/bindings/resources, Linux and
Windows runtime integrity/startup schemas; native constraints, exact numeric
assertions, error categories and streaming/incomplete completion; historical
runtime/profile, Inherit and per-engine/request override regressions.
No GitHub Actions, production GPU service or model inference was used.

## Files changed

- `Cargo.lock`
- `crates/scala-api/Cargo.toml`
- `crates/scala-api/src/chat.rs`
- `crates/scala-api/src/chat/unsloth_contract.rs`
- `crates/scala-api/src/constraint_numbers.rs`
- `crates/scala-api/src/decisions.rs`
- `crates/scala-api/src/decisions/images.rs`
- `crates/scala-api/src/error.rs`
- `crates/scala-api/src/input.rs`
- `crates/scala-api/src/lib.rs`
- `crates/scala-api/src/link.rs`
- `crates/scala-api/src/responses.rs`
- `crates/scala-api/src/systemone.rs`
- `crates/scala-core/src/model.rs`
- `crates/scala-core/src/ninfer_v3.rs`
- `crates/scala-core/testdata/ninfer-v3.json`
- `crates/scala-engine-llama-cpp/src/catalog.rs`
- `crates/scala-engine-llama-cpp/src/decision.rs`
- `crates/scala-engine-llama-cpp/src/lib.rs`
- `crates/scala-engine-llama-cpp/src/source_catalog.rs`
- `crates/scala-engine-llama-cpp/testdata/b11429-server-help.txt`
- `crates/scala-engine-llama-cpp/testdata/b11565-server-help.txt`
- `crates/scala-engine-native-decision/src/lib.rs`
- `crates/scala-engine-native-decision/src/tests.rs`
- `crates/scala-engine-ninfer/src/catalog.rs`
- `crates/scala-engine-ninfer/src/lib.rs`
- `crates/scala-engine-ninfer/src/protocol.rs`
- `crates/scala-engine-ninfer/src/settings.rs`
- `crates/scala-engine-ninfer/src/v3.rs`
- `crates/scala-engine-ninfer/src/v3/owners.rs`
- `crates/scala-engine-ninfer/src/windows_catalog.rs`
- `crates/scala-engine-q27/src/lib.rs`
- `crates/scala-engine-q27/src/model.rs`
- `crates/scala-engine/Cargo.toml`
- `crates/scala-engine/src/benchmark/retrieval.rs`
- `crates/scala-engine/src/benchmark/suite.rs`
- `crates/scala-engine/src/control.rs`
- `crates/scala-engine/src/decision.rs`
- `crates/scala-engine/src/lib.rs`
- `crates/scala-engine/src/link.rs`
- `crates/scala-engine/src/manager.rs`
- `crates/scala-engine/src/packs.rs`
- `crates/scala-model-library/src/lib.rs`
- `crates/scala-tui/src/federation.rs`
- `crates/scala-tui/src/lib.rs`
- `crates/scala-tui/src/ui/screens.rs`
- `crates/scala/src/main.rs`
- `crates/scala/src/output.rs`
- `docs/integration-october-2026.md`
- `docs/native-decision.md`
- `docs/ninfer.md`
- `docs/q27-grep-contract.md`
- `docs/runtime-settings-coverage.md`


## Native multimodal embeddings completion

This completion started from clean `integration/october-2026-inference` at
`6ca9372b8ee5f7ccf87ff6e9e97603fcff5e4cd8`. It preserves all prior October
Decision, llama.cpp, q27 v0.14.5, NInfer v3 and constrained-output integrations.
No model, production Settings/Profile or live Scala service was changed.

Contracts were checked at both frozen llama.cpp revisions, including
[stable server README](https://github.com/ggml-org/llama.cpp/blob/d81235049384534c167caea52b85a694f6103d14/tools/server/README.md#post-v1embeddings-openai-compatible-embeddings-api)
and [nightly server README](https://github.com/ggml-org/llama.cpp/blob/f9a95aeb2096d0c0218e0cd6e6d901cd37be798a/tools/server/README.md#post-v1embeddings-openai-compatible-embeddings-api),
`server-context.cpp::handle_embeddings_impl`, `server-common.cpp`
(`tokenize_oai_content_array`, `oaicompat_content_load_media`, `handle_media`,
`format_embeddings_response_oaicompat`), `server-task.cpp` embedding results,
and `tools/mtmd` (`mtmd.cpp`, `clip.cpp`, `mtmd-helper.cpp`, CMakeLists).
Both accept a single content object and ordered string/object mixtures as well
as the documented object array. Each object schedules one embedding task;
content parts are processed in supplied order without a chat template. This is
a llama.cpp extension, not standard OpenAI Embeddings API functionality.

The public contracts, examples and limits are in the [README](../README.md#secure-serving-and-openai-compatible-apis).
All four native part types are implemented now: text, image_url, input_audio,
input_video. Image URL uses `image_url.url`; audio/video accept exactly one
`data` or `url` with raw base64, data URI or safely acquired HTTP(S). Native
ignores audio `format`; Scala accepts only omitted/`wav`, matching the bytes.
Video has no format field. Other/ambiguous fields, unsupported token arrays,
empty inputs/groups and unknown part types fail before native dispatch.

Qualification distinguishes pooled-model candidates from loaded proof. It
uses the exact immutable runtime, real model architecture/pooling metadata,
existing SHA-256 projector/auxiliary binding, native encoder metadata,
matching `/props` and `/v1/models`, and an empty-input validation-only probe
that never dispatches inference. Images, audio and video qualify independently.
FFmpeg/FFprobe identities must also be available on native PATH for video and
WebP; compile-time video capability alone is insufficient. Proofs are ephemeral,
file-sensitive and removed with backend lifecycle state. No capability derives
from GGUF alone, a vision filename, chat capability, producer origin, or another
Engine. Existing text-only runtime/model combinations remain usable.

The native audio reader recognizes WAV/MP3/FLAC and allocates from decoded frame
length; the video reader launches FFprobe/FFmpeg on supplied bytes without a
protocol allowlist. Scala therefore admits complete integer PCM WAV and a
fully inspected, self-contained uncompressed BGR AVI subset. The small synthetic
AVI is also decoded by local libavformat/libavcodec as two 2x2 rawvideo frames;
the WAV decodes as four PCM samples. Compressed audio, MP4/WebM, playlists,
opaque AVI extensions/indexes, secondary streams and external container
references are explicitly unsupported by this bounded transport. These are
intentional security limits, not deferred modality implementations. Native
context/batch/token limits still apply; no pooling/normalization/preprocessing
or launch defaults were changed, and no transcoding or fabricated usage/vector
fallback was introduced.

Media reuses shared Decision raster validation/acquisition, with 32 MiB request,
two 8 MiB media, 15-second per-file / 30-second request limits including DNS,
strict base64/MIME and decoded geometry/duration bounds, no credentials/files,
no private/metadata/overlay hosts, public-only pinned DNS, no proxies and no
redirects. Acquired bytes become inline data so native cannot fetch the URL.
Responses retain exact native usage or its absence and validate vector
count/order/index/dimension/finiteness. Errors cannot echo media or credentials.
Scala Link uses the existing bounded owner request/response wire; artifacts
stay on their owner. Settings precedence and persisted contracts stay intact.

Synthetic coverage includes both frozen revisions, all public input forms and
modalities, mixed ordered parts/inputs, invalid shapes/media/size/decoded bounds,
private URLs, DNS rebinding/redirect resistance, missing or corrupt projectors,
independent modality evidence, wrong loaded identities/pooling, resource changes,
text-only compatibility, public float/base64/usage, cold Profile JIT and unload,
Link request/response forwarding, and explicit q27/NInfer embedding boundaries.


Files changed for this completion:

| Area | Files |
| --- | --- |
| Public API and shared acquisition | `crates/scala-api/src/embeddings.rs`, `media.rs` (new), `decisions/images.rs`, `lib.rs`, `link.rs` |
| Engine contract, validation and routing | `crates/scala-engine/src/embeddings.rs` (new), `media.rs` (new), `decision.rs`, `lib.rs`, `manager.rs` |
| GGUF encoder inspection | `crates/scala-core/src/model.rs` |
| Native adapter and synthetic backend tests | `crates/scala-engine-llama-cpp/src/embeddings.rs` (new), `embeddings/tests.rs` (new), `lib.rs`, `decision.rs`, `Cargo.toml` |
| Engine boundary tests only | `crates/scala-engine-q27/src/lib.rs`, `crates/scala-engine-ninfer/src/lib.rs` |
| Tiny byte fixtures | `crates/scala-engine/testdata/embedding.wav` (52 bytes), `embedding.avi` (272 bytes) |
| Documentation and test dependency lock | `README.md`, `docs/architecture.md`, `docs/integration-october-2026.md`, `Cargo.lock` |

Blocking findings: none for the admitted contracts. Material limits are the
explicit WAV/AVI safe subsets, exact reviewed-runtime and loaded-pair proof,
projector/encoder integrity, and native FFmpeg/FFprobe availability for video
and WebP. Actual model execution remains native and was not run during development.


Completion validation (all successful, no warnings): `cargo fmt --check`,
`cargo check --workspace`, `cargo clippy --workspace`, `cargo test --workspace`
(**432 passed, 0 failed, 1 existing ignored test, 22 suites including doc tests**),
and `git diff --check`. The tiny WAV/AVI fixtures also passed the local
libavformat/libavcodec format/decode check. No GitHub Actions, model inference,
GPU work, conversion, training, quantization, benchmarks, deployment, push,
merge or release was performed.
