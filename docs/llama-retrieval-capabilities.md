# Native llama.cpp chat capabilities

Scala serves ordinary GGUFs using upstream llama.cpp EOS/EOG and the
model's chat template. Model producer and package provenance do not grant or
require inference capabilities. No source overlay or exact-stop runtime exists.

Capability observations belong to the running executable/model/settings tuple.
The adapter checks the executable digest against its installed identity, for
external installations and managed builds alike. It reads `/props` template
capabilities and uses `/apply-template` to verify tool definitions, assistant
call history, arguments and tool results survive rendering. A bounded native
forced-tool request must produce a valid call. A separate bounded native JSON
Schema request must finish with the exact constrained object. Unknown or failed
proofs do not advertise the corresponding feature. No model-family or upstream
revision allowlist is involved. Runtime defaults can enable Jinja; an explicit
Settings override is not required when native behavior proves support.

Tool translation, named/required/auto/none choices, tool result IDs, streaming
ToolCallDelta, and local JSON Schema validation remain. Named choice narrows the
function list and uses native `required`. Active tools and structured finalization
remain separate requests. Text stops remain ordinary native request controls.

Retrieval is available when the loaded tuple proves ToolCalling and
StructuredOutput. Its tasks, scoring and protocol are unchanged. Capability
support does not establish retrieval quality.

`llama-retrieval-validation.json` is historical evidence from a patched b10786
experiment, including the 0/100 Q4_K_M result. It is not current capability or
portability evidence and does not establish a need for those patches. See
[native serving audit](native-serving-audit.md) for the replacement validation.
