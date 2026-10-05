# Native NInfer, MTP and DFlash2

The current reviewed runtime is clean upstream `Neroued/ninfer` commit
`d49296868dcc17bd478ec185f0d3a801bcc0bf56`, tree
`8e2f0275fc533cf11fe05a4ac3ac85f00eb91c72`. Source owners cover CLI parsing,
process defaults, scheduler/cache/state, native target bindings, masked-draft
configuration/implementation, sparse proposal acceptance, Vision and schema-20
startup/request evidence. Runtime code and templates are not patched or emulated.

For native Windows, Scala additionally manages the reviewed portable package
published by `natpate/ninfer-windows` `v0.7.1` (Windows x86_64, CUDA, RTX
5090/sm_120a; no WSL). That port remains in the v2 generation and includes
DFlash2. Its repository, release tag, asset and digest identities are retained
as its own provenance; it is not relabeled as canonical `Neroued/ninfer`, whose
managed route stays Linux source-only. The managed catalog exposes only that
exact reviewed package: the release ID, the tag's resolved commit/tree, and
the asset ID, name, size and SHA-256 digest are all pinned, so a replaced
asset or a moved tag makes the package unavailable rather than inheriting
review. Capability credit is evidence-bound per domain: at the reviewed tag,
74 of the 97 capability-owning source blobs are byte-identical to the
canonical reviewed set and the 23 differing fork blobs were reviewed in place
as contract-preserving, so the package receives the same capability domains
only while the exact pinned package facts hold. Upstream `v0.8.x`/`v0.9.x`
portable releases moved to NInfer v3 artifacts and remain unavailable while
Scala admits v2 containers only, and other `v0.7.x` tags are unreviewed and
never candidates.

Native target registry inspection covers Qwen3.6-27B and Qwen3.8-27B with
`groupwise-int` and `nvfp4`, plus Qwen3.6-35B-A3B `groupwise-int`. The reviewed
27B binding/variant supports DFlash2 when its complete native tensor bundle is
present. Old DFlash remains the separate 35B-A3B backend.

Settings use `Runtime → Settings → Profile → Load → request` precedence where the
native runtime supports request-time controls. Runtime defaults are read-only.
Speculation is a startup choice: `mtp` with 1–5 draft tokens or `dflash` / `dflash2`
with 1–15. A single choice enforces exclusivity. The optional optimized proposal
head is `ninfer.lm_head_draft`; full head remains native behavior unless overridden.
No benchmark preset becomes a production default.

Bounded native directory inspection checks all 66 required DFlash2 tensor names,
shapes, numeric formats, layouts, encoded sizes and alignment, after generic object
framing/range checks. Missing, partial, conflicting or malformed suffixes cannot
advertise or accept DFlash2. The native runtime remains the final artifact-validation
authority. A complete base artifact without a draft remains a legitimate format.
Scala, Swift, OrcaRouter, official and third-party origins follow identical rules.
A native ID is a dispatch/format identity, not a public alias, source lineage or
proof of draft presence.

Norted package schema 7 admits a nonempty selected-target map and checks actual
embedded frontend hashes against manifest evidence. Optional draft provenance must
agree with inspected native content. Packages no longer require a Sharp file or
both targets. Bare `.ninfer` remains first-class. Sidecars establish integrity and
producer lineage, not generic inference privilege; Server never duplicates Builder
master or conversion authority.

An augmented file supports MTP or DFlash2 as runtime alternatives. Included vision
weights are separate from enabled/resident Vision. Host/Device cache settings
control context retention, independently of drafting. Startup proof checks the
selected backend, draft window and proposal head alongside existing cache, Vision,
context, sampler, thinking and GPU facts. Requested settings and observed values
retain separate provenance; neither observation becomes a persisted override.

Norted owns fresh BF16 preparation and exact-parent optional augmentation. Target
payloads and all six frontend resources are byte-preserved during augmentation.
NVFP4 is not UD. Native compiled templates are exact accepted source strings;
embedding arbitrary Jinja or a Dirk/Sharp file does not make it executable.

