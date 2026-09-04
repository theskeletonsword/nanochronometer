/* SPDX-License-Identifier: Apache-2.0
 *
 * The one loose assembly file in this project, and the reason it has to be
 * one.
 *
 * A multiboot2 loader hands control over in 32-bit protected mode, with no
 * paging, no stack, and no guarantee about any register but EAX and EBX. Rust
 * cannot be entered there: the compiler emits 64-bit code for this target, the
 * ABI assumes a stack that does not exist yet, and `global_asm!` cannot place
 * the multiboot header in the first 32 KiB of the image the way the
 * specification requires. So this file exists to build the world Rust needs —
 * a stack, identity-mapped page tables, and long mode — and then jump into it.
 *
 * Everything after `kmain` is Rust. Every other instruction sequence in this
 * project is `core::arch::asm!`.
 */

/* ---------------------------------------------------------------------------
 * Boot headers. Both are emitted, because the two loaders that matter want
 * different ones: GRUB2 prefers multiboot2, and QEMU's `-kernel` implements
 * only multiboot1. Carrying both means the same image boots under either
 * without a separate build, and testing under QEMU needs no ISO.
 *
 * A loader scans the first 32 KiB for its magic, so the linker script places
 * this section first.
 * ------------------------------------------------------------------------ */
.section .multiboot_header, "a"

/* --- multiboot1: what QEMU's -kernel understands --- */
.align 4
    .long 0x1BADB002                /* magic                              */
    .long 0x00000000                /* flags: nothing beyond the defaults */
    .long -(0x1BADB002 + 0)         /* checksum: the three sum to zero    */

/* --- multiboot2: what GRUB2 prefers --- */
.align 8
multiboot2_header_start:
    .long 0xE85250D6                                      /* magic            */
    .long 0                                               /* architecture i386 */
    .long multiboot2_header_end - multiboot2_header_start /* header length    */
    /* The checksum makes the four fields sum to zero in 32 bits. */
    .long -(0xE85250D6 + 0 + (multiboot2_header_end - multiboot2_header_start))

    /* Framebuffer tag: ask the loader for a linear graphics mode. Zero for
     * width, height and depth means "whatever you think best", which is what
     * lets this work on a machine whose modes are unknown.
     *
     * Type 1 (optional) rather than 0: without it a loader that cannot set a
     * graphics mode refuses to boot at all, and a text-mode boot that still
     * prints to serial is far better than no boot. */
    .align 8
    .short 5                        /* tag type: framebuffer */
    .short 1                        /* flags: optional        */
    .long 20                        /* size                   */
    .long 0                         /* width:  loader chooses */
    .long 0                         /* height: loader chooses */
    .long 0                         /* depth:  loader chooses */

    /* The end tag, which terminates the tag list. */
    .align 8
    .short 0
    .short 0
    .long 8
multiboot2_header_end:

/* ---------------------------------------------------------------------------
 * 32-bit entry.
 * ------------------------------------------------------------------------ */
.section .boot, "ax"
.code32
.global _start
.type _start, @function
_start:
    /* The loader leaves no usable stack, so establish one before anything
     * that could push. */
    mov $stack_top, %esp

    /* EAX holds the boot magic and EBX the info structure pointer. Both are
     * needed in 64-bit code, and neither survives the journey: `cpuid`
     * clobbers all four general registers, and the stack cannot carry them
     * either — a 32-bit `push` writes four bytes where a 64-bit `pop` reads
     * eight, so the two values would come back fused into one. Fixed storage
     * is the only place that means the same thing on both sides of the mode
     * switch. */
    mov %eax, boot_magic
    mov %ebx, boot_info

    call check_cpuid
    call check_long_mode
    call enable_sse
    call build_page_tables
    call enter_long_mode

    /* The GDT is loaded and CS still refers to the 32-bit descriptor. A far
     * jump is the only way to reload CS, and reloading CS with a 64-bit
     * descriptor is what actually starts executing 64-bit code. */
    lgdt gdt64_pointer
    ljmp $0x08, $long_mode_start

