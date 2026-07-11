# Audio transport & ring (Phase 3.1 / 3.2)

How samples move between host and card. Recovered from the
`WRITE_REGISTER_BUFFER_ULONG` sites and the `ReadHead out of bounds` guard in
`MOTUAW.sys`. VAs use image base `0x10000`.

## It is PIO into a card aperture, not host bus-master DMA (CONFIRMED)

The vendor driver does **not** hand the card a host ring base. It copies host
audio into a card-side aperture in **window B** with
`WRITE_REGISTER_BUFFER_ULONG`. The push helper is now pinned to **`fn 0x29420`**
(confirmed with `tools/re/xref.py`): it allocs a `'MOTU'`-tagged `0x10000`
bounce buffer, `memset`s it, `memcpy`s ≤`0x10000` bytes of the source, then
feeds the buffer to a stream sub-object (obtained via `this` vtable slot `0x48`)
byte-by-byte through that sub-object's slot `0x8`, and frees the buffer. In
outline:

```
tmp = ExAllocatePool(..., 0x10000) tagged 'MOTU'   // 64 KB bounce buffer
memset(tmp, 0, 0x10000)
len = min(byteLen, 0x10000)
memcpy(tmp, src, len)
dwords = (len >> 2) + 1
dest = base_B + (cardAddr & 0x3fffff)              // window-B aperture slot
WRITE_REGISTER_BUFFER_ULONG(dest, tmp, dwords)     // ds:0x30100
ExFreePoolWithTag(tmp, 'MOTU')                     // ds:0x30104
```

So the transfer granularity is **≤ 64 KB per burst**, dword-aligned, into a
caller-chosen window-B card address (the current write position in the ring).

**Implication for the Linux driver:** the current
`snd_pcm_set_managed_buffer_all` + "hand the card `runtime->dma_addr`" model in
`motu424_hw.c` is wrong for this card. The rewrite (Phase 4.3) needs an indirect
/ copy PCM: on each period, `memcpy_toio`-style push host period data into the
window-B aperture and advance the software write head.

## Ring state machine (CONFIRMED shape — guard at `0x25500`)

The card exposes a hardware position `dmaPoint`; software tracks `readHead`,
`writeHead`, `len`. The bounds-check function (debug string VA `0x2f2e0`,
referenced at `0x255bb`) reads:

- `len`       = ring size in **dword units**, device-ext `[esi-0xa8]`
- `readHead`  = last consumed position,      device-ext `[esi-0x94]`
- `dmaPoint`  = HW play/capture position (function argument) — **see
  `dmaPoint` register below for its concrete address**
- `writeHead` = producer position

### `dmaPoint` register (CONFIRMED — reader `fn 0x2a5c0`)

`dmaPoint` is `READ_REGISTER_ULONG(audio_base + 0x2c)` where `[+0x98]` is the
audio register block base. The reader (`fn 0x2a5c0`) then subtracts
`[devext+0x10]` (the playback aperture base card address, cached here during
init — confirms `[devext+0x10] = [A+0x18]`, the playback aperture base) and
right-shifts by 2 (byte → dword), returning the **position in dwords** as
expected by the bounds guard:

```
u32 fn_2a5c0(devext *this) {
    u32 raw = READ_REGISTER_ULONG(this->audio_base + 0x2c);   // dmaPoint raw
    return (raw - this->play_aperture_base) >> 2;              // dword offset
}
```

So:

| Card addr | Dir | Method VA | Meaning |
|---|---|---|---|
| `audio_base + 0x2c` | R | `0x2a5c3`→`0x2a5e7`/`0x2a600` | **`dmaPoint`**: raw HW position counter (bytes); returned as `(raw - aperture_base) >> 2` (dwords) |

The sibling reader `fn 0x2a610` reads `audio_base + 0x34` (a second position
or status counter) and is called from `0x266ab` and `0x2b029`; its semantics
are OPEN but its existence confirms the audio-base register block has a
small position/counter cluster at `+0x2c` / `+0x34`.

