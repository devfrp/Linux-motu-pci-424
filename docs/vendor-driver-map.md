# Vendor driver map (Phase 0.3)

Roles of the binaries in `vendor/` (git-ignored, never redistributed). Facts
below come from static RE (`objdump -d -M intel`, `strings`, import tables). All
VAs are with the `MOTUAW.sys` image base `0x10000` (so an IAT thunk at RVA
`0x200xx` is called as `ds:0x300xx`).

## `MOTUAW.sys` — the audio function driver (RE target)

PE32 i386, image base `0x10000`. C++ with source-file strings
`PCI424Driver.cpp`, `PCI424NanoDriver.cpp`, `AW2408.cpp`, `AW24IO.cpp`,
`Awnano.cpp`. This is the only binary that touches register/DMA/FPGA logic.

Everything hardware-facing funnels through **three tiny accessor helpers** that
implement the windowed card-address dispatch (see `register-map.md`):

| Accessor | VA | Import used | Meaning |
|---|---|---|---|
| `WriteRegister(cardAddr, u32)` | `0x29110` | `WRITE_REGISTER_ULONG` (`ds:0x300fc`) | one 32-bit MMIO write |
| `ReadRegister(cardAddr) -> u32` | `0x29160` | `READ_REGISTER_ULONG` (`ds:0x300f8`) | one 32-bit MMIO read |
| `WriteBlock8(sel, src)` | `0x291a0` | `WRITE_REGISTER_ULONG` ×8 | writes 8 dwords to window-A `0x18c0000\|8` (bank0) or `0x1900000\|8` (bank1) |

The compiler also **inlines** the same dispatch at many call sites, so the raw
import counts are: `WRITE_REGISTER_ULONG` 62, `READ_REGISTER_ULONG` 23,
`WRITE_REGISTER_BUFFER_ULONG` 6, `READ_PORT_ULONG` 1, `WRITE_PORT_ULONG` 5.

Notable absence: **no file-I/O imports** (`ZwCreateFile`/`ZwReadFile` are not in
the IAT). The driver therefore does *not* read `altera424b.rbf` from disk
itself — see `fpga-upload.md`. It does import the registry `Zw*` calls
(`ZwCreateKey`, `ZwQueryValueKey`, `ZwSetValueKey`) and `IoGetDmaAdapter`.

Interrupt: `IoConnectInterrupt` at `0x2043c` registers the ISR shim at
`0x201f0` with `ServiceContext = this` (the hardware object). The shim forwards
to a **C++ virtual method** (`call [[ctx]+0x28]`). That slot was resolved by
hand: the hardware class vtable is at `.rdata` `0x30cc0`, and slot `0x28` =
`0x2bae0` = the real ISR (see `register-map.md` for the fully decoded IRQ path,
ack, enable and audio-register block). Vtable resolution was done with a small
Python helper over the raw section bytes, since objdump gives no xrefs.

### StartDevice resource parsing (Phase 1.2, RE'd via `xref.py`)

Located the IRP_MN_START_DEVICE resource-list translator: `fn 0x28ee0` walks
the raw `CM_PARTIAL_RESOURCE_DESCRIPTOR` array (16-byte stride) handed in by
the PnP manager, dispatching on `Type` through a compact translation table
(`0x2907c`) + a 5-entry jump table (`0x29068`). Only three types are handled;
everything else (including `CmResourceTypeDma`, type 4) hits the default
no-op case — **the resource parser never touches a DMA descriptor**, one more
independent confirmation that the audio path is not host bus-mastered.

- **Port** (`Type=1`, jump `0x28f2d`): captures `Start.LowPart` as the
  I/O-port BAR base. If it is zero, the parser **abandons the loop
  immediately** (jumps straight to the shared exit/WMI-log tail with event
  code `0x7a` instead of continuing to the next descriptor) — structurally
  this cuts resource processing short on a missing port BAR, so it is
  mandatory in practice. The exact NTSTATUS the function then returns wasn't
  pinned (it falls through to whatever the WMI-log helper `0x23570` returns
  in `eax`, not an explicitly-set status) — treat "hard failure" as a
  reasonable reading of the control flow, not a confirmed return value.
