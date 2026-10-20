# Register map (Phase 1.1)

Card-address constants recovered from static RE of `MOTUAW.sys`. VAs use image
base `0x10000`. **Confidence is tagged per row.** Only rows marked CONFIRMED are
safe to encode as facts; INFERRED rows are strong hypotheses; OPEN rows are gaps.

## Access model (CONFIRMED — accessor VAs `0x29110`/`0x29160`)

A 32-bit *card address* is dispatched, then added to a per-window mapped base:

```
if ((cardAddr & 0xff800000) == 0x01800000)   // window A
    mmio = base_A + (cardAddr & 0x007fffff);  // 8 MB, dev-ext +0x7c
else                                          // window B
    mmio = base_B + (cardAddr & 0x003fffff);  // 4 MB, dev-ext +0x78
READ_/WRITE_REGISTER_ULONG(mmio, ...)         // little-endian 32-bit
```

Window A is the DSP's **peripheral register space** (card `0x01800000..`:
EMIF, GPIO, PCI/RSTSRC, EEPROM controller); window B is the **DSP memory page**
selected by DSPP, with internal RAM at `0..0x3ffff` when DSPP = 0. The rest of
page 0 is reserved on the C6412 and is never accessed. (An earlier reading of
A and B as two aliases of one bank space is superseded.)

## I/O-port BAR (CONFIRMED — dev-ext `+0x80`)