The preserved 2026-09-13 experiment used the same native runtime and Swift-derived
OrcaRouter target: MTP3 scored 33/36 (strict 32), DFlash2 K7 scored 33/36 (strict 31),
with elapsed times 32.54 s and 21.66 s. These 36 budget-limited items are evidence
for that experiment, not a general accuracy/performance guarantee or serving defaults.
The stock-Qwen draft at `z-lab/Qwen3.8-27B-DFlash2` revision
`50307d4c4cde6860d4eee73e2547cd786fe8e8a4` was not retrained for Swift/OrcaRouter.
Implementation validation uses existing synthetic local contracts; it does not
rebuild models, rerun benchmarks or start a production service.

## Static canonical-master audit — 2026-10-05

This is a finite source audit, not a build, inference result, model migration or
blanket qualification of upstream HEAD. Canonical `Neroued/ninfer` master was
fetched once into Scala's Git objects as `refs/audit/ninfer-master`:

- Reviewed baseline: commit `d49296868dcc17bd478ec185f0d3a801bcc0bf56`, tree
  `8e2f0275fc533cf11fe05a4ac3ac85f00eb91c72`.
- Audited head: commit `68c54356fd490ab329bd1475d48957f886bb7dd1`, tree
  `a10f0928844093ef6ee9c2e0fa27e69980539e88`.
- Baseline is an ancestor; the delta contains **94 commits**.
- Of all **97** `REVIEWED_SOURCE_BLOBS`, **17 are byte-identical**, **2 have
  narrowly contract-preserving changes**, **21 have semantic changes**, and
  **57 old paths are removed/restructured**. New owner closures are unproven.
- **No capability domain advances.** All 17 owner sets contain changed or absent
  owners, and no complete successor owner set is proven. Old baseline evidence
  and the exact Windows `v0.7.1` reviewed package contract remain unchanged.

### Native-container admission is a separate contract

The audited head is **v3-only**, independently of setting or capability grants.
`src/artifact/framing.h` defines `NINFER\0\x03` entry magic, `NINPRT\0\x03` part
magic, a 32-byte header and 4096-byte payload alignment. Its
`src/artifact/reader.cpp` `check_magic` explicitly rejects a v2 artifact and points
to the offline upgrade tool. Static evidence is sufficient to reject this loader
for Scala's admitted v2 artifacts; no upgrade or conversion was run.

| Independent format owner | Reviewed v2 blob | Audited head blob |
| --- | --- | --- |
| `src/artifact/reader.cpp` | `56bce38c56746ce0b416360df470c5e949d2c1cc` | `3dfa0962bec00e854e2114c1c87de3a8acdbf114` |
| `src/artifact/reader.h` | `e27e591d3b1c339436d43b56a80f1acb2d9b488f` | `1857ed122d1a08b4c64626af623d809101ef943d` |
| `src/artifact/framing.h` | absent | `6947f0b848a1bbf3e7d1ce47138bf3be6a3f076b` |
| `src/artifact/schema.cpp` | absent | `71f7b86d1d3bb7c70aa9e21cf4819186dbc1d77e` |

The Linux catalog now resolves the **reviewed v2 commit**, not default-branch HEAD.
`Latest` means latest admitted managed snapshot, not latest upstream commit;
the label and advisory say it is pinned. Candidate re-verification also requires
that exact reviewed commit/tree. The adapter independently rejects the audited
v3 source snapshot for runtime selection and runtime/model compatibility, so a
stale candidate or installed manifest cannot admit it merely because settings
are empty or its target registry happens to resemble v2. Explicit runtime native
identities that support no v2 container are also rejected independently of any
package/domain credit, including release-package identities.

