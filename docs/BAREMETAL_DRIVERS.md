# Bare-metal drivers: what is here, and what is not

The freestanding build talks to four things: a display, a keyboard, a pointer,
and the power controller. This is what each one does, and — where the answer
is "nothing" — why.

FreeBSD's tree under `/home/skels/Documentos/bsd` was the reference for the
input path. No code was copied; the register names and the initialisation
order follow `sys/dev/atkbdc/atkbdcreg.h` and `psm.c` so the two can be read
side by side.

---

## Display — generic, and deliberately so

**Status: working.** Verified under QEMU at 800×600×32 booted from the ISO.

There is no vendor driver here and there should not be one. The kernel asks
the loader for a linear framebuffer through the multiboot2 header, and the
loader gets it from the firmware — VBE on a BIOS machine, GOP on a UEFI one.
The firmware talks to the card; the kernel gets an address, a pitch, a size
and a pixel format, and writes pixels.

That **is** the generic path, and it is generic in the way that matters:

| | Firmware framebuffer | Native driver |
|---|---|---|
| Intel integrated | works | needs an i915 driver |
| AMD integrated / discrete | works | needs an amdgpu driver |
| NVIDIA discrete | works | needs nouveau or the blob |
| A card released next year | works | needs a new driver |

A "generic driver for integrated and dedicated cards" that talks to hardware
directly does not exist, because there is no common register interface to talk
to. What exists is the firmware's mode-setting interface, which is what this
uses. The cost is that the mode is fixed at boot and there is no acceleration
— neither of which matters for drawing a static readout.

The one hardware-specific thing the kernel does is map the framebuffer:
firmware commonly places it high in MMIO space (QEMU's standard VGA lands near
`0xFD000000`), so `boot32.S` identity-maps the first four gigabytes rather
than the first one, with the upper three marked uncached.

---

## Keyboard and pointer — PS/2, which is also USB in practice

**Status: working.** Verified under QEMU: keyboard scancodes and mouse
packets, with a software cursor and clickable controls.

`crates/nanochrono-baremetal/src/input.rs` drives the 8042 controller:
controller command byte, both port resets, the IntelliMouse "magic knock" to
unlock 4-byte packets, packet resynchronisation on bit 3, and nine-bit sign
extension on the movement deltas.

**A touchpad needs no separate driver.** A PS/2 touchpad speaks the same
protocol; Synaptics and ALPS extensions add gestures and absolute coordinates,
but this interface wants a cursor and the standard protocol carries one.

### Why this covers USB peripherals on real hardware

On a real machine the keyboard and mouse are USB. Firmware translates them to
8042 for exactly this case — a loader or an early kernel with no USB stack.
That is "legacy USB support" in the firmware setup, and it is on by default on
essentially every machine with a BIOS-compatible mode.

Where it is switched off, the kernel reports no device rather than appearing
to work, and the status bar says which pointer it found.

### BadUSB

BadUSB is a USB device that declares itself a keyboard and types. The mitigation
here is structural rather than a feature: **this kernel has no USB stack, so it
cannot enumerate a USB device at all.** Anything reaching it has already been
translated by firmware, and firmware does not translate a device that
enumerates after boot.

That is not a general defence — it is a consequence of not having the
functionality. If a USB stack is ever added, the policy it needs is:

1. Enumerate once, at boot, and refuse any device that appears afterwards. A
   keystroke injector's entire method is arriving later.
2. Refuse a composite device that claims both mass storage and HID.
3. Never act on HID input from a device on the port the boot medium came from.

None of that is implemented, because there is nothing to implement it in.

---

## USB and filesystems — not here

**Status: not implemented.** This is the honest part.

The kernel does not need USB to be *booted from* USB. The ISO is a hybrid
image (see below); firmware and GRUB read the medium, and by the time Rust
runs the kernel is in memory and the medium is not touched again.

What a USB stack would be needed for is reading files at run time, and it is
not a small addition:

| Piece | What it is |
|---|---|
| PCI enumeration | Find the host controller |
| XHCI driver | Rings, TRBs, doorbells, event handling. FreeBSD's is ~5000 lines |
| USB core | Descriptors, addressing, control transfers, configuration |
| HID class | Report-descriptor parsing, boot protocol |
| Mass storage class | Bulk-only transport, plus a SCSI subset |
| FAT32 | Directory walk, cluster chains, long names |
| exFAT | A different on-disk format again |

Also an interrupt controller and a timer, because USB is not a polled bus.

Writing that from a reference without hardware to test it against produces
code that compiles and does not work, which is worse than nothing here. It is
absent rather than approximated.

---

## Boot media

The ISO `packaging/baremetal/build.sh` produces is a **hybrid image**,
verified as such:

```console
$ python3 - <<'PY'
d = open('nanochrono.iso','rb').read(2048)
print('MBR signature:', d[510:512] == b'\x55\xaa')
PY
MBR signature: True

$ xorriso -indev nanochrono.iso -report_el_torito plain
El Torito boot img :   1  BIOS  y   none  ...
El Torito boot img :   2  UEFI  y   none  ...
```

An MBR with a boot signature and a protective partition, plus El Torito images
for **both** BIOS and UEFI. That is what each tool needs:

| Tool | How it uses the image |
|---|---|
| `dd` / Rufus in DD mode | Byte-for-byte to the stick; the MBR makes it bootable |
| Rufus in ISO mode | Extracts and installs its own loader; GRUB config is found |
| Ventoy | Chainloads the ISO from its own menu |
| YUMI, UNetbootin | Extract and chainload |

`dd if=nanochrono.iso of=/dev/sdX bs=4M status=progress` is the simplest path
and needs nothing else.
