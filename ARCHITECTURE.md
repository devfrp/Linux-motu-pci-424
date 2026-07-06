# Architecture & contributor notes

Design notes for anyone working on this driver. Read alongside `README.md`.

## What this is

A from-scratch **Linux ALSA kernel driver** for the MOTU PCI-324 / PCI-424
audio card, plus a userspace reverse-engineering toolset and a CueMix-style
control app. There is no upstream vendor documentation for this hardware.

## Critical context: the driver is a framework over unconfirmed hardware

The PCI/IRQ/ALSA plumbing is complete and correct, and the hardware layer now
implements the **model recovered by static RE of the vendor driver** (windowed
card-address space, I/O-port bridge, PIO-aperture ring — see `docs/`). What
remains unverified is flagged `TODO: verify`, and the **card-reported runtime
addresses** (audio base / IRQ ack) are unknown until dumped from a real card —
until then the card registers but streaming is refused (injectable via the
`motu424.audio_base=`/`ack_addr=` module parameters for bring-up). Everything
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
motu424_hw.c     Windowed card-address accessors, BAR→window assignment,
                 clock/rate encoding, PIO-aperture ring, IRQ ack/accounting.
                 THE hardware truth lives here.
```

Key facts an implementer must keep consistent:

- **Native format** is 24-bit samples packed 3 bytes/channel/frame
  (`MOTU424_BYTES_PER_SAMPLE`). Exposed to ALSA as `S24_3LE`.
- **Sample-rate families** (1x/2x/4x): the fixed frame's channel count shrinks
  as the rate rises. `motu424_hw_set_rate()` owns this mapping (period
  increment `0x10 << (2*family)`).
- **Transport is PIO, not bus-master DMA**: the host buffer is plain vmalloc
  memory (`SNDRV_DMA_TYPE_VMALLOC`); `motu424_hw.c` copies periods into the
  card's window-B aperture ring (`memcpy_toio`) and tracks software heads.
  The card is never handed a host address, and `main.c` never enables bus
  mastering.
- **BAR roles are decided in `motu424_hw.c`** (`motu424_assign_windows()`):
  `main.c` maps every BAR without interpreting them; the hardware layer picks
  window A (8 MB MMIO), window B (4 MB MMIO) and the I/O-port bridge by
  BAR type/size.
- **Lifetime is fully devm/pcim-managed** — there is intentionally no `.remove`
  callback. Teardown order matters: hardware is quiesced via a
  `devm_add_action_or_reset` registered *after* the IRQ request so it runs
  before the IRQ is freed. Preserve that ordering.
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
