# Security and reliability

`let-smi` treats GPU drivers and operating-system telemetry as optional,
untrusted boundaries. Inventory must remain usable when a vendor library,
permission, counter, or sensor is absent or malformed.

## No telemetry subprocesses

Runtime code does not import Node's child-process APIs or use Rust/C process
launchers. It never invokes `nvidia-smi`, WMI command-line tools, PowerShell,
`amd-smi`, `intel_gpu_top`, `powermetrics`, `ldd`, or another executable.
Windows telemetry uses DXGI, D3DKMT, PDH, and dynamically loaded driver APIs.
The repository check `pnpm subprocess:check` scans every runtime source file and
the native package loader for prohibited launch paths.

## Windows DLL loading

NVML candidates come only from:

- the actual system directory returned by `GetSystemDirectoryW`; and
- the legacy `NVIDIA Corporation\NVSMI` directory beneath Program Files,
  where Program Files is returned by
  `SHGetKnownFolderPath(FOLDERID_ProgramFiles)`.

Relative roots, traversal components, and bare `nvml.dll` names are rejected.
Each OS-derived candidate is loaded directly by absolute path with
`LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32`, constraining
dependency resolution per call. The mapped module path is read back with
`GetModuleFileNameW` and must equal the trusted candidate before `nvml-wrapper`
receives that same absolute path. The current directory, `PATH`, arbitrary
environment-selected NVML paths, and process-global `SetDllDirectoryW` are
never used. Absence or load failure is an unavailable-provider diagnostic, not
an addon-load failure.

## Native addon loading

The JavaScript loader considers only the package-local addon with the exact
platform filename and the exact optional-package name for the detected target.
It does not honor `NAPI_RS_NATIVE_LIBRARY_PATH` or another environment-selected
native path, and it never searches `PATH` or the current working directory for
an addon. Loader error text has control characters removed and is length
bounded before it is exposed to callers.

ADLX and Level Zero probes use bare system-library names only together with
`LOAD_LIBRARY_SEARCH_SYSTEM32`; they are presence diagnostics, remain
`loaded: false`, and expose no telemetry capabilities.

## FFI bounds

PDH formatted arrays have explicit byte/item limits, checked rounding and
multiplication, x64 structure-size/alignment validation, returned-size checks,
a maximum of three resize retries, and bounded UTF-16 pointer/terminator
validation. Malformed layouts return `provider-error`; they do not create
unchecked slices or panic.

Driver- and OS-reported device, fan, process, encoder-session, channel, state,
metric, diagnostic, and JSON collection sizes are capped before or immediately
after the corresponding provider boundary. JavaScript validates native
collection lengths, object property counts, numeric ranges, and unknown option
keys before copying data. Excess data becomes a provider diagnostic or a
native-data error; it is never silently converted to zero.

## Concurrency and shutdown

One monitor owns one sampler thread. Its command queue is bounded to 256,
subscriptions are limited to 128, each subscription allows one pending
`next()`, and its delivery slot retains only the latest snapshot. Pending
`next()` calls are waker-driven futures and consume no libuv worker.

Open, one-shot sample, refresh, diagnostics, vendor information, and close use
NAPI-RS's Tokio blocking runtime. Cancellation closes slots and wakes consumers
immediately. Explicit close is idempotent and normally joins the sampler. A
two-second exceptional deadline prevents an unconditional wait after a stuck
provider call; the unfinished native thread is detached. Driver libraries do
not generally offer a portable way to interrupt an in-progress call, so that
call may continue until it returns or the process exits. A GC finalizer only
signals nonblocking cancellation, while provider ownership stays on the sampler
until shutdown runs there.

The library requires no administrator privileges and never writes, replaces,
renames, or deletes Windows or driver files.

## Dependency and release-chain controls

Cargo and npm dependency graphs are locked, direct dependencies are exact, and
CI installs JavaScript dependencies with lifecycle scripts disabled. The known
Windows esbuild development-server advisory is excluded by a workspace override
to `0.28.2`. The public and platform packages define no consumer install hooks.

Third-party GitHub Actions use full commit hashes, checkout does not persist a
Git credential, Node/Rust/Zig/cargo-zigbuild versions are explicit, and the
musl test container is digest-pinned. Release tag values enter shell steps only
through environment variables. Artifact uploads use an exact filename; release
assembly validates regular-file size, executable format, bitness, and target
machine before publication. Platform-package manifests must contain exactly
the expected OS/CPU/libc selector and addon file, with no scripts or dependency
graph.

The pre-release audit on 2026-08-12 found no npm or Rust security advisories.
RustSec reports one maintenance warning for the unmaintained `paste` procedural
macro, transitively used by macOS-only `metal`; it is not a vulnerability and
has no Windows runtime path. Workflow static analysis reports no findings in
pedantic offline mode.
