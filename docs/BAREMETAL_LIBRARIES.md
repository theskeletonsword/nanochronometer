# Using NanoChronometer from your own freestanding kernel

Two artifacts, and one of them needs work from you before it does anything.

```sh
./packaging/baremetal/build.sh
```

```
dist/baremetal/x86_64/
  libnanochrono_baremetal.a      static: link it and you are done
  libnanochrono_baremetal.so     dynamic: read the second half of this file
  nanochrono-kernel.elf          the demo kernel
  nanochrono-kernel.mb.elf       the same, as ELF32 for QEMU's -kernel
```

---

## Static — the one that just works

```sh
ld -T your-linker.ld your-kernel.o -L dist/baremetal/x86_64 -lnanochrono_baremetal
```

The archive carries the counter routes, the direct PMU, the ECC/TMR machinery,
the framebuffer and the PS/2 input driver. Your linker resolves every symbol at
link time and drops what you do not call.

Two things must match, or the result miscompiles rather than failing to link:

* **Target.** `x86_64-nanochrono-none` if you want the SIMD probes,
  `x86_64-unknown-none` otherwise. They differ in ABI — the second is
  soft-float — and linking objects built for one against a library built for
  the other passes floats in different places.
* **Red zone off.** Both targets set `disable-redzone`; if you use a custom
  spec, set it there too. An interrupt writes over the red zone, and the
  corruption is silent.

Before calling anything that touches a vector register, your boot code must
enable the state: `CR0.EM` clear, `CR0.MP` and `CR4.OSFXSR` set, then
`CR4.OSXSAVE` and `XCR0` for AVX. `crates/nanochrono-baremetal/boot/boot32.S`
does all of it and is a working reference. Without it every SIMD instruction is
`#UD`, not a slow path.

---

## Dynamic — you must supply the runtime

**`libnanochrono_baremetal.so` will not load itself.** There is no
`ld.so` on bare metal, so nothing exists to map the segments, apply the
relocations and bind the symbols. Shipping the file without saying so would be
shipping something that cannot work.

If you want dynamic linking in your kernel, here is what you are signing up
for.

### 1. Load the segments

Walk the ELF program headers and map each `PT_LOAD` at
`p_vaddr + load_bias`, with `p_memsz` bytes, zeroing the tail beyond
`p_filesz`. Honour `p_flags`: a page that does not need to be executable
should not be.

`load_bias` is yours to choose. The library is position-independent, which is
what makes that possible.

### 2. Apply the relocations

`PT_DYNAMIC` points at the tables. On x86-64 you will see:

| Relocation | What to write |
|---|---|
| `R_X86_64_RELATIVE` | `load_bias + addend` — the bulk of them |
| `R_X86_64_GLOB_DAT` | The symbol's address |
| `R_X86_64_JUMP_SLOT` | The symbol's address, or a resolver stub |
| `R_X86_64_64` | `symbol + addend` |

Relocations live in `DT_RELA` with `DT_RELASZ` bytes, and `DT_JMPREL` with
`DT_PLTRELSZ` for the PLT. **Do not skip `R_X86_64_RELATIVE`**: every absolute
address in the library is wrong until you have applied them, and the failure is
a jump into nothing rather than a diagnostic.

### 3. Resolve symbols

`DT_SYMTAB` and `DT_STRTAB` give the symbol table and its strings;
`DT_GNU_HASH` (or `DT_HASH`) gives the lookup structure. You can walk the
symbol table linearly instead — it is slower and much shorter to write, and a
kernel resolving a few dozen symbols once will not notice.

This library needs nothing from its host: it is `no_std` and calls no
imports. So your resolver only has to satisfy references *into* the library,
not out of it. That is what makes it a tractable first loader.

### 4. Flush the instruction cache

On AArch64, after writing relocations into memory you are about to execute:
`dc cvau` on each line, `dsb ish`, `ic ivau`, `dsb ish`, `isb`. Skipping this
works right up until it does not, on a machine with a larger cache than yours.

x86-64 keeps its caches coherent with instruction fetch and needs none of this.

### 5. Then call it

```rust
type Detect = unsafe extern "C" fn() -> u32;
let detect: Detect = core::mem::transmute(resolve(b"nc_pmu_detect\0")?);
```

### Why you might not want to

The static archive gives you the same code with none of that, and a kernel
rarely needs to swap an implementation at run time. The dynamic library is
here because it was asked for and because loadable modules are a legitimate
design — not because it is the easier path.

If you build the loader, the ELF header parsing in
`crates/nanochrono-baremetal/src/multiboot.rs` is a working example of reading
a structure out of raw physical memory safely in this codebase's style.