Other installed canonical source snapshots need the exact reviewed v2 reader
**and** reader-header blobs to prove container support; this does not grant any
capability domain. Source credit additionally requires the canonical repository,
repository URL, provider, managed platform/variant and matching identity revision.
Unknown source/repository identity never inherits reviewed credit. The Windows
catalog's existing exact package gate still withholds its v3 releases. Scala's
shared artifact parser remains v2-only; supporting v3 would require a separate
shared-format/frontend/draft inventory review, not a version-number bump.

### Complete baseline-blob classification

Paths below are exact upstream paths, not wildcard grants. Existing old blob
IDs remain in `REVIEWED_SOURCE_BLOBS`; the reproduction command below prints both
full blob IDs for every path. `C` is a limited declaration-preserving change,
`S` a semantic change; neither alone qualifies a complete domain.

**Byte-identical (17):**

```text
src/product/media_acquire/acquire.cpp
src/product/media_acquire/acquire.h
src/product/media_acquire/source.h
src/product/speculative_options.h
src/runtime/contract/sampling.cpp
src/runtime/contract/sampling.h
src/serve/http_transport.cpp
src/serve/openai_chat.h
src/serve/openai_chat_response.cpp
src/serve/openai_common.h
src/serve/openai_responses_store.cpp
src/serve/request_json.h
src/serve/request_validation.cpp
src/serve/request_validation.h
src/runtime/engine/kv_capacity.cpp
src/ops/wrapper/speculative_round.cpp
src/ops/kernel/speculative_round.cuh
```

**Changed (23):**

| Path | Class | Audited blob | Material delta |
| --- | --- | --- | --- |
| `apps/cli/options.cpp` | S | `5bbefbf16ab8895f29585fad407388f0d1aa4568` | Chat-template option and additional reasoning efforts. |
| `include/ninfer/types.h` | S | `75bc468c50dbf6b9dd84e169d7d4c9519704ff57` | Unified host quota; optional template-controlled thinking; scheduling/work observations. |
| `src/runtime/engine/engine.cpp` | S | `da799226380e4f4f6cfd0d3805e20b7707504302` | v3 model instances replace old target startup/dispatch. |
| `src/runtime/engine/engine_core.h` | S | `64b7a709a5ace4367608fb8b29ec72464a20db17` | ModelContract instance, pause/snapshot/replay and admission overhaul. |
| `src/serve/generation_service.cpp` | S | `92c08bd906a043cd470272714fe26f599f5d9a49` | Template defaults, changed observations and cancellation settlement. |
| `src/serve/generation_service.h` | S | `e759408579a631482a2454dc2a2a5e9cb5739d18` | Scheduling/first-output metrics and changed prompt semantics. |
| `src/serve/http_server.cpp` | S | `149849a52b230419d2bcea2ce82296a6c8ee33b7` | Interrupted transport/cancellation and scheduling logging. |
| `src/serve/http_server.h` | S | `0c7286f41ed15aec31994729b3f3503ead118ef2` | Client cancellation and scheduling callback surface. |
| `src/serve/http_transport.h` | C | `596d10785731f1961e13a9073f4bddc087a8c51d` | Only removes ClientDisconnected; exact class moved to generation_service.h. |
| `src/serve/openai_chat_http.cpp` | S | `cc47d56dd7a474811a39607a58fa6b2a1489e86d` | Interrupted-stream exception handling. |
| `src/serve/openai_chat_request.cpp` | S | `346117d8288421928a4ac4279a2b33e1c3dc9546` | Arbitrary chat_template_kwargs now passed through rather than rejected. |
| `src/serve/openai_common.cpp` | S | `aac57f9c46d997d63114de101adb22abf0f3afc2` | Effective thinking defaults resolve from prepared template output. |
| `src/serve/openai_responses_store.h` | S | `1a1990d4f8efee17119badec42818ccee5b93f28` | preserve_thinking becomes optional rather than false by default. |
| `src/serve/request.h` | S | `fcf83ada492173e0b56b8c21c74149205cbf133b` | Template kwargs, optional thinking and new metrics. |
| `src/serve/request_log.cpp` | S | `0833fdc681e2ab196e39e0f7f76e96b6141d0e85` | Changed native model/cache fields and scheduling events. |
| `src/serve/request_log.h` | S | `6230965663cda90c2c6bcd3cd6d89c921be971ea` | Schema 20 → 24 and request_scheduling records. |
| `src/serve/serve_options.cpp` | S | `94db898e8c1ecef4d0b3e860b4d59a1fca8dc7dd` | Old cache controls replaced by unified host capacity; template/reasoning changes. |
| `src/serve/serve_options.h` | S | `c8c1e1ea5f345d3e080adfd98f5ffba6159a054e` | Optional thinking/preservation; public model ID comes from artifact metadata.name. |
| `src/serve/translate.cpp` | S | `eace8e46307f0d599ba92f481fd7599c1acd109f` | Template kwargs/default/effort resolution replaces old capability checks. |
| `src/serve/translate.h` | S | `105d4f6f1689f03ad2a84627ba2fba691ac0ecb2` | Optional semantics and changed resolver signature. |
| `src/runtime/engine/kv_capacity.h` | C | `6bf6576a7b0f54d4148808e56bb6e7f4a86578a0` | Include-only move to resources.h; exact SequenceCapacityCurve/KvCapacityResolution declarations retained. |
| `src/runtime/engine/request_record.h` | S | `704a008d4908625746589dcc34b4b503177e959b` | New model-owned request/pause/recovery state and scheduling statistics. |
| `src/runtime/engine/scheduler.h` | S | `363809bd8d4c2daf975e8ea25e6ed55f83c39d38` | Bounded FIFO bypass/preemption/restoration replaces protected-head admission. |

