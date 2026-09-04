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
[`LICENSE.MIT`](LICENSE.MIT) for the MIT text; the GPL-2.0 text is the kernel's
own `COPYING`.

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