- **Interrupt** (`Type=2`, jump `0x28f42`): captures Level/Vector/Affinity
  plus an edge-vs-level flag (`Flags==1`) and a `ShareDisposition==3` flag —
  boilerplate `IoConnectInterrupt` prep, nothing card-specific.
- **Memory** (`Type=3`, jump `0x28f73`): dispatches purely on the resource's
  **exact `Length`**: `== 0x400000` (4 MiB) maps **window B**, `== 0x800000`
  (8 MiB) maps **window A**, both via `MmMapIoSpace(phys, len, CacheType=0)`
  — cache type is hard-coded to **non-cached** on both windows (no
  write-combining anywhere on the register/aperture path). This is an exact
  ground-truth confirmation of the two MMIO window sizes already assumed in
  `motu424.h`/`tools/motu424-probe.c`; our `motu424_assign_windows()` uses
  `>=` thresholds rather than an exact match, which is deliberately more
  permissive and still correct against this evidence.
- A separate helper (`fn 0x20100`) builds a `DEVICE_DESCRIPTION` (via
  `IoGetDeviceProperty(DevicePropertyBusNumber)`, IAT `0x30068`) and calls
  `IoGetDmaAdapter` (IAT `0x30064`), storing the HAL DMA-adapter object at
  offset `+0x28` of its `this` (`ecx`/`esi`), with a matching teardown
  routine (`fn 0x20190`, guarded by `+0xc`). *Caveat:* this trace did not
  confirm that `this` is the *same* device-extension object used by
  `WriteRegister`/`ReadRegister` elsewhere — it may be a sub-object, but the
  `+0x28`/teardown pair is internally consistent either way. What's solid
  regardless of that: this DMA-adapter object is **never referenced by the
  PIO audio path** (`WRITE_REGISTER_BUFFER_ULONG` sites don't touch it), so
  it's orthogonal to streaming — likely WDM-template boilerplate; not
  blocking for the Linux driver, which has no DMA adapter.

Device-extension field map extended by this trace: **`+0x28`** = HAL
DMA-adapter pointer, **`+0x50`** = pending-error/status code (checked before
each `MmMapIoSpace`, logged via a WMI-event helper `fn 0x23570` if set),
**`+0x6c`** = interrupt object — this cross-checks cleanly against the
already-resolved ISR shim (`fn 0x201f0`, `re-hardware-model` memory), which
guards on the same `[ctx+0x6c]` before dispatching to vtable slot `0x28`.

### Device-extension field map (Phase 0.2, partial — no full type recovery)

Consolidated from every setter/getter site identified so far (register-map.md,
transport.md, the StartDevice trace above, and the constructor/init trace below).
This is the hardware object's `this` (vtable `0x30cc0`) as seen by
`WriteRegister`/`ReadRegister`/ISR — **not** a complete struct layout (no
Ghidra grade type recovery was done), but every field offset whose meaning is
pinned by at least one concrete access is listed:

| Offset | Type | Meaning | Evidence |
|---|---|---|---|
| `+0x00` | vtable ptr | C++ class vtable `0x30cc0` | constructor `0x2b674` |
| `+0x28` | ptr | HAL DMA-adapter (`IoGetDmaAdapter` result), unused by audio path | StartDevice trace (`fn 0x20100`/`0x20190`) |
| `+0x38` | ptr | `[[+0x38]]` DPC object, queued on period-elapsed | ISR (`0x2bae0`) → `call [[[[edi+0x38]]]]` at `0x2bd2e` |
| `+0x40` | s32 | rate family / mode (`<8` = 1x, `≥8` = 2x/4x); drives buffer size + IRQ countdown | `0x2a710`, `0x2bd40`→`imul +0x1cc, 0xbb80` |
| `+0x50` | u32 | pending-error/status, gates `MmMapIoSpace`+WMI-log | StartDevice trace (`fn 0x28e60`) |
| `+0x6c` | ptr | interrupt object (`IoConnectInterrupt` result) | ISR shim guard (`0x201f0`), teardown `0x201d0` |
| `+0x70` | ptr | audio sub-object `A` (allocated 0x50 bytes via `0x2b610`); obtained in init via vtable slot `0x44` of `this` | init `0x2c15c`, sub-ctor `0x2b560` |
| `+0x74` | vtable ptr | embedded sub-object vtable `0x30ca4`; freed by `0x2b4f0` | ctor `0x2b67c`, dtor `0x2b7d6` |
| `+0x78` | ptr | window-B MMIO base (mapped VA, 4 MiB) | `motu424_addr()` dispatch everywhere |
| `+0x7c` | ptr | window-A MMIO base (mapped VA, 8 MiB) | `motu424_addr()` dispatch everywhere |
| `+0x80` | ptr | I/O-port BAR base (mapped VA) | ISR `0x2baf0`, enable `0x298f3` |
| `+0x84` | u32 | port `+0x8` page-select value cache (`apertureBase >> 22`) | arm `0x2c1ec`/`0x2c27e`, ctor zeroes it |
| `+0x88` | u32 | IRQ ack card-address (runtime; comes from `[A+0x04]`) | ISR (write `0x10`), init `0x2c16a` |
| `+0x8c` | u32 | zero-init; written through `audio_base+0x8c` mirror | ctor `0x2b6da` |
| `+0x90` | u32 | zero-init; written through `audio_base+0x90` mirror | ctor `0x2b6e0` |
| `+0x94` | u32 | zero-init | ctor `0x2b6e6` |
| `+0x98` | u32 | audio register block base (runtime card address, read from `[A+0x30]`) | init `0x2c395`; must be non-zero or init fails |
| `+0x9c` | u32 | CueMix mixer coefficient base (runtime card address; read from `audio_base+0x4`) | init `0x2c427` |
| `+0xa0` | u32 | audio_base+0x4 mirror (read-back at init) | init `0x2c45d` |
| `+0xa4` | u32 | audio_base+0x8 mirror (read-back at init) | init `0x2c493` |
| `+0xa8` | u32 | audio_base+0x14 mirror (read-back at init) | init `0x2c4c9` |
| `+0xac` | u32 | audio_base+0x18 mirror (read-back at init) | init `0x2c50e` |
| `+0x110` | u8[0xb4] | inline CueMix staging buffer (45 dwords), memset `0xb4` at init | init `0x2c4ff`→`0x2c519`, flusher `0x29aa0` |
| `+0x1c4`/`+0x1c8` | u32 | CueMix dirty-range markers (init `0`/`0x2c`; reset to `0xffffffff` later) | init `0x2c52c`, flusher |
| `+0x1cc` | u32 | rate multiplier (small int), drives `+0x1f0 = +0x1cc * 0xbb80` (48000) when `+0x40 >= 8`; init `1` | ctor `0x2b6ec`, `0x2bd49` |
| `+0x1d0` | u32 | rate bit-mask, tested by `0x2a710` (`test [+0x1d0], 1<<(rate-8)`) to choose buffer scaling | ctor zeroes, `0x2a74b` |
| `+0x1d4` | u32 | high-bit sentinel (init `0x80000000`; ORed into `audio_base+0x80` writes in `0x2a820`) | ctor `0x2b6f6`, `0x2a8b4`; init swaps it |
| `+0x1d8..+0x1e8` | u32×4 | zero-init (purpose unknown) | ctor `0x2b6fc`–`0x2b708` |
| `+0x1ec` | u32 | 0x9c40 (40000) init; when 0, `0x2a820` ORs `0xfff10000` into `audio_base+0x80`; else ORs `0x10000` — a rate/mode qualifier | init `0x2c5a0`, `0x2a89a` |
| `+0x1f0` | u32 | IRQ countdown target; each IRQ subtracts `[+0x260]`; on `<=0` calls `0x2a820`+`0x2a710` and zeroes +0x1f0 | ISR `0x2bcf8`–`0x2bd26`; set by `0x2bd59` |
| `+0x25c` | u8 | zero-init (purpose unknown) | ctor `0x2b70e` |
| `+0x260` | u32 | per-IRQ sample increment | ISR `0x2bb55` |
| `+0x264` | u32 | per-IRQ sample accumulator (>= `0x800` ⇒ period boundary) | ISR `0x2bb5b` |
| `+0x268` | u32 | max value of the position numerator (`base+0x12c / base+0x130`) | ISR `0x2bce6` |
| `+0x26c` | u32 | max value of the position counter (`base+0x128`) | ISR `0x2bce0` |
| `+0x270..+0x278` | u32×3 | zero-init (purpose unknown) | ctor `0x2b726`–`0x2b732` |
| `+0x27c`/`+0x280` | u32 | init `0xbb8` (3000); consumed by `0x2aab0` (`(+0x280 + 0x32) / 100 ≠`, then `*3` near `0x2ab2c`) — rate constant or sub-divider | ctor `0x2b738`/`0x2b73e` |
| `+0x284` | u32 | rate-state field (`0x2a9e0` multiplies by `[+0x44]==0 ? 0x1b9 : 0x1e0` then by `0xcccccccd>>3` = `/25`) | `0x2aa16` |
| `+0x288` | u32 | rate-state field (same path as +0x284, written to `audio_base+0xc4`) | `0x2aa07` |
| `+0x28c` | u32 | computed in `0x2aab0` from `+0x280+0x32` then `/100`, stored before writing `audio_base+0x9c` | `0x2aae8` |
| `+0x298` | embedded | large embedded sub-object (~0x2800 bytes; managed by `0x200b0`/`0x202c0` init and `0x20190`/`0x20280` teardown) | ctor `0x2b751`, init `0x2c164`, dtor `0x2c103` |
| `+0x2a4` | u32 | flag checked early during init (init `0x2c150`: nonzero skips the "no resource" WMI log) | init `0x2c185` |

Off the audio sub-object `A` (`[devext+0x70]`, 0x50 bytes, vtable `0x30ca8`,
constructed at `0x2b560`): `[A+0x04]` ack, `[A+0x18]`/`[A+0x1c]` playback
aperture base/len, `[A+0x20]`/`[A+0x24]`/`[A+0x28]`/`[A+0x2c]` capture
aperture base/flag/len (gated by `[A+0x24]!=0`), `[A+0x30]` audio_base card
address, `[A+0x34]` initial register value backing `audio_base+0x8` write
(see `transport.md`). Defaults set by the ctor: `[A+0x14]=0x20000001`,
`[A+0x24]=0x3800`, `[A+0x34]=0x20000001`, `[A+0x44]=0x3100`; the per-bank
init buffer (`WriteBlock8`-driven 8-dword block) is set up there too.

**Outstanding layout question:** the devext contains three sub-objects — the
0x50-byte audio sub-object `A` (vtable `0x30ca8`, allocated via `0x2b610`,
stored at `+0x70`), an embedded sub-object at `+0x74` (vtable `0x30ca4`),
and the ~0x2800-byte sub-object at `+0x298`. The total struct size is >0x2a8
bytes (last touched offset `+0x2a4` plus the 0x2800-byte tail at `+0x298`
pushes it well past 0x2a00). A complete layout is still out of reach: the
purpose of `+0x1d8..+0x1e8`, `+0x25c`, `+0x270..+0x278` remains
unidentified.

### Breakout (AudioWire) enumeration — Phase 3.5 (RESOLVED, no card, via rz-ghidra)

The vendor learns an attached interface's channel map by **deserializing a
descriptor blob**, not by reading a static channel-count table — which is why
`strings`/`objdump` on `AW2408.cpp` only ever yielded filenames. Recovered with
the `rz-ghidra` decompiler (`pdg`), which resolves the C++ dispatch `objdump`
could not:

- The config-build routine `fn 0x1b900` (called from `fn 0x1c380`) receives an
  **adapter object** (vtable `0x30d60`, constructed at `fn 0x2d720`) that wraps
  a byte buffer: `+0x08` = buffer base, `+0x10` = read cursor, `+0x14` = length.
- That adapter's vtable slots are typed byte-stream *pulls* over a bounded
  cursor primitive (slot `0x48` = `fn 0x2d870`: bounds-check
  `cursor+n <= length`, `memcpy` `n` bytes, advance cursor, else error
  `fn 0x142b0(0x2f3d0,…)`):
  - slot `0x64` (`fn 0x2e530`) → pull **1 byte** (per-channel config);
  - slot `0x68` (`fn 0x2e540`) → pull **2 bytes** (returned `& 0xffff`);
  - slot `0x6c` (`fn 0x2e560`) → pull **4 bytes** — the **count**, gated
    `> 2` in `fn 0x1b900` before the per-channel loops run.
- `fn 0x1b900` then fills, into a target buffer (`in_ECX[5]`), **three
  `0x1200`-byte banks** of per-channel bytes (slot `0x64`) followed by a
  **`0x180`-byte / 96-dword** table (`0x3600..0x3780`, slot `0x6c`) — the
  routing/channel map.

**Conclusion:** channel counts and the routing map are **runtime descriptor
data streamed from the connected breakout over AudioWire**, deserialized field
by field; there is no static per-interface channel-count constant to lift into
the Linux driver. A Linux driver must therefore either query the same
descriptor on a real card or expose channel counts as configuration — this
cannot be pinned further statically. (This matches, and now proves with a
decompiler, the earlier "channel count is interface-supplied" hypothesis.)

## `MotuBus.sys` — PCI bus/multifunction enumerator (RE'd)

Small (`~25 KB`, 44 NTOSKRNL imports, no other DLLs). Confirmed by its import
table to be a **pure PnP bus/filter driver**: `IoCreateDevice`,
`IoAttachDeviceToDeviceStack`, `IoRegisterDeviceInterface`,
`IoSetDeviceInterfaceState`, `IoInvalidateDeviceRelations`,
`IoGetAttachedDeviceReference` — it creates/stacks the child device object(s)
that `MOTUAW.sys` attaches to and triggers (re)enumeration. **Decisive negatives:**
*no* hardware access (no `READ/WRITE_REGISTER`, no `READ/WRITE_PORT`, no
`HalGetBusData`) and *no* file I/O (no `ZwCreateFile/ZwReadFile`). So it neither
touches registers nor uploads the FPGA — which, together with the same negative
for `MOTUAW.sys`, means the **FPGA bitstream upload is done by a user-mode
component**, not by either kernel driver (see `fpga-upload.md`). A Linux driver
binds the PCI function directly and needs no equivalent.

## `MAWWAVE.sys` — WDM/WaveRT audio miniport (RE'd)

`~70 KB`. Links **`portcls.sys`** (`PcNewPort`, `PcNewServiceGroup`) — it is a
Windows Port Class **WaveRT / WaveCyclic miniport** (source strings
`wavecyc.cpp`, `wavecycstrm.cpp`, `WaveRT.cpp`, `waveRTStrm.cpp`; pdb
`mawwave.pdb`). It presents the card to the WDM audio stack and **delegates all
hardware to `MOTUAW.sys`** — confirmed: *no* direct `READ/WRITE_REGISTER` or
`READ/WRITE_PORT`. This is exactly the layer the Linux **ALSA PCM** side replaces,
so it carries no unique register knowledge. Minor confirmation: the WaveRT model
(a shared cyclic buffer the app fills and the device drains) is consistent with
the PIO-aperture ring recovered in `transport.md`.

## `HDExpress_FullImageRun.bin` — PCIe variant image (RE'd)

`~1.2 MB` image for the PCIe "HD Express" (`DEV_0005`) card. **Fully dissected**
(see `fpga-upload.md`): a 24-byte-header container (load `0x100000`, entry
`0x108608`, sum32 payload checksum verified) holding an ARM firmware section + a
Xilinx Virtex FPGA bitstream (sync `0xAA995566`) + two small config records — an
**ARM SoC + Xilinx** design, distinct from the classic PCI-324/424's Altera
passive-serial FPGA (`altera424b.rbf`, which is *not* in the 4.0.6 installer).