A small port window (`READ_/WRITE_PORT_ULONG`), base stored at dev-ext `+0x80`.
It is the **TMS320C6412 Host Port Interface** register set (SPRU581C), checked
on hardware (issue #2):

| Port off | Register | Meaning |
|---|---|---|
| `+0x0` | HSR | bit 1 INTAVAL = interrupt pending (ISR `0x2baf0`); bit 2 INTAM masks PINTA# when set |
| `+0x4` | HDCR | bit 0 WARMRESET; bit 1 DSPINT releases the CPU after the program load |
| `+0x8` | DSPP | window-B page; `0` = DSP internal RAM |

## Bank register file (SUPERSEDED)

The "banks" at window-B `0x000c0000` / `0x00100000` (`bank+0x24`, bit 6) are
the **PCI-324** (`DEV_0003`) FPGA-load path (`fpga-upload.md`), not a PCI-424
register file. On the PCI-424 those window-B offsets fall in reserved C6412
space and the driver no longer reads them.

## Sample-rate *values* — NOT addresses (CONFIRMED)

Earlier notes listed `0xAC44` as a card address. **It is not** — `0xAC44` =
44100 decimal, a **sample-rate constant**. All six standard rates appear as
literal Hz immediates used in divisor/period math (`imul`, compares):

| Hex | Decimal | Family |
|---|---|---|
| `0xAC44` | 44100 | 1x |
| `0xBB80` | 48000 | 1x |
| `0x15888` | 88200 | 2x |
| `0x17700` | 96000 | 2x |
| `0x2B110` | 176400 | 4x |
| `0x2EE00` | 192000 | 4x |

The base-rate select is computed as `(sel ? 0 : 0x0f3c) + 0xac44` where
`0x0f3c` = 3900 = 48000−44100 (VA `0x147f8`, `0x16f83`, `0x1b63c`), i.e. a
1-bit "44.1 vs 48 kHz base" flag, multiplied out by the 2x/4x family.

## Audio register block (CONFIRMED via vtable RE — class vtable `0x30cc0`)

The hardware-object C++ class (vtable at `.rdata` `0x30cc0`) holds an **audio
register base as a card address in device-ext `+0x98`**, read from the card at
init (from a per-bank sub-object field `+0x30`; method `0x2c360`, and it must be
non-zero or init fails with error 5). All audio registers are offsets from that
base and go through the same window dispatch:

| Reg (base+off) | Dir | Method VA | Meaning (recovered) |
|---|---|---|---|
| `base+0x0` | R | init `0x2c425` | read at init; mirror gets stored in devext `+0x9c` (**mix base** — CONFIRMED on hardware, issue #2: `audio_base+0` = `0x8290`, a 45-dword block; `+0x14` = mix base `+0xb8`) |
| `base+0x4` | R | init `0x2c45b` | read at init; mirror gets stored in devext `+0xa0` (purpose UNKNOWN) |
| `base+0x8` | R | init `0x2c491` | read at init; mirror gets stored in devext `+0xa4` (purpose UNKNOWN) |
| `base+0x14` | R | init `0x2c4c7` | read at init; mirror gets stored in devext `+0xa8` (purpose UNKNOWN) |
| `base+0x18` | R | init `0x2c4fd` | read at init; mirror gets stored in devext `+0xac` (purpose UNKNOWN) |
| `base+0x34` | W | init `0x2c615` | initial register value from `[A+0x34]` (written via `audio_base+0x8`'s sibling during bring-up) — purpose OPEN |
| `base+0x50` | W `1` | init `0x2c5ff` | bring-up **enable** at end of the init routine (`0x2c5d3` address) |
| `base+0x54` | W `1` | `0x29660` (slot 1) | stream/DMA **enable** (writes 1 when arg≠0) |
| `base+0x5c` | W | `0x29840` | **host ring bus address** (writes `[this+0x10]`; `0xd9e20000` on a Windows-configured card) — see `transport.md` |
| `base+0x58` | W | `0x29930` | configure arg `c` (`0` on Windows) |
| `base+0x60` | W | `0x295e0` (slot 0) | **frames per interrupt** = `0x10 << (2*arg)` (×2 if `[+0x1cc]>1`); `0x80` on the card at 96 kHz (arg 1, doubled). The arg is not derived from the rate family: `fn 0x13c30` passes 2, the 96 kHz path 1. Host bookkeeping: the DSP loads it once in its tick and does not appear to use it |
| `base+0x64` | W | `0x297f0` (slot 2) | **periods per ring wrap**: the DSP's tick resets its pointer and `+0x2c` to `+0x5c` when its period counter reaches it (`0x2610..0x2674`). `8` on Windows, `4` from `fn 0x13c30`; `0` after bring-up |
| `base+0x68` | W | `0x29930` | configure arg `a` (`0xe` on Windows); the DSP's per-pass host fetch is `0x30 × +0x68` bytes |
| `base+0x6c` | W | `0x29930` | configure arg `b` (`0x10` on Windows); host-read geometry |
| `base+0x80` | W | `0x2a820` (`0x2a8c2`) | **rate/mode register**: `lookup_table[rate_mode] (at 0x30c74)` OR `[+0x1d4]` (high-bit sentinel) OR `(0x10000 or 0xf0000` based on `[+0x1ec]`); also influenced by `+0x1ec` `OR` mask `0xfff10000` |
| `base+0x88` | W | `0x2a710` (`0x2a7a2`) | **period buffer size** (e.g. `0x80000` default, `0x40000` for rate class 2, scaled for 2x/4x families) |
| `base+0x8c` | W | `0x2a710` (`0x2a7d7`) | **host ring size code** in bytes (computed per rate family: `0x80000`, `0x40000`, or `1 << (rate-8+0xe)` if `[+0x1d0]` bit set, else `0x20000`); `0x80000` = 512 KB on the Windows-configured card. The DSP reads it once (`0x2820`) in the rate/mode path; it is **not** the wrap point (that is `+0x64`) |
| `base+0x90` | W | `0x2a710` (`0x2a803`) | secondary period value `[ebp-4]` (purpose OPEN; zero-init default) |
| `base+0x9c` | W | `0x2aab0` (`0x2ab08`) | rate-state write: `([+0x280]+0x32)/100` (the CueMix base mirror's +0x9c is read at init; this writes the audio-block register) — also written by `+0x288` path |
| `base+0xac` | W | `0x2a820` (`0x2a88c`) | **transport-arm length**: `(rate_mode-4)*25 + 24` (only written when 4 ≤ rate_mode < 8) |
| `base+0xc4` | W | `0x2a9e0` (`0x2aa50`) | rate-state write: `[+0x284]` (scaled by `[+0x44]==0 ? 0x1b9 : 0x1e0` then `/25` via `0xcccccccd`) |
| `base+0x128` | R→W`0` | ISR `0x2bae0` | position counter; read each period, then zeroed |
| `base+0x12c` | R→W`0` | ISR `0x2bae0` | numerator; read, divided by `base+0x130`, then zeroed |
| `base+0x130` | R→W`0` | ISR `0x2bae0` | divisor; read then zeroed |

The **ack register is a separate card address in device-ext `+0x88`** (from the
same sub-object, field `+0x4`; method `0x2c150`). On the PCI-424 it is the
DSP's **RSTSRC** register, card `0x01c00000` (window A `0x400000`): the DSP
raises PINTA# with RSTSRC bit 3 (INTREQ) and the host's `0x10` is bit 4
(INTRST), which clears it.

## The audio block is the DSP program's global data (CONFIRMED, issue #2)

From Audiosyncrasy's analysis of the 28,272-byte C64x program (linked at 0,
little-endian) and a read of a card warm-rebooted from Windows (2026-10-15).
The program's data page B14 is `0x6e70` and `audio_base = 0x6fac` = B14 +
`0x13c`, so this block is the firmware's global data. Its init routine writes
exactly the values the first Linux bring-up read (`mix_base 0x8290`, `+4
0x87a0`, `+8 0x7988`, `+0x14 0x8348`, `+0x18 0x7240`, `+0x80 0x800f0200`,
`+0x98 0x05355555`, `+0x108 1`).

State left by Windows (96 kHz, interfaces connected, audio playing), read
after a warm reboot into Linux. DSP RAM survived it: the mailbox still read
`0x6fac` and the program text was byte-identical to the extracted image.

| Off | Value | Meaning |
|---|---|---|
| `+0x2c` | `0xd9e35000` | DSP position: a host bus address |
| `+0x5c` | `0xd9e20000` | host ring base (written by the host); `+0x2c` is `0x15000` past it |
| `+0x60` | `0x80` | frames per interrupt |
| `+0x64` | `8` | periods per DSP counter wrap |
| `+0x70` | `0x0044ffbc` | `q<<16 \| -q`, `q = 65536/(480*2)`: multiplier 2 |
| `+0x80` | `0x8f001200` | rate/mode at 2x; the 2x field is bits 27:24 |
| `+0x8c` | `0x80000` | ring size code: 512 KB |
| `+0x98` | `0x05355555` | still 48000: the base rate; the multiplier lives elsewhere |
| `+0xb0..+0xf0` | as predicted | every formula fits with rate 48000, multiplier 2 |
| `+0x44/+0x48` | as after the first bring-up | constants, not measured clock counts |
| `+0xf4` | `1` | DSP stop acknowledge |
| `+0x128..+0x130` | `0xa4 / 0x2f94 / 0x5b` | live per-period statistics |

Window B also held the two 0x2800-byte buffers at `0x30000/0x32800`, each
with 0x5b0 bytes of data. Window A's head read EMIF GBLCTL `0x00010ffc`, then
CECTL1 = CECTL0 = `0x10914221` (32-bit asynchronous CE timing, the "init word"
the vendor writes to window A `+0x8`).

The DSP forwards `+0x80 >> 8` to an FPGA control register whenever the
rate/mode word changes. `+0x138/+0x13c` are firmware timing maxima.

### What the vendor writes after bring-up (RAM diff, issue #2, 2026-10-17)

Audiosyncrasy diffed the data page (`0x6e70..0x9000`) of the Windows image
above against the Linux image after a 48 kHz stream on `4cfb714`
(`frame_bytes=96 period_bytes=0x1800`): the stream ran, but no output played
and neither interface locked. Program text is identical. Leaving out live
counters, bus addresses and the rate-dependent `+0x60`/`+0x80`, every
difference is a value the vendor writes after bring-up and the Linux driver
does not:

| Off / address | Windows (96 kHz) | Linux | Written by |
|---|---|---|---|
| `+0x70` | `0x0044ffbc` | 0 | relock sequence `0x2b7f0` |
| `+0x78` | `0x00032800` | 0 | relock sequence |
| `+0x88` | `1` | 0 | parameter group |
| `+0x9c/+0xa0/+0xa4` | `0x1e / 0x708 / 0x4650` | 0 | parameter group |
| `+0xb0..+0xbc` | `0x1e / 0xcccd / 0x492b4000 / 0x640` | 0 | parameter group |
| `+0xdc/+0xe0/+0xe4` | `0x7fff / 0x4e0c0 / 0xc31e00` | 0 | parameter group |
| `+0xf0` | `0x223` | 0 | parameter group |
| `+0xf4` | `1` | 0 | stop acknowledge / parameter group |
| `+0xf8/+0xfc` | host address / `0x32cfc` | 0 | `+0x100` copy (host source, card destination) |
| `+0x114` | `1` | 0 | commit of the parameter group |
| `0x7648..0x7984` | live box status | `ff000000` defaults | presumably the DSP once the clock runs; no host writer |
| `0x7c68..0x7c84`, `0x7ca4..0x7cb8` | channel lists `0x27..0x2e`, `0x10..0x17`, `0x19..0x26` | 0 | metering requests `[+0x24]/[+0x28]` (`0x24960`, host-chosen; metering only) |
| `0x8348..0x8355` (`[+0x14]`) | 14 bytes `0x19..0x26` (14 = `+0x68`) | 0 | channel map, configure step `0x26760` at stream start |
| `0x8d40..0x8e04`, `0x8ff0..0x8ffc` | structured non-zero tables | 0 | not attributed yet |

Identical on both, so set by the firmware's own init: the 100-byte routing
table at `0x8738` (unity `0x40`) and the `0x4000` gain run at `0x7184`. The
45-dword mixer, the `m4` gain pairs, the `m8` flags and the `m18` deltas are
zero on Windows too. The order and arguments the vendor uses for these writes
are in the write list below and in `transport.md` (Device configure).

### Which offsets the DSP program loads and stores (`--fields`, issue #2)

Audiosyncrasy's `tools/re/dis-c64x.py --fields --dp 0x6e70 --ab 0x6fac` over
the program, after the load/store fixes of PR #9: 211 access sites at 93
offsets (summary attached to issue #2, 2026-10-17). The fixes corrected the
offset of eight byte and halfword sites and left the others unchanged:

| Site | Access | Corrected offset |
|---|---|---|
| `0x6e4c` | `ldh` | `+0xb6`, the upper half of `+0xb4` |
| `0x6d58` | `stb` | `0x6eab` = `-0x101`, below the block |
| six byte sites | `ldb`/`stb` | `0x8730` (`+0x1784`, 2 loads, 2 stores), `0x8abc` (`+0x1b10`, store), `0x969b` (`+0x26ef`, load); all previously placed far too high |

Coverage: only accesses relative to the data page (`*+B14[n]`). A word the
DSP reaches through a pointer register is missing, e.g. the live
`+0x128..+0x130` statistics, which only the init routine stores relative to
B14. An offset absent here is therefore not proof the DSP ignores it. The data
page starts where the 28,272-byte (`0x6e70`) image ends, and the host zeroes
RAM before the load, so nothing in it is preloaded.

Routine addresses are the tool's heuristic starts. `0x1b8c` is the init
routine: it stores every value the first bring-up read (`+0x0`, `+0x4`,
`+0x8`, `+0x14`, `+0x18`, `+0x80`, `+0x98`, `+0x108`). `0x2140` holds the
period tick (`0x2610..0x2674`) and the `+0x8c` read at `0x2820`.

| DSP access | Offsets from `audio_base` |
|---|---|
| loads only | `+0x58..+0x70`, `+0x78`, `+0x84..+0x90`, `+0x9c..+0xf0` (incl. `+0xb6`, 16-bit), `-0x13c` (16-bit), `-0xd0`, `+0x26ef` (8-bit) |
| stores only | `+0x0`, `+0x4`, `+0x8`, `+0x14`, `+0x18`, `+0x2c..+0x34`, `+0x3c..+0x48`, `+0xf4`, `+0x108`, `+0x11c..+0x134`, `-0x2c..-0x20`, `-0x101` (8-bit), `+0x10e0`, `+0x1394`, `+0x1b10` (8-bit) |
| both | `+0x38`, `+0x4c`, `+0x54`, `+0x74`, `+0x7c`, `+0x80`, `+0x94`, `+0x98`, `+0xf8..+0x104`, `+0x10c..+0x118`, `+0x138`, `-0x1c..-0x10`, `+0x1774..+0x1784` (`+0x1784` 8-bit) |
| none (B14-relative) | `+0xc`, `+0x10`, `+0x1c..+0x28`, `+0x50`, `+0x13c` |

What follows for the configure path:

- Every word of the parameter group the RAM diff shows missing on Linux
  (`+0x88`, `+0x9c..+0xa4`, `+0xb0..+0xbc`, `+0xdc..+0xe4`, `+0xf0`) and the
  relock words `+0x70`/`+0x78` is load-only, and all of them read 0 on Linux
  after a stream: the host has to write them. The DSP also stores `+0x114`
  (routine `0x45e0`) and `+0xf8..+0x100`, so those are not write-once
  parameters.
- `+0xf4` is store-only: the DSP publishes the stop acknowledge and never
  reads back the host's `+0xf4 ← 0` before a stop.
- `+0x84`, `+0xa8`, `+0xc0`, `+0xc8..+0xd8`, `+0xe8`, `+0xec` are load-only
  too and read the same in the Windows and Linux images. Resolved by the
  write list below: the vendor writes `0` to `+0x84/+0xa8`, `+0xc0`, `+0xc8`
  and `+0xcc..+0xd4`; `+0xd8`, `+0xe8`, `+0xec` and `+0xac` (clock sources
  4..7 only) stay unwritten at 48 kHz.
- `+0x34` is store-only on the DSP side and, per the write list, stays
  unwritten on Windows at 48 kHz too. `+0x50` (the vendor's end-of-bring-up
  `1`) has no B14-relative access at all. OPEN.

### The vendor's 48 kHz configure writes (issue #2, 2026-10-20)

Audiosyncrasy's write list from the 32-bit `MOTUAW.sys`, for 48 kHz at 1x
with the constructor defaults (internal clock, latency 0, P = Q = 3000) and
the firmware constants K1 = `+0x44` = `0x03a89000`, K2 = `+0x48` =
`0x0009c180` (bring-up run 1). It runs inside bring-up, after the mixer zero
and before `+0x50 ← 1`; `transport.md` (Device configure) has where the
other pieces run. Every derived value below reproduces from its formula.

| # | Word | Value | Formula / note |
|---|---|---|---|
| 1 | `+0x80` | `0x80000200` | clock-source index 0, 1x; first bring-up only. The driver keeps the firmware's `0x800f0200` |
| 2 | `+0x88` | `1` | |
| 3 | `+0x8c` | `0x80000` | ring size code |
| 4 | `+0x90` | `0` | |
| 5 | `+0xc4` | `0` | latency, `ms × rate / 1000` |
| 6 | `+0xc8` | `0` | |
| 7 | `+0x9c` | `0x1e` | `(Q + 50) / 100` |
| 8 | `+0xa4` | `0x4650` | `6Q` |
| 9 | `+0xa0` | `0x708` | `6Q / 10` |
| 10 | `+0xb0` | `0x1e` | `Q / 100` |
| 11 | `+0xb4` | `0xcccd` | `(8P × 2^20 / rate + 5) / 10` |
| 12 | `+0xb8` | `0x492b4000` | `K1 × rate × 10 / 8P` |
| 13 | `+0xbc` | `0x640` | `rate × 100 / P` |
| 14 | `+0xc0` | `0` | |
| 15 | `+0xe0` | `0x9c180` | `K2`, halved at 2x/4x (`0x4e0c0` on Windows at 96 kHz) |
| 16 | `+0xe4` | `0xc31e00` | `(K2 × rate × 10 + 4P) / 8P` |
| 17 | `+0xf0` | `0x223` | `0x40000 / (K2 × rate / 64e6)` |
| 18 | `+0x114` | `1` | commit |
| 19 | `+0x98` | `0x05355555` | then rows 5-18 again |
| 20 | `+0x84`, `+0xa8` | `0`, `0` | internal clock; then `+0x114 ← 1` again |
| 21 | `+0x118` | `1` | the DSP clears it |
| 22 | `+0xcc`, `+0xd0`, `+0xd4` | `0` | clock-source words |
| 23 | `+0xdc` | `0x7fff` | restored user setting |

Then `+0x50 ← 1`, CECTL0, and the relock sequence with the mixer program
(`transport.md`). At every stream start the configure step adds the
channel map at `[+0x14]` to the fetch geometry.

## Rate-mode lookup table (CONFIRMED — rdata at VA `0x30c74`)

The transport-arm routine `0x2a820` writes `audio_base+0x80` with
`lookup[rate_mode] OR [+0x1d4] OR (conditional 0x10000/0xf0000)`, where
`lookup = dword[rate_mode*4 + 0x30c74]`. The first 16 entries (recovered via
`pefile`/`struct.unpack`):

| idx | value | idx | value |
|---|---|---|---|
| 0 | `0x00000200` | 8 | `0x00000400` |
| 1 | `0x00000200` | 9 | `0x00000500` |
| 2 | `0x00000800` | 10 | `0x00000600` |
| 3 | `0x00000201` | 11 | `0x00000700` |
| 4 | `0x00000202` | 12 | `0x0002c0a0` |
| 5 | `0x00000202` | 13 | `0x0002b5e0` |
| 6 | `0x00000202` | 14 | `0x000292b0` |
| 7 | `0x00000202` | 15 | `0x00029220` |

Indices 0–11 carry small mode/flag dwords; 12–15 look like card-relative
addresses or function pointers — purpose of the upper slots is OPEN.

## Mixer coefficient region (CONFIRMED via the bulk-write destinations)

A **third card-reported base lives in device-ext `+0x9c`**: at init (`fn 0x2c360`,
store at `0x2c427`) the driver `READ_REGISTER`s a location off the audio base and
keeps the returned card address there — same pattern as `+0x98`/`+0x88`. The
**CueMix mixer coefficients** are staged in an inline object buffer at `+0x110`
(~45 dwords) and flushed to `+0x9c` with `WRITE_REGISTER_BUFFER_ULONG`
(`fn 0x29aa0`, site `0x29ad5`), writing only the **dirty range** `[+0x1c4]..[+0x1c8]`
(classic mixer incremental update). This is the hardware sink for the CueMix
control set — see `cuemix-control-map.md`. Full destination table for all six
`WRITE_REGISTER_BUFFER_ULONG` sites (DSP program / channel-bank / mixer) is in
`fpga-upload.md`.

## IRQ path (CONFIRMED — ISR `0x2bae0` = vtable `0x30cc0` slot `0x28`)

The ISR shim (`0x201f0`) forwards to the virtual ISR `0x2bae0`:

```
pending = (READ_PORT(port_base + 0x0) >> 1) & 1;   // port BAR bit 1
if (!pending) return FALSE;                         // not ours
WRITE_REGISTER([devext+0x88], 0x10);                // ACK = write 0x10
acc = (devext[0x264] += devext[0x260]);             // += period increment
if (acc >= 0x800) {                                 // period boundary (2048)
    devext[0x264] = 0;
    pos = READ_REGISTER(base+0x128); WRITE_REGISTER(base+0x128, 0);
    div = READ_REGISTER(base+0x130);
    q   = div ? READ_REGISTER(base+0x12c)/div : 0;  WRITE_REGISTER(base+0x12c,0);
    WRITE_REGISTER(base+0x130, 0);
    devext[0x26c] = max(devext[0x26c], pos);
    devext[0x268] = max(devext[0x268], q);
}
... countdown devext[0x1f0] -= devext[0x260]; on <=0 call 0x2a820/0x2a710 ...
call [[devext+0x38]]+0;                              // queue DPC (period_elapsed)
return TRUE;
```

So: **IRQ pending = port BAR `+0x0` bit 1; ACK = write `0x10` to RSTSRC
(device-ext `+0x88`); the DPC signals period-elapsed on every interrupt, and the
`0x800` accumulator only paces the read-and-clear of the `+0x128..+0x130`
statistics.** Position is not read in the ISR; it is computed on demand from
`base+0x2c` (`transport.md`).

## Port BAR writes in the vendor driver (CONFIRMED)

| Port off | Value | Evidence | Meaning |
|---|---|---|---|
| `+0x0` | `4` | `0x298f3` | HSR INTAM: mask PINTA# |
| `+0x0` | `0` | bring-up step 4 | unmask PINTA# |
| `+0x4` | `1` | `0x29906`, `0x2c3dc` | HDCR WARMRESET |
| `+0x4` | `2` | bring-up step 4 | HDCR DSPINT |
| `+0x8` | `0` | `0x2b6c8` | DSPP = 0 |

Method `0x298e0` (`HSR ← 4`, then `HDCR ← 1`) is the **stop/reset** sequence,
not a start.

## Rate mapping (PARTIAL)

- **Superseded (2026-10-16):** the `+0x60` argument is not the rate family
  (`fn 0x13c30` passes 2, the 96 kHz Windows path 1), and `+0x64` is periods
  per ring wrap, not a rate mode (DSP tick `0x2610..0x2674`). The notes below
  on `+0x64` as a rate/mode index are kept for history only.
- ~~**family** ∈ {0,1,2} = {1x, 2x, 4x} drives `period_increment = 0x10 <<
  (2*family)` → `base+0x60` (16 / 64 / 256, doubled in one mode).~~
- **base rate** is validated as **44100 or 48000 only** (`0x191a0`; else logs
  error `0x4c5`); the 2x/4x multiply is applied on top.
- **Resolved (2026-07-06):** `base+0x64` selects the internal **rate mode**
  (see the `[obj+0x3e]`/`fn 0x11330` note below); there is **no clock-SOURCE
  register in MOTUAW.sys** — external-source (word/ADAT/SPDIF) selection is a
  higher-layer / CueMix concern, not an audio-base register. Only the exact
  enum→dword value written to `base+0x64` per rate remains (needs a card).

**New lead (rate-classification fn `0x11320`, no card).** Before the register
writes, the driver classifies the requested Hz into per-object state bytes:
- `[obj+0x3a]` = base-rate **class**: `3` for the 44.1 family
  (44100/88200/176400 = `0xac44`/`0x15888`/`0x2b110`), `4` for the 48 family
  (48000/96000/192000). Matches the "44.1 vs 48 base" flag above.
- `[obj+0x3e]` = an **11-way mode enum** (0..0xa), consumed via a jump table at
  `0x11420` — the strongest candidate for the internal/word/ADAT/SPDIF +
  1x/2x/4x **clock-source/rate-mode selector**. Mapping each enum value to the
  concrete `base+0x64` dword needs the virtual dispatch traced or a card.

**Partial trace (via `tools/re/xref.py`, LOW confidence).** A transport-arm
routine `fn 0x13c30` (12 callers) dispatches, on a hardware object held at
`driver+0x16884`: slot `0x0`(increment/family) ← **2**, slot `0x8`(param →
`base+0x64`) ← **4**, then slot `0x4`(enable) ← **1**, and calls a DMA-setup
`0x13000(…, 0x10, 1, 4)`. *If* that object is the vtable-`0x30cc0` class, this
means `base+0x64` is a **small integer** (here `4`) programmed next to the family
and enable — i.e. a mode/divider index, not a raw Hz value. **Caveat:** the
`0x30cc0` object built at `fn 0x2b610` is only a `0x50`-byte allocation, which
cannot hold the `[this+0x98]`/`[this+0x260]` fields the setters read — so either
there are multiple hardware sub-classes sharing the vtable shape, or the
setter⇄object mapping is not what it appears. Resolving this reliably needs
proper type recovery (Ghidra) or a card to observe the actual `base+0x64` writes.
Treat the "`+0x64` = small mode index" reading as a hypothesis, not a fact.

## DSPP (port `+0x8`)

The window-B page select. The bring-up (`fn 0x2c150`) writes `0` before the
program load; audio does not go through window B (`transport.md`), so there is
no per-direction aperture paging.

## Genuinely open (needs a card or more RE)

- ~~The numeric values of the audio base, ack address and aperture bases.~~
  Resolved on hardware: `audio_base` comes from the mailbox (`0x6fac` with the
  current program), the ack is RSTSRC, and there are no apertures (the ring is
  host memory, `transport.md`).
- Clock-**source** select register and the `base+0x64` parameter encoding: the
  setter (`0x297f0`, vtable slot 0x8) writes a caller-supplied dword with no
  encoding of its own; the value comes from the `[obj+0x3e]` 11-way enum
  (jump table `0x11420`) / the `fn 0x13c30` arm chain (observed value `4` for
  the 4x family) — resolving the full enum→dword table needs the virtual
  dispatch traced deeper or a card.
  **Update 2 (2026-07-06): the `base+0x64` value is interface-supplied runtime
  data — NOT a static rate→dword table.** The generic arm routine `fn 0x13b30`
  writes `base+0x64` from the state field `devext+0x1a2d0`; that field's writers
  are (a) a setter `fn 0x14ff0` storing a caller-supplied dword (`0x15018`),
  (b) `0x199e7` storing the constant `4`, and (c) `0x1ba10` storing the result
  of a **virtual call, interface-object vtable slot 0x6c** — i.e. the value is
  *queried from the connected breakout/interface*. The fixed-family-2 arm
  `fn 0x13c30` hardcodes increment-arg `2` alongside `base+0x64 = 4`, so the
  driver's `param = 2*family` heuristic matches the one static constant
  (family 2 → 4). The exact per-rate/per-interface dword can only be observed on
  a card. **This closes the static +0x64 question: it is card/interface-bound.**

  **Update (2026-07-06): the `[obj+0x3e]` enum is the RATE-mode index, not a
  clock source.** Classifier `fn 0x11330` reads it and its jump table `0x11420`
  maps each value straight to a standard Hz immediate (`0xac44`/`0xbb80`/
  `0x15888`/`0x17700` = 44.1/48/88.2/96k), i.e. `+0x3e` selects internal
  sample-rate mode. **No clock-SOURCE register lives in MOTUAW.sys**: the only
  clock strings are SMPTE ("Unknown SMPTE format", "SMPTE output not supported
  by this interface") — no "word clock"/"ADAT"/"internal" register enum. So
  internal/word/ADAT/SPDIF source selection is done at a higher layer (the app
  / CueMix control path, `mix_base` region — see `cuemix-control-map.md`), not
  as a simple audio-base register. This is now a CueMix-side / card-bound item,
  not a missing audio register.
- The `dmaPoint`/ring bounds guard (`0x255bb`) consumes a position read by the
  caller; ties to `base+0x128` and `0x29500(A+0x10,A+0x14,A+0x18)`.
