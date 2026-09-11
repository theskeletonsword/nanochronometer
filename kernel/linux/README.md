# `nanochrono.ko` — optional ring 0 hypervisor detection

**You do not need this module.** NanoChronometer detects hypervisors from
userspace through CPUID and platform signatures, and that works without any
privileges. This module only sharpens the answer.

## What it adds

| Probe | Why ring 3 cannot do it |
|---|---|
| `VMCALL` / `VMMCALL` / `HVC #0` | The instructions are only valid inside a guest. Outside one they raise an undefined-instruction fault, which userspace cannot recover from safely. A hypercall that *returns* is proof of a hypervisor — and it holds even against one that clears the CPUID hypervisor bit. |
| VMX / SVM feature MSRs | Distinguishes "not virtualized" from "not virtualized, but this CPU can host a guest". |
| Trap cost with preemption disabled | Removes the scheduling noise that makes the userspace estimate fuzzy. |

Every probe recovers from its fault through the kernel exception tables, so
loading this on bare metal is safe: the instruction faults, the fixup marks it
unsupported, and execution continues. The module is read-only and takes no
input.

## It is written in Rust

Like the rest of the project. That needs three things, all of which this
Makefile handles or checks:

- `CONFIG_RUST=y` in the running kernel.
- The kernel's `rust/*.rmeta` metadata, shipped with the kernel headers.
- **The exact `rustc` that built the kernel.** Rust crate metadata is version-
  locked, so a rustup toolchain will fail with `E0514` even at the same version
  number. The Makefile defaults to `/usr/bin/rustc`, the distribution compiler.
  Check with `grep CONFIG_RUSTC_VERSION_TEXT /boot/config-$(uname -r)`.

There is no Rust abstraction for kernel exception tables, so the module emits
its own `__ex_table` entries from `asm!`, in the layout
`arch/x86/include/asm/asm.h` defines. You can confirm they were emitted:

```sh
objdump -h nanochrono.ko | grep __ex_table   # 12 bytes per probe
```

## Build and load

```sh
cd kernel/linux
make                     # needs kernel headers: /lib/modules/$(uname -r)/build
sudo insmod nanochrono.ko
cat /proc/nanochrono
```

`make load` does the last two steps. `sudo rmmod nanochrono` unloads it.

The report is at `/proc/nanochrono`, mode 0444, so any process can read it.
The kernel's Rust crate exposes debugfs but not procfs, and debugfs is mode
0700 — which would have limited the report to root, defeating the point. So
the module declares the procfs ABI itself. That means a hand-written
`#[repr(C)]` mirror of `struct proc_ops`, which is sound only without struct
randomisation; the module refuses to build otherwise:

```rust
#[cfg(not(CONFIG_RANDSTRUCT_NONE))]
compile_error!(...);
```

The library picks the module up automatically on its next detection — nothing
to configure:

```sh
nanochrono hypervisor
```

The report line changes from `kernel module : not loaded` to `loaded (v1)`
with the hypercall results underneath.

## Output format

One `key=value` per line at `/proc/nanochrono`. Keys are stable; the parser
ignores ones it does not know, so an older library keeps working against a
newer module.

```
version=1
arch=x86
cpuid_hypervisor_bit=0
vmx_available=1
svm_available=0
cpuid_vendor=
vmcall_ok=0
vmmcall_ok=0
exit_cycles=58
baseline_cycles=12
```

## Windows port

The same detection logic — probes, gate, report format — is ported to a
Windows kernel driver (WDM, Rust) in [`../windows/`](../windows), built for
x86_64 and ARM64 with the `*-pc-windows-gnullvm` rustc targets. The Windows
build cannot rely on `__ex_table` fixups (SEH fault recovery is unavailable
under the MinGW ABI), so its hypercall probes are detection-gated instead; the
porting guide ([`../windows/docs/PORTING_LINUX_TO_WDM.md`](../windows/docs/PORTING_LINUX_TO_WDM.md))
maps every primitive and records the verified ABI offsets.

## Licensing

This directory is **dual MIT / GPL-2.0**, not Apache-2.0 like the rest of the
project. That is a constraint the kernel imposes, not a preference:
`MODULE_LICENSE()` accepts only a fixed set of idents — `GPL`, `GPL v2`,
`GPL and additional rights`, `Dual BSD/GPL`, `Dual MIT/GPL`, `Dual MPL/GPL`,
`Proprietary` — and none of them is Apache. Anything outside that set is
treated as proprietary, taints the kernel on load, and loses access to
`EXPORT_SYMBOL_GPL` symbols. Apache-2.0 is also generally held to be
incompatible with GPLv2, which the kernel is.

`Dual MIT/GPL` is the most permissive recognised option: it loads without
tainting, and an Apache-2.0 project can redistribute it without friction. See
[`LICENSE-MIT`](LICENSE-MIT) for the MIT text and [`LICENSE-GPL`](LICENSE-GPL)
for the GPL-2.0 text.

The boundary is clean: this directory shares no code with the rest of the tree
and communicates only through the text format above.

## Caveats

- Kernel headers matching the running kernel are required to build, plus the
  distribution `rustc` that built it.
- The C version disabled preemption around the trap measurement. The Rust
  kernel crate exports no preemption abstraction, so the module takes the
  minimum over 128 rounds instead — which discards exactly the samples a
  scheduling decision would have inflated.
- Secure Boot will refuse an unsigned module. Sign it, or disable Secure Boot,
  or simply skip the module — userspace detection still works.
- `hvc` on AArch64 is only meaningful from EL1. On a host kernel running at EL2
  (VHE) the instruction has different semantics; the module reports
  `current_el` so the reading can be interpreted.

## The crypto benchmark (ring 0)

The module also times the kernel's hash algorithms and publishes what it
measured. This is the optional half of a two-part measurement:

| Half | How it reaches the algorithm | Needs |
|---|---|---|
| Ring 3 | `AF_ALG` socket: `sendmsg` + `read` per operation | nothing — always built |
| Ring 0 | a direct call, no socket, no syscall, no copy | this module |

The ring-3 half is the honest cost of *using* kernel crypto from a program,
and it is what the benchmark reports by default. It cannot separate the
primitive from the transport. This half can: the same algorithm over the same
16 KiB buffer, with none of the boundary crossing. **The difference between
the two numbers is what `AF_ALG` costs.**

Hashes only. A `shash` is one exported call over a flat buffer; a symmetric
cipher needs a request object, scatterlists and a completion, and a benchmark
that got any of those wrong would report a number for something other than
what it named.

Published as repeated `crypto=` keys, because a kernel algorithm name can
contain characters — `cbc(aes)` — with no business on the left of an `=`:

```console
$ grep crypto /proc/nanochrono
crypto_payload_bytes=16384
crypto_rounds=64
crypto=sha256,10431
crypto=sha512,24887
```

The value is **cycles**, best of `crypto_rounds`, not nanoseconds: the module
reads the same counter the userspace side does, and it has no calibration of
its own to convert with. Best rather than mean for the reason every other
measurement in this project takes a minimum — the fastest observed run is the
one least disturbed by everything else the machine was doing.

`nanochrono bench --mode kernel` picks this up automatically when the module
is loaded and says `ring 0 module: not loaded` when it is not. Nothing
requires it.
