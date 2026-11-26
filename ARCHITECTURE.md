# Architecture & contributor notes

Design notes for anyone working on this driver. Read alongside `README.md`.

## What this is

A from-scratch **Linux ALSA kernel driver**, written in Rust, for the MOTU PCI-324 / PCI-424
audio card, plus a userspace reverse-engineering toolset and a CueMix-style
control app. There is no upstream vendor documentation for this hardware.

## Critical context: the driver is a framework over unconfirmed hardware

The PCI/IRQ/ALSA plumbing is complete, and the hardware layer implements the
**model recovered from the vendor driver and the card's DSP program** (a
TMS320C6412 in PCI boot mode, host-loaded FPGA and DSP program, DSP bus-master
DMA from a host ring — see `docs/`). The firmware load and the stream have run
on a real PCI-424 with the C driver the Rust one was ported from; the Rust
port has not run on a card yet. What remains unverified is flagged
`TODO: verify` — chiefly the frame layout on the host ring, so streaming stays
off until the `motu424.frame_bytes=` module parameter is set. Everything
uncertain is deliberately confined to one file:

- `kernel/hw.rs` — the register map and the *only* code encoding real register
  semantics.

**When changing hardware behaviour, change only `hw.rs`.** The PCI layer
(`motu424_main.rs`), the ALSA layer (`alsa.rs`) and the parameters
(`params.rs`) must stay hardware-agnostic: they call into `hw.rs` and never
touch a register offset directly.

## Architecture

The module is Rust, with no C. The C driver it replaces is in the history up
to commit `1cb25bb`; `hw.rs` follows its `motu424_hw.c` function by function
and names the same vendor routines.

```
motu424_main.rs  pci::Driver: enable, BAR roles (map_bars), DMA mask, boot,
   │             bus master, IRQ vector + irq::Registration, teardown order.
   ▼
alsa.rs          ALSA card + PCM ops (open/prepare/trigger/pointer), over
   │             bindings bindgen generates at build time. Imposes the
   │             fixed-frame limits; IRQ → snd_pcm_period_elapsed().
   ▼
hw.rs            Windowed card-address accessors with a write allow-list,
                 FPGA + DSP program load (sha256.rs checks the images),
                 relock, idle stream, ring start/stop/position, IRQ handler.
                 THE hardware truth lives here.

params.rs        The module parameters, as kernel_param entries.
```

Key facts an implementer must keep consistent:

- **Firmware is host-loaded at probe** (`hw::boot()`): both images are MOTU's,
  extracted by the user, loaded with `request_firmware()` from
  `/lib/firmware/motu424/` and checked by length and SHA-256. Never commit
  them.
- **Transport is DSP bus-master DMA**: the ALSA buffer is the 512 KB coherent
  host ring (`SNDRV_DMA_TYPE_DEV`, 32-bit mask; `motu424_main.rs` enables bus
  mastering after the DSP has been reset and reloaded). The DSP reads it by
  itself; the pointer comes from the bus address it reports at
  `audio_base+0x2c`.
- **Frame format is not known yet**: playback only, 48 kHz only, exposed as
  `frame_bytes / 4` raw `S32_LE` words. Rate changes need the vendor relock
  sequence, which only runs once, at load.
- **BAR roles** are decided by `map_bars()` from BAR type and size: window A
  (8 MB MMIO), window B (4 MB MMIO) and the I/O-port HPI registers.
- **Teardown order** is fixed by `PinnedDrop` on the driver data, then its
  field order: disconnect the ALSA card (`snd_card_disconnect_sync()`, so no
  PCM callback can reach the chip afterwards), quiesce and reset the DSP
  (ending its DMA and interrupts), free the IRQ, drop the chip with its DMA
  buffers and BAR mappings, disable the device. The ALSA card itself is
  devres-managed and freed last. Preserve that order.
- **Locking:** the IRQ handler takes no lock (the Rust `SpinLock` does not
  disable interrupts). It acks RSTSRC and reads an atomic `playing` flag;
  the lock serialises the start/stop, idle-stream and shutdown sequences.

## Where the Rust kernel API stops

The code says so at each place:

- **ALSA has no Rust bindings.** `Kbuild` generates them with bindgen from
  the kernel's `<sound/core.h>` and `<sound/pcm.h>`, with the flags the
  kernel's `rust/Makefile` uses for its own bindings (including the
  GCC-flag filter for GCC-built kernels), into `alsa_generated.rs`. Every
  ALSA structure is laid out from the real headers; nothing is copied by
  hand. Two uapi constants bindgen cannot evaluate (`SNDRV_PCM_FORMAT_S32_LE`,
  the playback stream number) are written out in `alsa.rs`.
- **Module parameters are integers only** in `module!`. `params.rs` writes
  the `struct kernel_param` entries by hand, with the parsers the kernel
  exports (`param_ops_uint`, `param_ops_bool`, `param_array_ops`, …), so
  `index`/`id`/`enable` stay arrays and `configure`/`idle_stream` stay bools,
  with the same names, types, descriptions and 0444 sysfs files as in C.
- The I/O-port BAR is read with `ioread32()` (the `Bar` accessors are
  MMIO-only); the device is enabled with `pci_enable_device()` (the API only
  has `enable_device_mem()`); the PCI IDs are built from
  `struct pci_device_id` (there is no `pci::Vendor` for MOTU); `sha256.rs`
  replaces the kernel's `sha256()`, which has no binding.

## Build & test

Requires the headers of a kernel built with `CONFIG_RUST`, the exact `rustc`
that built it, and `bindgen` with libclang (Arch: `rust-bindgen`, `clang`).

```sh
make            # build module (kernel/) + userspace tools (tools/)
make module     # module only
make tools      # userspace tools; builds without kernel headers
make load       # sudo insmod kernel/motu424.ko
make unload     # sudo rmmod motu424
make clean      # note: also runs cargo clean in tools/
make -C /lib/modules/$(uname -r)/build M=$PWD/kernel CLIPPY=1 modules   # Clippy
```

`BINDGEN=/path/to/bindgen` overrides the bindgen binary. There is no
unit-test harness (kernel module). Verification is empirical: `make load`,
then check `dmesg` and `aplay -l` / `arecord -l`.

## Reverse-engineering workflow

The userspace tools are Rust, one Cargo workspace in `tools/` (`make tools`).
`motu424-probe` scans `/sys/bus/pci` for vendor `0x137A`, classifies the BARs
and dumps them through the sysfs `resourceN` files. Use it with the driver
**unbound** to locate registers by diffing idle vs. streaming dumps;
`motu424-ringwatch` reads the DSP's ring position while the driver streams.
`tools/re/` holds the static-RE helpers (`vtable-scan`, `xref`, `dis-c64x`). Record
confirmed offsets in `kernel/hw.rs` and the docs as they are established. Vendor ID
`0x137A` = Mark of the Unicorn; the register semantics recovered so far are
documented in `docs/`.
