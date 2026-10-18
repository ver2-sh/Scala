# Runtime dependency installation

The normal **Runtimes > Install** action and `scala runtimes install <runtime-id>`
own software acquisition and any required dependency/runtime build for the
admitted managed routes below. Browsing, searching, probing compatibility, and
updating the Scala application do not install dependencies. Model acquisition
and inference configuration remain separate.

## Managed routes

| Route | Software prepared by Install |
| --- | --- |
| Qualified upstream or alternative release assets | The verified executable package and declared companion assets. No compiler is acquired for a prebuilt package that does not need one. |
| Linux x86_64 llama.cpp CUDA source | Pinned private Git, CMake, Ninja, C/C++ compiler, development sysroot and the adapter-selected CUDA 12.8 or 13.2 toolchain. |
| Linux x86_64 q27 source | Pinned private Git, Make, C/C++ compiler, sysroot and the CUDA dependency lock selected by that source contract. Scala still builds only the selected upstream Make target. |
| Linux x86_64 canonical NInfer / NInfer-all source | The private compiler/build tools, sysroot, selected CUDA components, FFmpeg/curl development libraries and their complete locked dependencies. NInfer-all has its separate CUDA 13.4 lock. |
| Windows x86_64 NInfer-all source | Private pinned Git/CMake/Ninja/CUDA, vcpkg and FFmpeg/curl dependencies, plus automatic reuse or installation of compatible Microsoft C++ tools and SDK. |
| Managed Python | Private pinned CPython, uv, locked wheels and the existing engine-owned serving resources. Linux vLLM additionally acquires a private C++ environment for JIT; Windows vLLM prepares Microsoft C++ tools/SDK before its existing private SDK/wheel provisioning. |

These are supported routes, not a promise to compile arbitrary upstream source
or invent support for another operating system, CPU/GPU architecture or runtime.
Externally supplied unmanaged runtimes remain externally managed. A supported
operating system/libc, adequate storage, the selected GPU and its working driver
remain host requirements. Scala does not install a driver, enable WSL, switch a
CUDA recipe to another toolkit, or fall back to CPU.

## Isolation and reuse

Linux acquisition uses the unchanged, pinned micromamba executable with explicit
package URLs and SHA-256 locks from conda-forge. There is no install-time solver,
floating channel selection, global environment activation or host package-manager
transaction. User configuration and environment variables are not imported into
the package acquisition operation; package link scripts are disabled. The engine
adapter selects its lock, while the shared installer handles acquisition.

The stores are separate:

* `<Scala cache>/runtime-packs/native/` holds disposable native acquisition data.
  Windows extracted tools and assembled CUDA component trees are keyed by their
  own pinned inputs, so another dependency set can reuse compatible components.
  Linux environments share micromamba's package store.
* `<Scala data>/runtime-dependencies/linux/<environment-hash>/prefix/` holds an
  admitted Linux compiler/library environment needed by installed runtimes.
  A recipe's pinned inputs identify its environment; a runtime revision alone
  does not require another copy of the same toolchain.
* Installed runtime packages hold their own executable, source/evidence and
  necessary runtime-owned files. Windows native DLLs and installed Python wheels
  remain independent of disposable caches.

Micromamba uses hardlinks where the filesystem and package relocation permit.
A different filesystem or a prefix-patched file can require a copy; incompatible
versions deliberately stay separate. Windows tool extraction verifies existing
inventories before reuse. Clearing a download archive does not force a verified
extracted tool to be downloaded again.

Microsoft Build Tools and the Windows SDK are the machine-level exception, not
portable cache installations. A compatible existing installation is reused.
Otherwise Scala invokes Microsoft's verified installer at the separate
`%ProgramFiles(x86)%\Scala\BuildTools-17.14` installation path. Windows may request
administrator approval or require a restart. Scala reports cancellation/restart
requirements and never reboots the host automatically. Existing Visual Studio
instances are not silently upgraded. The Microsoft installer owns its registered
host components; cache cleanup does not uninstall them.

There are no persistent global PATH or CUDA-selection changes. Build and JIT
bindings are operational child-process environment values, not inference Settings
or Model Profile overrides.

## Integrity, lifetime and cleanup

Downloads are pinned and verified. Original source commit/tree evidence, selected
tools, effective build arguments and dependency receipts remain attributable.
Dependency metadata grants no model capabilities or origin-specific treatment.
An incomplete installation is not admitted as an installed runtime.

Linux runtimes retain references to their durable dependency environments.
Managed Python JIT bindings verify their reference, retained lock, ready receipt,
inventory and constrained environment before use. Missing, changed or escaping
files are rejected rather than replaced with a host compiler. Windows native
runtime DLLs are staged beside the executable and recorded in provenance.

Use `scala runtimes clean-cache` to reclaim acquisition caches and unreferenced
Linux dependency environments. Referenced durable environments and installed
runtimes survive. Shared acquisition leases prevent cleanup during native
installation/build work. Source-build scratch and ordinary failed/cancelled
pending work are disposable. Microsoft host tools are not rolled back by cache
cleanup.

Native downloaded archives are trimmed to 2 GiB per relevant archive cache.
This is not a 2 GiB limit on all installed compilers, libraries, Python packages,
or runtimes. Retained compatible tools intentionally trade storage for reuse;
explicit cache cleanup remains available.

## Local validation

Normal checks remain local and synthetic:

```sh
cargo fmt --all -- --check
cargo check --locked --workspace
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
python3 -m unittest discover -s crates/scala-engine/runtime -p 'test_*.py'
```

An additional opt-in Linux test exercises the actual pinned C++ dependency
installer, builds a tiny C++20 fixture, verifies same-environment reuse and
cross-environment package hardlinks, and checks cache removal. It is ignored by
routine tests because it downloads development packages:

```sh
cargo test --locked -p scala-engine --lib \
  pinned_cpp_install_build_reuse_and_clean_cache -- --ignored --nocapture
```

Use a temporary filesystem with enough real space for that opt-in download/build.
It does not build a GPU inference runtime, load a model or execute inference.
The existing tiny managed-Python fixtures also exercise production disk guards;
their test filesystem must report the required reservation. Do not weaken those
guards to run fixtures on a low-capacity development machine.

The upstream package-store behavior is documented in the
[micromamba user guide](https://mamba.readthedocs.io/en/stable/user_guide/micromamba.html)
and [root-prefix concepts](https://mamba.readthedocs.io/en/latest/user_guide/concepts.html).
