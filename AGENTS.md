# Agent instructions

## Settings and defaults policy

- Inference precedence is only `Runtime default → Model Profile → Invocation`.
- Never add a Global/shared-parent inference-default layer or a compatibility path for the removed design.
- Server operational settings are separate Server Settings, not inference settings or provenance.
- Common setting definitions may be shared in code; runtime values are independently owned and resolved.
- Every supported effective-setting row must show its concrete value. Source annotations supplement values and never replace them.
- Never substitute `runtime/model default`, `runtime-selected`, `model/thinking-mode default`, `inherited`, `default`, `automatic`, or similar prose for an effective value.
- Model Profile inheritance displays `VALUE (runtime default)`; boot overrides display `VALUE (boot inference)`.
- Derived values remain in the runtime-default layer and show the result plus optional derivation detail.
- A future apply-to-all-runtimes feature copies independent values; it must not add inheritance.