**Removed/restructured (57):**

```text
src/runtime/engine/resource_manager.h
src/targets/qwen3_6/impl/frontend/chat_template.cpp
src/targets/qwen3_6/impl/frontend/chat_template.h
src/targets/qwen3_6/impl/frontend/digest.cpp
src/targets/qwen3_6/impl/frontend/digest.h
src/targets/qwen3_6/impl/frontend/frontend.cpp
src/targets/qwen3_6/impl/frontend/media_cache.cpp
src/targets/qwen3_6/impl/frontend/media_cache.h
src/targets/qwen3_6/impl/frontend/processor.cpp
src/targets/qwen3_6/impl/frontend/processor.h
src/targets/qwen3_6/impl/frontend/resources.cpp
src/targets/qwen3_6/impl/frontend/tool_call_parser.cpp
src/targets/qwen3_6/impl/frontend/tool_call_parser.h
src/runtime/engine/admission_policy.cpp
src/runtime/engine/admission_policy.h
src/runtime/engine/context_cost.cpp
src/runtime/engine/context_cost.h
src/runtime/engine/context_cost_defaults.cpp
src/runtime/engine/context_portfolio_value.h
src/runtime/engine/materialization_planner.h
src/runtime/engine/resource_search.h
src/runtime/engine/shared_capture_planner.h
src/targets/qwen3_6/export/ninfer/targets/qwen3_6/round_state.h
src/targets/qwen3_6/impl/runtime/dflash_context.h
src/targets/qwen3_6/impl/runtime/dflash_context_impl.h
src/targets/qwen3_6/impl/runtime/dflash_impl.h
src/targets/qwen3_6/impl/runtime/host_kv_extent_store.h
src/targets/qwen3_6/impl/runtime/layouts.h
src/targets/qwen3_6/impl/runtime/layouts_impl.h
src/targets/qwen3_6/impl/runtime/logical_kv_store.h
src/targets/qwen3_6/impl/runtime/mtp_impl.h
src/targets/qwen3_6/impl/runtime/pressure_planner.h
src/targets/qwen3_6/impl/runtime/program.h
src/targets/qwen3_6/impl/runtime/program_impl.h
src/targets/qwen3_6/impl/runtime/rebuild_work.h
src/targets/qwen3_6/impl/runtime/request_plan_impl.h
src/targets/qwen3_6/impl/runtime/resource_projection.h
src/targets/qwen3_6/impl/runtime/schedule.h
src/targets/qwen3_6/impl/runtime/state_image_store.h
src/targets/qwen3_6/impl/runtime/text_context.h
src/targets/qwen3_6/impl/runtime/text_context_impl.h
src/targets/qwen3_6/impl/runtime/text_prefill_impl.h
src/targets/qwen3_6/impl/runtime/vision_context.h
src/targets/qwen3_6/impl/runtime/vision_context_impl.h
src/targets/qwen3_6/impl/runtime/vision_prefill.h
src/targets/qwen3_6/impl/runtime/workspace_recipe.h
src/targets/qwen3_6/impl/state/decoder_state.cpp
src/targets/qwen3_6/impl/state/round_state.cpp
src/targets/qwen3_6/impl/state/state_image.cpp
src/targets/qwen3_6_27b/impl/package.cpp
src/targets/qwen3_6_35b_a3b/impl/package.cpp
src/targets/qwen3_6_27b/impl/config.h
src/targets/qwen3_6_27b/impl/variant.h
src/targets/qwen3_6_27b/impl/variant.cpp
src/targets/qwen3_6_27b/impl/load/bindings.cpp
src/targets/qwen3_6/export/ninfer/targets/qwen3_6/startup_features.h
src/targets/qwen3_6/impl/runtime/speculative_target_impl.h
```