/* CPUID is not architecturally guaranteed: it exists only if bit 21 of EFLAGS
 * can be toggled. Everything below depends on it, so it is checked first. */
check_cpuid:
    pushfl
    pop %eax
    mov %eax, %ecx
    xor $(1 << 21), %eax
    push %eax
    popfl
    pushfl
    pop %eax
    push %ecx
    popfl
    cmp %ecx, %eax
    je no_cpuid
    ret

/* Long mode is reported by CPUID.80000001H:EDX[29], and that leaf only exists
 * if the maximum extended leaf reaches it. */
check_long_mode:
    mov $0x80000000, %eax
    cpuid
    cmp $0x80000001, %eax
    jb no_long_mode
    mov $0x80000001, %eax
    cpuid
    test $(1 << 29), %edx
    jz no_long_mode
    ret

/* Enables SSE, and AVX where the CPU has it.
 *
 * `x86_64-unknown-none` is a soft-float target: the compiler will not touch
 * XMM unless told to, because on a freestanding target nothing has enabled
 * it. This kernel does tell it to (`-C target-feature=+sse,+sse2`), because
 * the SIMD probes it shares with the hosted build are the whole point of
 * measuring here — so the enable has to happen, and it has to happen before
 * any Rust runs. Executing an SSE instruction with CR0.EM set or CR4.OSFXSR
 * clear is #UD, not a slow path. */
enable_sse:
    mov %cr0, %eax
    and $~(1 << 2), %eax            /* EM: no coprocessor emulation */
    or  $(1 << 1), %eax             /* MP: monitor coprocessor */
    mov %eax, %cr0

    mov %cr4, %eax
    or  $(1 << 9), %eax             /* OSFXSR: FXSAVE/FXRSTOR and SSE */
    or  $(1 << 10), %eax            /* OSXMMEXCPT: SIMD FP exceptions */
    mov %eax, %cr4

    /* Everything wider than XMM needs the OS to declare that it will save the
     * state, which is what XCR0 is for. Without it CPUID still reports AVX
     * and AVX-512, but every VEX or EVEX instruction is #UD — which is
     * exactly the trap the hosted build's XGETBV check exists to avoid, and
     * here it has to be made *true* rather than merely detected.
     *
     * OSXSAVE is set whenever XSAVE exists, not only when AVX does: it is
     * what makes XGETBV legal, and the feature detection reads XCR0 to decide
     * what the OS has enabled. Leaving it clear would make that read return
     * zero and every wide register file look disabled. */
    mov $1, %eax
    cpuid
    test $(1 << 26), %ecx           /* CPUID.1:ECX[26], XSAVE */
    jz 2f

    mov %cr4, %eax
    or  $(1 << 18), %eax            /* CR4.OSXSAVE, required before XGETBV */
    mov %eax, %cr4

    /* Which XCR0 bits this CPU actually supports, from CPUID.0DH:EAX. XSETBV
     * raises #GP on any bit the processor does not implement, so the wanted
     * set is masked by the supported set rather than written blind. */
    mov $0x0D, %eax
    xor %ecx, %ecx
    cpuid
    mov %eax, %ebx

    /* Bits 0 and 1 are x87 and SSE, 2 is AVX (YMM_Hi128), and 5, 6 and 7 are
     * the three AVX-512 components: opmask, ZMM_Hi256 and Hi16_ZMM. All of
     * AVX-512 needs all three, which is why the hosted detection checks
     * 0xE6 and not just bit 5. */
    and $0b11100111, %ebx
    or  $0b00000011, %ebx           /* x87 and SSE are always supported */

    xor %ecx, %ecx
    xgetbv                          /* EDX:EAX = current XCR0 */
    or  %ebx, %eax
    xsetbv
2:
    ret

/* Identity-maps the first four gigabytes with four 1 GiB pages.
 *
 * One gigabyte would cover everything this kernel allocates, but not
 * everything it must reach: a framebuffer is memory-mapped I/O, and firmware
 * commonly puts it high — QEMU's standard VGA lands around 0xFD000000. A
 * mapping that stopped at 1 GiB would fault on the first pixel, so the range
 * is sized by where devices are rather than by how much memory is used.
 *
 * Four huge pages need four entries instead of two thousand; 1 GiB pages are
 * available on every CPU with long mode and PDPE1GB. */
