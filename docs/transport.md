# Audio transport & ring (Phase 3.1 / 3.2)

How samples move between host and card. Rewritten on 2026-10-16 from
Audiosyncrasy's analysis of the DSP program and a Windows-configured card
state (issue #2, comment of 2026-10-15). VAs in `MOTUAW.sys` use image base
`0x10000`; DSP addresses are card addresses in DSP internal RAM.

## It is DSP bus-master DMA, not PIO (CONFIRMED)

The card's PCI interface is the PCI port of its TMS320C6412 DSP, and the DSP
is the PCI bus master. Playback data is pulled from **host memory** by the DSP
itself, through its own master port (DSPMA/PCIMA/PCIMC, START = 2,
prefetchable read), in pieces of a few hundred bytes. The FPGA sits on the
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
| `+0x60` | W | **frames per interrupt**: `0x40` at 1x, `0x80` at 2x/4x |
| `+0x54` | W | `1` = start, `0` = stop |
| `+0xf4` | R/W | stop acknowledge: host writes `0` before stopping, the DSP sets it non-zero once stopped |
| `+0x2c` | R | **position**: the host bus address the DSP has reached |

Start: `+0x5c ← ring`, `+0x8c ← size`, `+0x60 ← frames`, `+0x54 ← 1`.
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
`0x15000` bytes into the ring. The vendor reduces the offset modulo the ring
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

- **Bytes per frame on the ring.** The DSP side hints at 48 bytes (12
  dwords), not conclusively. The driver keeps it as the `frame_bytes` module
  parameter and refuses to stream while it is 0. A 512 KB ring is not a whole
  number of 48-byte frames, so either the size code is not the exact wrap
  point or the frame is not 48 bytes; a mid-stream dump of the ring should
  settle it.
- **Sample format and channel order** inside a frame. The driver exposes
  `frame_bytes / 4` raw 32-bit words (`S32_LE`) until that is known.
- **Capture.** Audio is DMA both ways, but where the DSP writes recorded
  audio on the host side (same ring, or a second address) is not mapped yet.
  The driver exposes no capture stream.
- **Rates other than 48 kHz**: `+0x80` (the 2x field sits at bits 27:24),
  `+0x88/+0x8c/+0x90` and `+0x70` per rate, through the relock sequence.