### Domain disposition and unresolved successor owners

Counts are **identical / changed / removed**, not capability grants. Each row is
**unproven at the audited head, withheld**; the historical owner arrays remain
qualified against their individual baseline blobs.

| Owner set | I / changed / removed | Material reason credit is withheld |
| --- | --- | --- |
| CORE_PROCESS_FILES | 0 / 9 / 7 | v3 instances/targets and frontend startup replace reviewed closure. |
| CONTEXT_CACHE_FILES | 1 / 9 / 25 | Unified host quota, removed materialization/admission owners, pause/replay. |
| SPECULATION_FILES | 3 / 7 / 22 | Old Qwen target/program/draft bindings replaced. |
| SERVING_LIMIT_FILES | 0 / 9 / 8 | Scheduler/admission and resource manager replaced. |
| RESPONSES_STORE_FILES | 1 / 6 / 0 | Optional prompt defaults and changed serving semantics. |
| REQUEST_DEFAULT_FILES | 4 / 5 / 0 | Prompt defaults now depend on template output. |
| PROCESS_SAMPLER_FILES | 2 / 8 / 0 | Resolver is identical, but startup/engine/request closure changed. |
| MODEL_SAMPLER_DEFAULT_FILES | 2 / 2 / 2 | Old model package defaults removed; v3 model/config loading unproven. |
| REQUEST_SAMPLER_FILES | 6 / 9 / 0 | Parser/resolver subsets retained, full request execution closure changed. |
| REQUEST_PROTOCOL_FILES | 7 / 10 / 5 | kwargs/thinking semantics and frontend/output session replaced. |
| REQUEST_LOG_FILES | 0 / 2 / 0 | Schema 24 differs from Scala's reviewed schema-20 observations. |
| THINKING_PROCESS_FILES | 0 / 8 / 5 | Optional template-driven thinking and expanded effort values. |
| THINKING_REQUEST_FILES | 5 / 8 / 5 | Old template capability checks replaced by kwargs resolution. |
| TOOL_CALLING_FILES | 5 / 8 / 5 | New frontend template/output-session/tool parsing closure. |
| VISION_PROCESS_FILES | 0 / 7 / 21 | Model/program/resource/startup owners replaced. |
| DFLASH_VISION_FILES | 3 / 6 / 25 | Joint draft/vision bindings, storage/planning/verification unproven. |
| MEDIA_REQUEST_FILES | 7 / 8 / 8 | Acquisition subset retained, model frontend/media execution changed. |

