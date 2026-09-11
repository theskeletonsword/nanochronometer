# NanoChronometer — Windows kernel driver (WDM, Rust)

Cross-platform companion to the Linux module at [`../linux/`](../linux). A
ring-0 hypervisor detector for **x86_64 and ARM64** Windows, optimized for
Microsoft Surface ARM64 devices, exposing a `key=value` report — the same
output shape as the Linux `/proc` module — through a single `METHOD_BUFFERED`
IOCTL.

```
\\.\NanoChronometer  --IOCTL 0x222004-->  version=1 arch=arm64 current_el=1 ...
```

## Layout

```
kernel/windows/
├── src/
│   ├── main.rs        DriverEntry, dispatch table, IOCTL handling
│   ├── nt.rs          WDM types/imports + verified offset constants (compile-time asserts)
│   ├── hypercall.rs   x64 cpuid/vmcall/vmmcall, arm64 hvc/CurrentEL (detection-gated)
│   ├── report.rs      key=value report writer + physical-memory demo
│   └── mem.rs         self-contained memcpy/memset/memmove/memcmp (no CRT imports)
├── defs/
│   ├── ntoskrnl.def        import library descriptor (dlltool)
│   ├── offsets-check.c     C floor-check of every Rust offset constant (both arches)
│   └── armddk-shim/        _M_ARM shim so arm64 can compile the C checks
├── certs/                  nanochrono-test.crt (public, committed)
├── certs-private/          key/pem/pfx — GITIGNORED, never commit
├── packaging/nanochrono.inf
├── tools/query.py          user-mode client (ctypes)
├── docs/PORTING_LINUX_TO_WDM.md
├── Makefile  sign.sh (Linux)  sign.bat (Windows)
```

## Build (from Linux)

```sh
cd kernel/windows
make all      # offsets checks + x64/arm64 .sys -> build/
make sign     # test-sign with osslsigncode -> build/signed/
```

Requirements: `rustc` ≥ 1.77 with `{x86_64,aarch64}-pc-windows-gnullvm`
targets; the clang-based mingw-w64 cross toolchain (default path documented in
the Makefile); `dlltool` (in the toolchain); `osslsigncode` for signing.

Run `make TOOLCHAIN=/path/to/bin all` to point at another toolchain.

## Safety model (read before loading)

Because the MinGW build has no kernel fault recovery (SEH is a no-op under
clang#windows-gnu — verified), hypercalls are **never** executed unless a
hypervisor is detected:

- x64: CPUID hypervisor-present bit **or** `KeIsHypervisorPresent()`;
- arm64: `KeIsHypervisorPresent()` (and `CurrentEL` is always reported).

Full fault-recovery (execute probes unconditionally) requires an MSVC/WDK
build; the porting guide explains the mapping in detail.

## Deployment (target machine, admin shell)

```bat
bcdedit /set testsigning on        & reboot once
sc create nanochrono type= kernel binPath= C:\nanochrono_x64.sys
sc start  nanochrono
python tools\query.py --wait
sc stop   nanochrono
sc delete nanochrono
```

On ARM64 machines (Surface Pro X / Pro 9 World Edition) use
`nanochrono_arm64.sys`.

## License

`SPDX-License-Identifier: MIT` (distributed under the same MIT license
document used by the Linux twin; see [LICENSE-MIT](../linux/LICENSE-MIT)).