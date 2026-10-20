# Audio transport & ring (Phase 3.1 / 3.2)

How samples move between host and card. Rewritten on 2026-10-16 from
Audiosyncrasy's analysis of the DSP program and a Windows-configured card
state (issue #2, comments of 2026-10-15 and 2026-10-16). VAs in `MOTUAW.sys` use image base
`0x10000`; DSP addresses are card addresses in DSP internal RAM.

## It is DSP bus-master DMA, not PIO (CONFIRMED)

The card's PCI interface is the PCI port of its TMS320C6412 DSP, and the DSP
is the PCI bus master. Playback data is pulled from **host memory** by the DSP
itself, through its own master port (DSPMA/PCIMA/PCIMC, START = 2, a
host-to-DSP read; 1 would be a DSP-to-host write), in pieces of a few hundred
bytes. Every PCIMC write in the program (playback fetch, the `+0x100` copy and
a third copy) uses START = 2: the firmware never writes host memory. The FPGA sits on the
DSP's EMIF (not host-visible); the DSP moves audio between it and internal RAM
with QDMA. The host never writes PCM through a window.

The earlier "PIO into a window-B aperture" reading of this file was wrong. It
rested on `fn 0x29420` and the 28 KB block write at `fn 0x29500`, which are the
**FPGA loader and the DSP program upload** (`fpga-upload.md`), not an audio
push.

## Host ring (CONFIRMED shape)

The host allocates one coherent ring and hands its bus address to the DSP.
Windows allocates **512 KB**; the separate 0x2800-byte common buffer
(`vendor-driver-map.md`, devext `+0x298`) is not the ring.

The DSP wraps at **`+0x64` periods**, not at the size code. In its period tick
(`0x2610..0x2674`) it counts periods, and when the count reaches `+0x64` it
resets the counter and sets both its running pointer and `+0x2c` back to
`+0x5c`. So the part of the ring in use is `+0x64 × period_bytes`: 8 ×
`0x3000` = 96 KB on Windows at 96 kHz, 8 × `0x1800` = 48 KB on Linux at
48 kHz (measured: `+0x2c - +0x5c` takes exactly the eight values
`0..0xa800`). `+0x8c` is read only once, at `0x2820`,
in the rate/mode path with `+0x88`/`+0x90`.

| Audio block off | Dir | Meaning |
|---|---|---|
| `+0x5c` | W | host ring **bus address** (vendor writes `[this+0x10]`, `0x29840`) |
| `+0x8c` | W | ring **size code** in bytes (`0x80000` = 512 KB on Windows); rate/mode path only |
| `+0x60` | W | **frames per interrupt** (`0x80` at 96 kHz on Windows); host bookkeeping, the DSP loads it once in the tick and does not appear to use it |
| `+0x64` | W | **periods per ring wrap** (`8` on Windows; `fn 0x13c30` uses `4`). `0` after bring-up, which makes the DSP replay the first period |
| `+0x58/+0x68/+0x6c` | W | fetch geometry from the vendor configure path `0x29930(a, b, c)`: `+0x68 ← a`, `+0x6c ← b`, `+0x58 ← c`, then the 24-dword channel map at `[+0x14]`. Windows: `a = 0xe`, `b = 0x10`, `c = 0`. The DSP's per-pass fetch is `0x30 × +0x68` bytes |
| `+0x54` | W | `1` = start, `0` = stop |
| `+0xf4` | R/W | stop acknowledge: host writes `0` before stopping, the DSP sets it non-zero once stopped |
| `+0x2c` | R | **position**: the host bus address the DSP has reached |

Start: `+0x58/+0x68/+0x6c` (and the channel map), `+0x5c ← ring`, `+0x8c ←
size`, `+0x60 ← frames`, `+0x64 ← periods`, `+0x54 ← 1`.
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

Once per period the DSP stores its position to `+0x2c`, then sets
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
  complete". The relock sequence (`0x2b7f0`, below) uses it to hand the DSP
  a mixer program from the host's 0x2800-byte common buffer.
- `+0x128..+0x130`: per-period statistics the host reads and clears.
- `+0x138/+0x13c`: firmware timing maxima (profiling); the host need not
  touch them.
- Window B `0x30000/0x32800` (`+0x0c/+0x10`): the DSP's two 0x2800-byte
  **mixer-program buffers**, not PCM. The Windows-configured card held two
  generations: 0x5bc bytes at `0x30000` and 0x4fc bytes at `0x32800`, the one
  in use (`+0x78 = 0x32800`).

## Device configure (issue #2, 2026-10-20)

Audiosyncrasy's reading of the 32-bit `MOTUAW.sys`: what the vendor writes
after bring-up beyond the stream start, and when. The RAM diff in
`register-map.md` shows all of it missing on Linux; the driver now writes it
(below), not yet run on a card.

- **Parameter group** (`+0x88/+0x8c/+0x90`, `+0x9c..+0xc0`, `+0xc4/+0xc8`,
  `+0xe0/+0xe4/+0xf0`, `+0x98`, `+0x114` commit): written **inside bring-up**
  (step 16, hw slot `0x30`: `0x2a820/0x2a710/0x2a9e0/0x2aab0`), after the
  mixer zero and before `+0x50 ← 1`; rewritten identically at the settings
  restore and on every rate change. The DSP routine that consumes `+0x114`
  (`0x45e0`) also reads `+0x80` and `+0xc4/+0xc8`; it presumably configures
  the clock (INFERRED: the missing lock).
- **Relock / `+0x100` sequence** (`0x2b7f0`): on channel-set or mixer
  changes (`0x27970`), on resume, on a meter-mode change, and in set-rate
  (`0x1b620`) when the 1x/2x/4x multiplier changes. Not part of a stream
  start. At driver start the registry restore calls set-rate with the saved
  rate, so a box saved at 96 kHz relocks at load and one saved at 1x does
  not.
- **Configure** (`0x26760`: channel map, `+0x58/+0x68/+0x6c`): at every
  stream start (`0x13b30`), then `+0x60`, `+0x64`, the ring, `+0x54 ← 1`.
- **Mailbox** (`0x2a190`): **metering only**, never needed for clock or
  audio. `0x24960` sends the channels the host has meters open for (the
  channel-map byte encoding, at most 0x30 bytes = hw slot `+0x34`),
  bank-selects with `+0x7c`, polls `+0x30 == 0` and reads 26 dwords of
  peaks; `0x24420` resets it. The lists are host-chosen. The interface-state
  area at `0x7648..` is presumably maintained by the DSP once the clock runs;
  nothing on the host writes it.

### The relock sequence

The host keeps a ping-pong index starting at 0; `n` is the dword count of
the mixer program.

1. `idx = 1 - idx`; `dst = READ(+0x0c + 4 × idx)` (the first call reads
   `+0x10` = `0x32800`);
2. `+0xf8 ← ` bus address of the 0x2800-byte coherent common buffer;
3. `+0xfc ← dst`;
4. `+0x100 ← n × 4`;
5. `+0x74 ← 1`;
6. poll `+0x100 == 0`, 20 µs apart, at most 10000 times;
7. `+0x74 ← 0`; on timeout, stop here;
8. poll `+0x108 != 0`, 5 µs apart;
9. `+0x108 ← 0`, `+0x78 ← dst`, `+0x70 ← (q << 16) | (u16)-q` with
   `q = 0x10000 / (mult × 0x1e0)` (1x `0x0088ff78`, 2x `0x0044ffbc`, the
   Windows value), `+0x104 ← 1`.

The DSP copies the program from `+0xf8` to `+0xfc` in chunks, reading host
memory only. On `+0x104` its main loop takes `+0x78` as the mixer-program
pointer, clears `+0x104`, and raises `+0x108` when the next swap is safe.

### The mixer program

The CueMix **mixer program** is built by the host (`0x28d30`) from its own
mixer model: variable-length records ending with the trailer `0x001c00e2
0x079803e2 0x00008000`. The serialiser stops at 0x9f5 dwords, so 0x2800
bytes is the buffer size. It is neither a constant nor a mailbox response.
Before this, Linux never handed the DSP one (`+0x78 = 0`), the second
candidate for the silence. The record format is not decoded yet, so the
driver can only replay a program saved from a Windows-configured card.

## The Linux driver

`kernel/motu424_hw.c` follows this model: the ring is a `SNDRV_DMA_TYPE_DEV`
allocation of 512 KB (32-bit DMA mask), and the ALSA buffer is the first
`8 × period_bytes` of it (`+0x64 = 8`, `period_bytes` a module parameter,
`0x1800` by default; `+0x60` is the `irq_frames` parameter, `0x40` by
default). The trigger runs the start and stop sequences above, `.sync_stop`
waits for `+0xf4`, the IRQ handler acks INTRST and calls
`snd_pcm_period_elapsed()`, and `.pointer` returns `(+0x2c - ring) /
frame_bytes`.

The device configure follows the write list Audiosyncrasy gave for 48 kHz
with a 2408mk3 and a 24I/O (issue #2, 2026-10-20):

- `motu424_hw_init()` writes the parameter group between the mixer zero and
  `+0x50 ← 1`, in the vendor's order (`register-map.md`). Half the values
  derive from the firmware constants `+0x44/+0x48`, so it is skipped with a
  warning unless they read `0x03a89000/0x0009c180`. `+0x80` keeps the
  firmware's own `0x800f0200`; the vendor writes `0x80000200` there at the
  first bring-up only, and leaving it is the conservative choice.
- If `/lib/firmware/motu424/pci424-mixer.bin` exists, probe hands it to the
  DSP with the relock sequence once, after bus mastering is on. It must be a
  multiple of 4 bytes, at most 0x2800, and end with the trailer above. It is
  never shipped. Without it, the DSP runs with no mixer program, as before.
- The stream start writes the channel map before `+0x58/+0x68/+0x6c`:
  `0x191a1b1c 0x1d1e1f20 0x21222324 0x25260000` and 20 zeros. These are the
  14 (= `+0x68`) playback channels 24..37, one byte each as `ch + ch/24` at
  byte index `i ^ 3`, as the Windows image held them at `0x8348`. `+0x6c =
  0x10` is the input count.
- `configure=0` skips the parameter group and the channel map.

Only playback at 48 kHz (1x, the state the program boots into) is exposed.
Rate changes go through the relock sequence with a different `+0x70` and the
parameter group per rate, not implemented.

## Open

- **Bytes per frame: 96 (CONFIRMED).** Measured on a Linux stream two ways:
  752 interrupts/s × `0x1800` bytes = 96 bytes × 48 000 frames/s, and the
  DSP staging buffers held one sine sample every 96 bytes. The Windows dump
  gives the same (750/s × `0x3000` at 96 kHz). The driver still keeps it as
  the `frame_bytes` parameter, 0 by default, until the outputs play.
- **Period bytes vs `+0x60`.** `+0x60 × frame_bytes = period_bytes` held in
  both observations (`0x40 × 96 = 0x1800` at 48 kHz, `0x80 × 96 = 0x3000` at
  96 kHz). Whether `+0x60` sets the period or the DSP picks it per rate is
  not split yet: a 48 kHz run with `irq_frames=0x80` does it (half the
  interrupt rate and `0x3000` steps in `+0x2c` if `+0x60` drives it).
- **Device configure (blocked audio; now written, not yet run).** A 48 kHz
  Linux stream ran cleanly, but nothing reached the outputs and the
  interfaces never locked. The parameter group, the mixer program and the
  channel map were the missing pieces (Device configure, above); the driver
  now writes all three. Pass criterion: the 2408mk3 and 24I/O lock after
  the load, before any stream. Loading once without and once with
  `pci424-mixer.bin` separates the two candidates (parameter group alone vs.
  mixer program).
- **Channel map for other interface sets.** The driver writes the one
  Windows used for a 2408mk3 + 24I/O. How the vendor derives it from the
  connected boxes is not traced.
- **Mixer program format.** Records ending with a fixed trailer, not
  decoded; a default program for a clean state would remove the need to
  save one from Windows.
- **Sample format and channel order** inside a frame. The driver exposes
  `frame_bytes / 4` raw 32-bit words (`S32_LE`) until that is known.
- **Capture.** Audio is DMA both ways, but where the DSP writes recorded
  audio on the host side (same ring, or a second address) is not mapped yet.
  The driver exposes no capture stream.
- **Rates other than 48 kHz**: `+0x80` (the 2x field sits at bits 27:24),
  `+0x88/+0x8c/+0x90` and `+0x70` per rate, through the relock sequence.
