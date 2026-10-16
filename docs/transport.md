# Audio transport & ring (Phase 3.1 / 3.2)

How samples move between host and card. Rewritten on 2026-10-16 from
Audiosyncrasy's analysis of the DSP program and a Windows-configured card
state (issue #2, comment of 2026-10-15). VAs in `MOTUAW.sys` use image base
`0x10000`; DSP addresses are card addresses in DSP internal RAM.

## It is DSP bus-master DMA, not PIO (CONFIRMED)

The card's PCI interface is the PCI port of its TMS320C6412 DSP, and the DSP
is the PCI bus master. Playback data is pulled from **host memory** by the DSP
itself, through its own master port (DSPMA/PCIMA/PCIMC, START = 2 =
host-to-DSP read; every master transfer in the program is a read, the DSP
never writes host memory), in pieces of a few hundred bytes. The FPGA sits on the
DSP's EMIF (not host-visible); the DSP moves audio between it and internal RAM
with QDMA. The host never writes PCM through a window.

The earlier "PIO into a window-B aperture" reading of this file was wrong. It
rested on `fn 0x29420` and the 28 KB block write at `fn 0x29500`, which are the
**FPGA loader and the DSP program upload** (`fpga-upload.md`), not an audio
push.

## Host ring (CONFIRMED shape)

The host allocates one coherent ring and hands its bus address to the DSP.
Windows uses **512 KB**; the separate 0x2800-byte common buffer
(`vendor-driver-map.md`, devext `+0x298`) is not the ring.

| Audio block off | Dir | Meaning |
|---|---|---|
| `+0x5c` | W | host ring **bus address** (vendor writes `[this+0x10]`, `0x29840`) |
| `+0x8c` | W | ring **size code** in bytes (`0x80000` = 512 KB on Windows) |
| `+0x60` | W | **frames per interrupt** as the host counts them (`0x80` on Windows at 96 kHz; the DSP loads it once in its tick and does not visibly use it) |
| `+0x64` | W | **periods per ring wrap**: the DSP's period counter wraps here and its pointer returns to `+0x5c` (`8` on Windows; `0` after bring-up, which would replay the first period) |
| `+0x54` | W | `1` = start, `0` = stop |
| `+0xf4` | R/W | stop acknowledge: host writes `0` before stopping, the DSP sets it non-zero once stopped |
| `+0x2c` | R | **position**: the host bus address the DSP has reached |

Start: `+0x5c ← ring`, `+0x8c ← size`, `+0x60 ← frames`, `+0x64 ← periods`,
`+0x54 ← 1`. The ring in use is `+0x64 × period_bytes` (`8 × 0x3000` = 96 KB on
Windows at 96 kHz), inside the 512 KB allocation; `+0x8c` is read by the DSP
only in its rate/mode path, not in the tick. The vendor's configure path
(`0x29930(a, b, c)`) also writes `+0x58 ← c`, `+0x68 ← a`, `+0x6c ← b` and a
24-dword channel map at `[+0x14]`; Windows had `a = 0xe`, `b = 0x10`, `c = 0`,
and the DSP's host-read size per pass is `0x30 × +0x68`.
Stop: `+0xf4 ← 0`, `+0x54 ← 0`, then poll `+0xf4 != 0`.

## Position (CONFIRMED — reader `fn 0x2a5c0`)

```
u32 fn_2a5c0(devext *this) {
    u32 raw = READ_REGISTER_ULONG(this->audio_base + 0x2c);  // DSP's bus address
    return (raw - this->ring_bus) >> 2;                      // dword offset in the ring
}
```

`[devext+0x10]`, read here as "playback aperture base", is the **ring's bus
address**, the same value the vendor writes to `+0x5c`. On a Windows-configured
card (96 kHz, playing) `+0x5c = 0xd9e20000` and `+0x2c = 0xd9e35000`, i.e.
`0x15000` bytes into the ring with the period counter at 7: seven periods of
`0x3000` bytes. The vendor reduces the offset modulo the ring
to find completed periods; the `ReadHead out of bounds` guard (`0x25500`)
checks the same dword offset against the ring length.

## Interrupts (CONFIRMED)

Once per `+0x60` frames the DSP stores its position to `+0x2c`, then sets
RSTSRC bit 3 (INTREQ), which raises PINTA#. The host:

1. tests HSR bit 1 (port BAR `+0x0`);
2. acks with RSTSRC bit 4 (INTRST): write `0x10` to card `0x01c00000`
   (window A `0x400000`);
3. signals period-elapsed.

The Windows ISR never reads `+0x2c`; position is read on demand. After a rate
change the ISR rewrites `+0x80` and `+0x88/+0x8c/+0x90` once a settle delay
has passed (`vendor-driver-map.md`, devext `+0x1f0`).

## Other DSP-side transport fields

- `+0x100` command: a chunked DSP-master copy of `+0x100` bytes from host
  address `+0xf8` to card address `+0xfc`; polling `+0x100` to 0 means "copy
  complete". The clock-relock routine (`0x2b7f0`) uses it to prime the DSP
  buffer from the host's common buffer.
- `+0x128..+0x130`: per-period statistics the host reads and clears.
- `+0x138/+0x13c`: firmware timing maxima (profiling); the host need not
  touch them.
- Window B `0x30000/0x32800` (`+0x0c/+0x10`): two 0x2800-byte DSP-side
  buffers; on the Windows-configured card each held 0x5b0 bytes of data.

## The Linux driver

`kernel/motu424_hw.c` follows this model: the ALSA buffer is the ring
(`SNDRV_DMA_TYPE_DEV`, 512 KB, 32-bit DMA mask), the trigger runs the start
and stop sequences above, `.sync_stop` waits for `+0xf4`, the IRQ handler
acks INTRST and calls `snd_pcm_period_elapsed()`, and `.pointer` returns
`(+0x2c - ring) / frame_bytes`.

Only playback at 48 kHz (1x, the state the program boots into) is exposed.
Rate changes go through the relock sequence, which is not implemented.

## Open

- **Bytes per frame on the ring.** The Windows dump gives `0x3000` bytes per
  period at `+0x60 = 0x80`, i.e. 96 bytes (24 dwords, one 24-channel frame
  of 32-bit samples) if `+0x60` counts frames; the DSP's `0x30`-granular
  transfer loop is a sub-unit of that. The period size is the DSP's own
  (`0x3000` at 2x; the 1x value is not observed yet). The driver keeps the
  frame size as the `frame_bytes` module parameter and refuses to stream
  while it is 0; a mid-stream dump of the ring should settle it.
- **Sample format and channel order** inside a frame. The driver exposes
  `frame_bytes / 4` raw 32-bit words (`S32_LE`) until that is known.
- **Capture.** Audio is DMA both ways, but where the DSP writes recorded
  audio on the host side (same ring, or a second address) is not mapped yet.
  The driver exposes no capture stream.
- **Rates other than 48 kHz**: `+0x80` (the 2x field sits at bits 27:24),
  `+0x88/+0x8c/+0x90` and `+0x70` per rate, through the relock sequence.
