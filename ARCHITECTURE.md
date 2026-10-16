# Architecture & contributor notes

Design notes for anyone working on this driver. Read alongside `README.md`.

## What this is

A from-scratch **Linux ALSA kernel driver** for the MOTU PCI-324 / PCI-424
audio card, plus a userspace reverse-engineering toolset and a CueMix-style
control app. There is no upstream vendor documentation for this hardware.

## Critical context: the driver is a framework over unconfirmed hardware

The PCI/IRQ/ALSA plumbing is complete, and the hardware layer implements the
**model recovered from the vendor driver and the card's DSP program** (a
TMS320C6412 in PCI boot mode, host-loaded FPGA and DSP program, DSP bus-master
DMA from a host ring — see `docs/`). The firmware load has run on a real
PCI-424; the stream path has not yet. What remains unverified is flagged
`TODO: verify` — chiefly the frame layout on the host ring, so streaming stays
off until the `motu424.frame_bytes=` module parameter is set. Everything
uncertain is deliberately confined to two files:

- `kernel/motu424.h` — the register map (offsets/bits marked `TODO: verify`).
- `kernel/motu424_hw.c` — the *only* file encoding real register semantics.

**When changing hardware behaviour, change only these two files.** The PCI layer
(`motu424_main.c`) and PCM layer (`motu424_pcm.c`) must stay hardware-agnostic —
they call into `motu424_hw.c` and never touch register offsets directly. Do not
leak a register access or a `MOTU424_REG_*` reference into those files.

## Architecture

Data/control flow is a clean three-layer split:

```
motu424_main.c   PCI attach, maps ALL BARs generically, IRQ vector, card create.
   │             IRQ handler → motu424_hw_irq_ack() → snd_pcm_period_elapsed()
   ▼
motu424_pcm.c    ALSA PCM ops (open/prepare/trigger/pointer). Translates ALSA
   │             callbacks into motu424_hw_* calls; imposes fixed-frame limits.
   ▼
motu424_hw.c     Windowed card-address accessors with a write allow-list,
                 BAR→window assignment, FPGA + DSP program load, ring
                 start/stop/position, IRQ ack. THE hardware truth lives here.
```

Key facts an implementer must keep consistent:

- **Firmware is host-loaded at probe** (`motu424_hw_init()`): both images are
  MOTU's, extracted by the user, loaded with `request_firmware()` from
  `/lib/firmware/motu424/` and checked by length and SHA-256. Never commit them.
- **Transport is DSP bus-master DMA**: the ALSA buffer is the 512 KB coherent
  host ring (`SNDRV_DMA_TYPE_DEV`, 32-bit mask, `main.c` enables bus
  mastering). The DSP reads it by itself; the pointer comes from the bus
  address it reports at `audio_base+0x2c`.
- **Frame format is not known yet**: playback only, 48 kHz only, exposed as
  `frame_bytes / 4` raw `S32_LE` words. Rate changes need the vendor relock
  sequence, which is not implemented.
- **BAR roles are decided in `motu424_hw.c`** (`motu424_assign_windows()`):
  `main.c` maps every BAR without interpreting them; the hardware layer picks
  window A (8 MB MMIO), window B (4 MB MMIO) and the I/O-port bridge by
  BAR type/size.
- **Lifetime is fully devm/pcim-managed** — there is intentionally no `.remove`
  callback. Teardown order matters: the shutdown action (stop, mask, DSP
  WARMRESET, which also ends its DMA) is registered *after* the IRQ request so
  it runs before the IRQ is freed and before the ring goes away. Preserve that
  ordering.
- **Locking:** `chip->lock` guards register access + stream state. The IRQ
  handler and the (atomic) PCM trigger both take it.

## Build & test

Requires kernel headers for the running kernel (Arch RT kernel:
`linux-rt-headers`). `clang` diagnostics in an editor are false positives — this
is kbuild-only code; trust `make`, not standalone clang.

```sh
make            # build module (kernel/) + userspace tools (tools/)
make module     # module only
make tools      # userspace tools (probe + motu424-ctl); builds without kernel headers
make load       # sudo insmod kernel/motu424.ko
make unload     # sudo rmmod motu424
make clean
```

There is no unit-test harness (kernel module). Verification is empirical:
`make load`, then check `dmesg` and `aplay -l` / `arecord -l`.

## Reverse-engineering workflow

`tools/motu424-probe.c` (plain userspace C, no kernel headers) scans
`/sys/bus/pci` for vendor `0x137A` and dumps BAR0 via `resource0`. Use it with
the driver **unbound** to locate registers by diffing idle vs. streaming dumps.
`tools/re/` holds the static-RE helpers (`vtable-scan.py`, `xref.py`). Record
confirmed offsets in `motu424.h` and the docs as they are established. Vendor ID
`0x137A` = Mark of the Unicorn; the register semantics recovered so far are
documented in `docs/`.