Logic (paraphrased):

```
writeHead_wrapped = writeHead & (writeHead >= len ? 0 : ~0)   // wrap at len
delta = len - dmaPoint ... newReadHead = ...
if (newReadHead != stored_readHead) {
    if (newReadHead < 0 || newReadHead >= len)
        DbgPrint("ReadHead out of bounds: dmaPoint %08x, readHead %08x, "
                 "writeHead %08x, len %08x", dmaPoint, newReadHead,
                 writeHead, len);
    stored_readHead = newReadHead;
    // notify / advance client (calls at 0x13170, 0x131b0, 0x131e0)
}
```

The `& (>=len ? 0 : ~0)` idiom masks to wrap, which assumes **`len` is a power
of two** in dword units. Positions are in dwords, not bytes.

## Format (from `motu424.h`, unchanged)

Native samples are 24-bit, packed **3 bytes per channel per frame**
(`S24_3LE`/`_3BE`). Confirm endianness on a card. The per-frame channel count
shrinks with the rate family (1x→2x→4x); the concrete mapping is the Phase 3.3
clock/rate work and is still OPEN.

## Aperture bases are runtime, and paged via the port bridge (CONFIRMED — arm fn `0x2c150`)

The arm/prepare routine `fn 0x2c150` (the sole caller of the bulk push `0x29420`)
resolves the aperture from the audio sub-object `A = [devext+0x70]` (obtained via
`this` vtable slot `0x44`):

| Field | Meaning |
|---|---|
| `[A+0x04]` | IRQ **ack** card-address (→ `devext+0x88`) — confirms register-map.md |
| `[A+0x18]` / `[A+0x1c]` | **playback** aperture base / length (bytes) |
| `[A+0x28]` / `[A+0x2c]` | **capture** aperture base / length (bytes); gated by `[A+0x24]!=0` |

So playback and capture use **separate apertures** (both runtime card-reported —
they cannot be static constants). Before streaming a direction the driver pages
the window in:

```
WRITE_PORT(port_base + 0x8, apertureBase >> 22);   // 4 MB window-B page index
```

then, per direction: `WRITE_PORT(port+0x4, 1)` (strobe) → bulk push `0x29420` →
set page (above) → zero-fill the aperture (`for edi in [0,len) step 4:
WRITE_REGISTER(dispatch(base+edi), 0)`) → `call 0x29500(A+0x10, A+0x14, A+0x18)`
(post-push commit, unwalked).

## Open

- Aperture `len` power-of-two assumption vs. the runtime `[A+0x1c]`/`[A+0x2c]`.
- Endianness / channel packing of the aperture payload.
- `audio_base + 0x34` (second position/counter reader `0x2a610`): is it the
  *capture* `dmaPoint` counterpart to `audio_base + 0x2c` (playback)?
  Apache-shape hypothesis — both are read via the same window dispatch, both
  run through `>> 2` dword conversion differently — but the second reader
  at `0x2a610` doesn't do the subtraction, so its semantics stay OPEN.
- **Full-duplex page aliasing (design risk, code-reviewed, not RE'd further):**
  play and capture apertures are two independent runtime addresses; if they
  land in different 4 MB window-B pages, a single shared page-select register
  (port `+0x8`) means starting/streaming one direction can silently repoint
  the window out from under the other. The vendor trace only shows the page
  write happening once, "before streaming a direction" (`fn 0x2c150`) — it
  was not confirmed whether the vendor re-arms it on every transfer or relies
  on both apertures sharing a page by construction. `motu424_hw.c` now
  re-arms the page-select on every `motu424_push_period()` call (not just at
  stream start) as a defensive fix, which is correct either way but adds one
  extra port write per period. Worth confirming on a card whether this was
  ever actually reachable (i.e. whether play/cap apertures do share a page).