build_page_tables:
    /* PML4[0] -> PDPT */
    mov $pdpt, %eax
    or $0b11, %eax                  /* present + writable */
    mov %eax, pml4

    /* PDPT[0..3] -> 1 GiB pages at 0, 1G, 2G, 3G.
     *
     * PCD (bit 4) is set on the upper three: they cover device space, and a
     * cached framebuffer write may sit in a line instead of reaching the
     * display. The first gigabyte is where the kernel and its stack live and
     * stays cached. */
    mov $0b10000011, %eax           /* present + writable + huge */
    mov %eax, pdpt

    mov $0x40000000, %eax
    or  $0b10010011, %eax           /* + PCD: uncached */
    mov %eax, pdpt + 8

    mov $0x80000000, %eax
    or  $0b10010011, %eax
    mov %eax, pdpt + 16

    mov $0xC0000000, %eax
    or  $0b10010011, %eax
    mov %eax, pdpt + 24
    ret

enter_long_mode:
    /* CR3 points the MMU at the tables built above. */
    mov $pml4, %eax
    mov %eax, %cr3

    /* PAE (CR4 bit 5) is mandatory for long mode: the 4-level page tables
     * only exist in PAE format. */
    mov %cr4, %eax
    or $(1 << 5), %eax
    mov %eax, %cr4

    /* EFER.LME (bit 8) requests long mode. It does not take effect until
     * paging is enabled, which is the next step. */
    mov $0xC0000080, %ecx
    rdmsr
    or $(1 << 8), %eax
    wrmsr

    /* CR0.PG (bit 31) turns paging on, which activates long mode. The CPU is
     * in compatibility mode from here until CS is reloaded. */
    mov %cr0, %eax
    or $(1 << 31), %eax
    mov %eax, %cr0
    ret

/* Failure paths. There is no console yet — the UART is not initialised and
 * would need code that cannot run before long mode — so the only signal
 * available is the halt itself. */
no_cpuid:
no_long_mode:
    cli
1:  hlt
    jmp 1b

/* ---------------------------------------------------------------------------
 * 64-bit entry.
 * ------------------------------------------------------------------------ */
.code64
long_mode_start:
    /* The data segment registers still hold 32-bit selectors. Long mode
     * ignores their bases, but a stale selector faults on load, so they are
     * zeroed. */
    xor %ax, %ax
    mov %ax, %ss
    mov %ax, %ds
    mov %ax, %es
    mov %ax, %fs
    mov %ax, %gs

    /* Recover the values saved in 32-bit mode, into the first two System V
     * argument registers. A 32-bit load zero-extends, which is what is
     * wanted: both quantities are 32-bit. */
    mov boot_magic, %edi
    mov boot_info, %esi

    call kmain

    /* kmain is `-> !` and does not return. If it somehow does, stopping is
     * the only safe thing left. */
    cli
1:  hlt
    jmp 1b

/* ---------------------------------------------------------------------------
 * Tables and stack.
 * ------------------------------------------------------------------------ */
.section .rodata
.align 8
gdt64:
    .quad 0                                     /* the mandatory null entry */
    /* Code segment: executable, descriptor type, present, 64-bit.
     * Long mode ignores base and limit, so only the flags matter. */
    .quad (1<<43) | (1<<44) | (1<<47) | (1<<53)
gdt64_pointer:
    .short . - gdt64 - 1
    .quad gdt64

.section .data
.align 4
/* Written in 32-bit mode, read in 64-bit mode. In `.data` rather than `.bss`
 * because a multiboot loader is not required to zero `.bss`, and a stale
 * value here would be reported as a boot magic. */
boot_magic:
    .long 0
boot_info:
    .long 0

.section .bss
.align 4096
pml4:
    .skip 4096
pdpt:
    .skip 4096
stack_bottom:
    /* 64 KiB. The kernel makes no deep calls and allocates nothing, but the
     * measurement buffers are stack-resident. */
    .skip 65536
stack_top:
