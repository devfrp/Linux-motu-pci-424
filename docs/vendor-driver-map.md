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
transport.md, and the StartDevice trace above). This is the hardware object's
"this" as seen by `WriteRegister`/`ReadRegister` and the ISR — **not** a
complete struct layout (no Ghidra-grade type recovery was done), just every
field offset whose meaning is pinned by at least one concrete access:

| Offset | Meaning | Evidence |
|---|---|---|
| `+0x28` | HAL DMA-adapter pointer (`IoGetDmaAdapter` result) | StartDevice trace (`fn 0x20100`/`0x20190`); unused by the audio path |
| `+0x38` | `[[+0x38]]` DPC object, queued on period-elapsed | ISR (`0x2bae0`) |
| `+0x50` | pending-error/status code, gates `MmMapIoSpace`+WMI-log helper | StartDevice trace (`fn 0x28e60`) |
| `+0x6c` | interrupt object (`IoConnectInterrupt` result) | ISR shim guard (`0x201f0`), teardown (`fn 0x201d0` → `IoDisconnectInterrupt`) |
| `+0x70` | audio sub-object pointer `A` (vtable slot `0x44`) | arm routine `0x2c150` |
| `+0x78` | window-B MMIO base (mapped VA) | `motu424_addr()` dispatch |
| `+0x7c` | window-A MMIO base (mapped VA) | `motu424_addr()` dispatch |
| `+0x80` | I/O-port BAR base (mapped VA) | port accessors |
| `+0x88` | IRQ ack card-address (runtime) | ISR (write `0x10`) |
| `+0x98` | audio register block base (runtime card address) | init read (`0x2c360`) |
| `+0x9c` | CueMix mixer coefficient base (runtime card address) | init read (`0x2c360`) |
| `+0x110` | inline CueMix staging buffer (~45 dwords) | `0x29aa0` |
| `+0x1c4`/`+0x1c8` | CueMix dirty-range markers | `0x29aa0` |
| `+0x260` | per-IRQ sample increment | ISR |
| `+0x264` | per-IRQ sample accumulator | ISR |

Off the audio sub-object `A` (`[devext+0x70]`): `[A+0x04]` ack, `[A+0x18]`/
`[A+0x1c]` playback aperture base/len, `[A+0x28]`/`[A+0x2c]` capture aperture
base/len (see `transport.md`). Genuinely open: the object's total size/full
layout (the `0x50`-byte allocation seen at `fn 0x2b610` cannot hold every
field above, so either multiple sub-classes share the vtable shape or the
`this` identity varies by call site — this needs real type recovery, not
achievable with `objdump`/`xref.py` alone).

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
