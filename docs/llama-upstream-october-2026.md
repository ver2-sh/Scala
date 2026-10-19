# Official llama.cpp compatibility review, 2026-10-19

Reviewed from clean Scala `master` / `origin/master` at
`6722fdfc4f723dee84b70d5b70ec6f93b91c115b`. This extends the earlier
[integration ledger](integration-october-2026.md) and [Decision contract](decisions.md).
LM Studio 2.50.0 and Bionic extension-pack 2.53.0 are vendor identifiers;
neither identifies or qualifies an official upstream source/package pair.

## Frozen upstream evidence

| Official release | Commit | Tree |
| --- | --- | --- |
| [v0.7.0](https://github.com/ggml-org/llama.cpp/releases/tag/v0.7.0), published October 15; pointer to b11669 | `620d2225cc436ab0b001e3200e590e3b286ef9ab` | `6868e0a538a05e9a253a0b86ccb71b4e06a64684` |
| [b11739](https://github.com/ggml-org/llama.cpp/releases/tag/b11739), newest published nightly at the October 19 review cutoff (17:57:49 UTC) | `0ce8cd11416c8840498e6701bcbe39ad4b858c2b` | `2a72ec734bd91f312f22d2e1e45de5b2a679186d` |

The annotated v0.7.0 tag object is `757cad021b357f417065ddbc41a6e8f87c1f8136`.
Its seven-byte `nightly-tag.txt` is `b11669\n`, SHA-256
`0bbf7c261e41e777ad8d70f2c83f8d7dd2ae4d7e41a9b6f26926fbc902209c70`.
The existing provider verifies this digest and equality of the stable/nightly
commit and tree. Both executable builds report `0.7.0-dev`, with their separate
build numbers and abbreviated commits; that version string grants no capability.

Downloaded archives matched their GitHub-published SHA-256 values:

| Asset | SHA-256 |
| --- | --- |
| llama-b11669-bin-win-cpu-x64.zip | `37b3efcf962847ca869745fd4ccf7f5e7e8dacfeff9fa3523b4ca8cdc66d632b` |
| llama-b11739-bin-win-cpu-x64.zip | `b1e0a14abdb4665c1b821b377ba063b221cec62882c373065cf8fb672988d3ff` |
| llama-b11669-bin-ubuntu-x64.tar.gz | `cf07c1c112d709bd67e522017471de57ed0ec8bea031d732c5422351c07f65ac` |
| llama-b11739-bin-ubuntu-x64.tar.gz | `0b3b7579835cc3d837c6046bbc970fd4be0281d1a9ee2ea5b7cdb6eeb84b5842` |

Windows server executable hashes, also recorded by Scala's installer:

- b11669: `6a4635aeb35656153d4bfeeddf761d24ed84aada9c9108d580737cbed465141b`.
- b11739: `8675d6877d6ba049cecd1abf4c1663b7b85d40f5cdccf1e98aa63a141545ebce`.

The `testdata/b11669-release.json` and `b11739-release.json` fixtures under
`crates/scala-engine-llama-cpp` retain the exact release/asset fields consumed by
the provider, including asset IDs, URLs, sizes, digests and upload states.
Their Windows CPU `--help` captures are byte-identical, SHA-256
`d50dc14f37b93e90d6c8dba2a371c666cf5aa53cbd457dd5c641f4a5cd99f47b` (CRLF).
Independently executed Linux CPU help captures are also identical and equal the
Windows text after newline normalization, SHA-256
`9c15905aca31fd975b183f00f4dbc9c79b46b2b61df7247682d2cc2738feea8d` (LF).

## Acquisition, dependencies and launch

Each release yields 14 currently supported official variants through the existing
provider. CUDA packages still require the uploaded, digest-bearing companion from
the same release and exact CUDA version/architecture. CUDA 12.4/13.4 Windows and
12.8/13.4 Linux names fit the existing classifier. No new backend/platform support
or CUDA host qualification is inferred from the asset list.

A concrete catalog bug discarded a verified semantic reference: `v0.7.0` resolved
to b11669, then failed a redundant textual search against the canonical identity.
Exact-reference results now retain the provider's resolution after its existing
authority validation; broad searches retain their normal text filtering. No
runtime identity, cache schema or selection format changes.

The normal `RuntimePackManager` discovered, downloaded, verified and installed
both Windows CPU packages in a detached temporary store. Their immutable IDs are
`llama-cpp-b11669-windows-x86-64-cpu-default-e155258a50b0c022` and
`llama-cpp-b11739-windows-x86-64-cpu-default-6dfd2378ce424f3e`. Installing the second
preserved the first pinned selection; explicit selection of each worked. Adapter
probes and launch-spec construction passed with a synthetic artifact. Scala emits
the private loopback host/port explicitly, so upstream's new default port 9931
does not alter routing. No model-bearing process was started.

Windows PE imports retain `llama-server-impl.dll`, `llama-common.dll`, `mtmd.dll`,
llama/GGML libraries and the packaged OpenMP runtime. External dependencies remain
Windows system/UCRT and MSVC runtime DLLs (`MSVCP140`, `VCRUNTIME140`,
`VCRUNTIME140_1`); their import sets are unchanged between the reviewed packages.
Both Linux CPU packages ran version/help under local Ubuntu WSL. `ldd` resolved
their packaged server/common/mtmd/llama/GGML libraries and host C/C++/OpenMP,
OpenSSL 3, zlib and zstd libraries without missing dependencies. This is evidence
for this host, not a claim about every host or GPU backend.

Both exact revisions passed the existing managed-source provider's CMake
admission. `managed-portable-v5` and `managed-portable-cuda13-v3` remain unchanged:
exact commit/tree checkout, explicit architecture policy, NCCL disabled, CCCL
download guards disabled, and UI/tools build policy preserved. No source or GPU
build was performed. Installed runtimes and selections outside the temporary store
were never modified.

## Native Decision and vision

The immutable source owners are
[stable server-decision.cpp](https://github.com/ggml-org/llama.cpp/blob/620d2225cc436ab0b001e3200e590e3b286ef9ab/tools/server/server-decision.cpp),
[nightly server-decision.cpp](https://github.com/ggml-org/llama.cpp/blob/0ce8cd11416c8840498e6701bcbe39ad4b858c2b/tools/server/server-decision.cpp),
[native type recognition](https://github.com/ggml-org/llama.cpp/blob/0ce8cd11416c8840498e6701bcbe39ad4b858c2b/common/common.cpp),
[image admission](https://github.com/ggml-org/llama.cpp/blob/0ce8cd11416c8840498e6701bcbe39ad4b858c2b/tools/server/server-decision.h),
[route/output owner](https://github.com/ggml-org/llama.cpp/blob/0ce8cd11416c8840498e6701bcbe39ad4b858c2b/tools/server/server-context.cpp),
and [projector loader](https://github.com/ggml-org/llama.cpp/blob/0ce8cd11416c8840498e6701bcbe39ad4b858c2b/tools/mtmd/mtmd.cpp).

Stable includes `vllm-decision2`, `vllm-d3`, `lfm2-d1`, `lfm2-d1-omni` and
`pplx-decider`. The nightly adds `granite-switch` to the existing label-based
execution path. Native architecture-scoped GGUF metadata admits all as candidates
without adding a Scala name classifier. `granite-switch` can report both `text`
and `decisions`; extra modalities do not substitute for independent qualification
or change Scala's existing Decision-only profile behavior.

The six `can_use_images()` cases remain `openjev`, `clef`, `pplx-decider`,
`vllm-d3`, `lfm2-d1`, `lfm2-d1-omni`. Those exact source contracts are now admitted
at the two frozen commits, alongside the previously reviewed revisions. Names,
later build numbers and vendor releases cannot extend that gate. Native startup
must still validate the projector, report vision, and prove the exact loaded
runtime/artifact/profile/settings tuple through props, model identity/modalities
and the empty System One validator. Missing `state` still returns the exact
`400 invalid_request_error`, `"state" must be provided`, before task creation.

Parser limits remain native-owned: score levels 2–10, model-dependent choice
limits, optional d3 instructions, and null state accepted by d3/LFM variants.
Scala retains its existing public structural contract and native empty-instruction
translation. D3 and D1-omni evaluate the whole prompt in one micro-batch; D1-omni
truncates to 16384 tokens and uses uncalibrated media probabilities. These are
upstream behaviors, not Scala defaults or benchmark recommendations.

`parse_state` still strips image parts into a shared ordered media list; rendering
places native template markers independently. Existing byte/order admission and
rejection of embedded text/image positioning or multi-message associations remain
unchanged. Upstream D1-omni audio and the `files` alias do not expand Scala's
image-only public Decision transport. There is no audio/video admission, prompt
approximation, generated answer, or synthetic refusal.

Successful native System One responses have exactly these observations:

| Field | Native semantics |
| --- | --- |
| `model` | `meta->model_name`, preserved as reported |
| `answers[name]` choice | `type`, winning `choice`, native `probabilities`, native `confidence` |
| `answers[name]` score | `type`, native expected `score`, `legend`, `probabilities`, `confidence` |
| `answers[name]` noul | `type`, `noul`; no probability map/confidence; Lev remains a nine-level expectation |
| `usage.input_tokens` | Native task count, summed across question variants for non-joint models |
| `usage.output_tokens` | Explicit native zero |

There is no native total, cached/cache-write count or reasoning count. Scala's
checked System One total is not an independently observed OpenAI usage total.
The stable and nightly System One handlers are identical; upstream registers
`/v1/systemone` and adds no `/v1/decisions` route at either frozen revision.
The adapter continues to advertise no `openai_decisions` support and returns
`unsupported_capability` before inference for `/v1/decisions`. No field is filled
from defaults, startup observations or another request.

## Settings and validation

The only new option header relative to the b11565 inventory is
`--spec-draft-n-ubatch`. The typed `llama.cpp.draft_micro_batch_size` setting maps
to it and reserves `LLAMA_ARG_SPEC_DRAFT_N_UBATCH`. Native values are 0 through
INT32_MAX; zero is the explicit runtime policy of using the main micro-batch size.
Omission emits no argument. The setting is unsupported on old help contracts and
has no invented pre-probe default. Raw argument/environment collision checks and
runtime → Settings → Model Profile → invocation precedence remain unchanged.

New help also advertises a contextual `--cache-ram` default: Decision models
disable the prompt cache and idle-slot caching unless cache RAM is explicit.
Both previews now retain this native policy instead of displaying one universal
scalar. Explicit zero/false and ordinary old-runtime defaults remain intact.
All other headers retain the existing typed/Scala-managed/unsupported ownership;
unclassified future options still reject runtime admission.

Local validation uses frozen help/release fixtures and synthetic HTTP/artifacts.
The native llama.cpp suite includes legacy qualification, changed physical
identities, projector binding, missing/contradictory observations, optional wire
fields, media order/position limits, cold JIT/unload/restart, and refusal of the
incomplete OpenAI contract. Two existing Windows fixture path mismatches also
reproduced on untouched master; canonicalizing only their synthetic artifact
paths makes them match the library's physical identities.

Final local checks passed (119 targeted tests):

- `cargo test -p scala-engine-llama-cpp --lib --locked`: 70.
- `cargo test -p scala-engine --lib --locked catalog::tests`: 14.
- `cargo test -p scala-engine --lib --locked manager::tests`: 27.
- `cargo test -p scala-engine --lib --locked profile_runtime_pins_are_independent_validated_and_protected`: 1.
- `cargo test -p scala-engine --lib --locked verified_binary_install_uses_one_live_release_lookup_and_direct_download`: 1.
- `cargo test -p scala-api --lib --locked decisions::tests`: 6.
- `cargo check --workspace --all-targets --locked`, `cargo fmt --all --check`
  and `git diff --check`.

The broader `scala-engine --lib` run has eight additional failures on this Windows
host, all reproduced on untouched master: the source dependency audit's
case-sensitive Makefile assumption, three pack observation/platform fixtures,
and four Python-dependent Windows process-tree tests. They remain outside this
compatibility update. Existing platform-specific compiler warnings remain.

Scratch downloads, source trees, package manifests, dependency reports and the
one-off manager audit are under `%TEMP%\scala-llama-20261019`. This review ran no
inference, training, benchmarks, hosted validation or GPU builds, and changed no
production profiles, runtime selections, service or published contract.
