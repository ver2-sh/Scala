# Native NInfer, MTP and DFlash2

The current reviewed runtime is clean upstream `Neroued/ninfer` commit
`d49296868dcc17bd478ec185f0d3a801bcc0bf56`, tree
`8e2f0275fc533cf11fe05a4ac3ac85f00eb91c72`. Source owners cover CLI parsing,
process defaults, scheduler/cache/state, native target bindings, masked-draft
configuration/implementation, sparse proposal acceptance, Vision and schema-20
startup/request evidence. Runtime code and templates are not patched or emulated.

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
Norted, Swift, OrcaRouter, official and third-party origins follow identical rules.
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