Known successor owner locations include `src/models/registry.cpp`,
`src/models/qwen3_5/config.cpp`, `src/models/qwen3_5/load.cpp`,
`src/models/qwen3_5/load/{bindings.h,dflash.cpp,dflash2.cpp,mtp.cpp,resources.cpp,vision.cpp}`,
`src/models/qwen3_5/frontend/{chat_template.cpp,frontend.cpp,processor.cpp,output_session.cpp,tool_call_parser.cpp}`,
`src/models/qwen3_5/program/{program_impl.cpp,vision_control.cpp,vision_prefill.h}`,
`src/models/qwen3_5/program/speculative/{mtp.cpp,target_verification.cpp}`,
`src/models/qwen3_5/program/transactions/{binding.cpp,pause.cpp,replay.cpp}`,
`src/runtime/engine/{model_instance.cpp,generation_budget.h}`,
`src/runtime/engine/context_cache/{resource_manager.h,context_cost.cpp,context_cost_defaults.cpp}`,
`src/runtime/contract/resources.h` and the artifact framing/reader/schema owners.
These are **unproven successor closures**, not an exhaustive approved owner list,
not rename-equivalent evidence, and not new grants. In particular, unchanged
sampling/acceptance primitives cannot approve model defaults or speculative
execution across this architecture change. Relevant restructure commits include
`4cde7ad0` (v3 loader), `469f014c` (explicit v2 rejection guidance), and `04350ba9`
(v3 bound-instance engine).

### Reproducing the finite comparison locally

No upstream checkout/build or second HEAD fetch is needed. With the two commits
already in Scala's object store:

```sh
git rev-parse 68c54356fd490ab329bd1475d48957f886bb7dd1^{tree}
git merge-base --is-ancestor d49296868dcc17bd478ec185f0d3a801bcc0bf56 68c54356fd490ab329bd1475d48957f886bb7dd1
git rev-list --count d49296868dcc17bd478ec185f0d3a801bcc0bf56..68c54356fd490ab329bd1475d48957f886bb7dd1
python3 - <<'PY'
from pathlib import Path
import collections, re, subprocess
source = Path('crates/scala-engine-ninfer/src/lib.rs').read_text()
section = source.split('const REVIEWED_SOURCE_BLOBS:')[1].split('];')[0]
reviewed = dict(re.findall(r'\("([^"]+)", "([a-f0-9]{40})"\)', section))
head = '68c54356fd490ab329bd1475d48957f886bb7dd1'
tree = {line.split('\t')[1]: line.split()[2] for line in
        subprocess.check_output(['git', 'ls-tree', '-r', head], text=True).splitlines()}
preserving = {'src/runtime/engine/kv_capacity.h', 'src/serve/http_transport.h'}
def classification(path):
    if path not in tree: return 'removed/restructured'
    if tree[path] == reviewed[path]: return 'byte-identical'
    return 'contract-preserving changed' if path in preserving else 'semantic changed'
for path, old_blob in reviewed.items():
    print(classification(path), path, old_blob, tree.get(path, 'ABSENT'))
print(collections.Counter(classification(path) for path in reviewed))
for name, body in re.findall(r'const (\w+_FILES): &\[&str\] = &\[(.*?)\];', source, re.S):
    owners = re.findall(r'"([^"]+)"', body)
    print(name, len(owners), collections.Counter(classification(path) for path in owners))
PY
```

The preserving labels are grounded in direct diffs and exact moved declarations,
not inferred from path similarity. The tables above supply the semantic findings;
the script reproduces path/hash coverage. Validation remains synthetic: every
owner's changed or missing blob withholds its domain, canonical v2 retains credit,
v3 candidate/installed runtime selection is rejected, unknown source identity gets
no credit, and the existing exact Windows and Decision contracts remain intact.
