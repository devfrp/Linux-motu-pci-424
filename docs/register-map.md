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

The `0x01800000` tag bit selects window A for the **same** underlying bank
registers: window-B `0x000c0000` and window-A `0x018c0000` differ by exactly
`0x01800000` and both name bank 0. Window A is used for the 8-dword block write
(`WriteBlock8`, VA `0x291a0`); window B for ordinary single reads/writes and the
audio aperture. Interpret window A vs B as two access *paths/timings* to one
address space, not two disjoint memories.

## I/O-port BAR (CONFIRMED — dev-ext `+0x80`)

A small port window (`READ_/WRITE_PORT_ULONG`), base stored at dev-ext `+0x80`.
Likely a PLX-style local-bus bridge fronting the FPGA.

| Port off | Dir | Evidence VA | Meaning (inferred) |
|---|---|---|---|
| `+0x0` | R | `0x2baf0` | status; code does `(val >> 1) & 1` → **bit 1 = ready/pending** poll |
| `+0x4` | W `1` | `0x2c3dc` | strobe/kick (written `1` after a timeout/elapsed check) |
| `+0x8` | W | `0x2b6c8` | init write (during bring-up; value from dev-ext `+0x84`, seen `0`) |

## Bank register file (INFERRED)

Two banks at window-B card addresses `0x000c0000` (bank 0) and `0x00100000`
(bank 1); stride `0x40000`. Same banks via window A at `0x018c0000` /
`0x01900000`.

| Card addr | Dir | Evidence VA | Notes |
|---|---|---|---|
| `bank+0x24` (`0xc0024`, `0x100024`) | R/W | `0x29305` (`or 0x40` write), `0x29408` (read `>>6`) | per-bank control/status; **bit 6** is a r/w control flag; other bits form a per-bank mask (`rol` loop at `0x29299`) |
| `bank+0x08 … +0x24` | W | `WriteBlock8` `0x291a0` | 8-dword block programmed as a unit at init (window A) |

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
| `base+0x54` | W `1` | `0x29660` (slot 1) | stream/DMA **enable** (writes 1 when arg≠0) |
| `base+0x5c` | W | `0x29840` | secondary param (writes `[this+0x10]`); purpose OPEN |
| `base+0x60` | W | `0x295e0` (slot 0) | **period increment** = `0x10 << (2*family)` (×2 if `[+0x1cc]>1`); samples-per-IRQ / block size |
| `base+0x64` | W | `0x297f0` (slot 2) | rate/clock **parameter** (caller-supplied dword, no encoding in the setter) |
| `base+0x128` | R→W`0` | ISR `0x2bae0` | position counter; read each period, then zeroed |
| `base+0x12c` | R→W`0` | ISR `0x2bae0` | numerator; read, divided by `base+0x130`, then zeroed |
| `base+0x130` | R→W`0` | ISR `0x2bae0` | divisor; read then zeroed |

The **ack register is a separate card address in device-ext `+0x88`** (from the
same sub-object, field `+0x4`; method `0x2c150`).

## Mixer coefficient region (CONFIRMED via the bulk-write destinations)

A **third card-reported base lives in device-ext `+0x9c`**: at init (`fn 0x2c360`,
store at `0x2c427`) the driver `READ_REGISTER`s a location off the audio base and
keeps the returned card address there — same pattern as `+0x98`/`+0x88`. The
**CueMix mixer coefficients** are staged in an inline object buffer at `+0x110`
(~45 dwords) and flushed to `+0x9c` with `WRITE_REGISTER_BUFFER_ULONG`
(`fn 0x29aa0`, site `0x29ad5`), writing only the **dirty range** `[+0x1c4]..[+0x1c8]`
(classic mixer incremental update). This is the hardware sink for the CueMix
control set — see `cuemix-control-map.md`. Full destination table for all six
`WRITE_REGISTER_BUFFER_ULONG` sites (audio aperture / channel-bank / mixer) is in
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

So: **IRQ pending = port BAR `+0x0` bit 1; ACK = write `0x10` to the card
address in device-ext `+0x88`; "period elapsed" fires when a per-IRQ accumulator
crosses `0x800`, then a DPC is queued.**

## Port BAR — refined (CONFIRMED)

| Port off | Dir | Value | Evidence | Meaning |
|---|---|---|---|---|
| `+0x0` | R | — | ISR `0x2baf0` | status; **bit 1 = IRQ pending** |
| `+0x0` | W | `4` | `0x298f3` | control; **bit 2 = IRQ/stream enable** |
| `+0x4` | W | `1` | `0x29906`, `0x2c3dc` | **strobe / commit** |
| `+0x8` | W | var | `0x2b6c8` | init value (bring-up) |

Method `0x298e0` is the **start/enable** sequence: `WRITE_PORT(+0x0, 4)` then
`WRITE_PORT(+0x4, 1)`.

## Rate mapping (PARTIAL)

- **family** ∈ {0,1,2} = {1x, 2x, 4x} drives `period_increment = 0x10 <<
  (2*family)` → `base+0x60` (16 / 64 / 256, doubled in one mode).
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

## Port bridge page register (CONFIRMED — arm fn `0x2c150`)

Port `+0x8` (previously "init value") is the **window-B page/segment select**:
the arm routine writes `apertureBase >> 22` there before streaming each
direction (`>>22` = the 4 MB window-B page index). See `transport.md`.

## Genuinely open (needs a card or more RE)

- The **numeric** values of the audio base (`devext+0x98`), ack address
  (`devext+0x88`) and the two aperture bases (audio sub-object `[A+0x18]`
  playback / `[A+0x28]` capture, with lengths `[A+0x1c]`/`[A+0x2c]`): all read
  from the card at init, so they are runtime values, not static constants. On a
  card, dump them via the probe. The *structure* is fixed and known (base+0x54/
  0x5c/0x60/0x64/0x128/0x12c/0x130, ack=0x10, page=base>>22 to port+0x8).
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
