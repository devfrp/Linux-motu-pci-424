# FPGA upload (Phase 2)

Whether the card needs an Altera FPGA bitstream (`altera424b.rbf`) pushed from
the host was the central question of this phase. Static RE of `MOTUAW.sys`
settled it for the classic card (**verdict below: no host upload**). This note
records what that RE did and did **not** establish. VAs use image base `0x10000`.

## Confirmed facts

- The filename strings are present in `MOTUAW.sys`:
  - `A/Repositories/Drivers/MOTUPCIAudioDriver/Resources/altera424b.rbf`
    at VA `0x3fe43`
  - `altera424b.rbf` at VA `0x3fe94`
- The bitstream is **not embedded** in `MOTUAW.sys` and **not read from disk by
  it**: there is no `ZwCreateFile`/`ZwReadFile`/`ZwOpenFile` in the driver's
  import table. The path string has **no code xref** (no `push 0x3fe94` etc.),
  consistent with it being a build-time resource path, not a runtime open.
- The card has an I/O-port bridge (dev-ext `+0x80`) whose bits look like a
  config/status interface: read `+0x0` bit 1 = ready/pending; write `+0x4`←1 =
  strobe; write `+0x8` = value (see `register-map.md`).

## What this means for how the bitstream actually reaches the card

Because the audio `.sys` neither embeds nor opens the `.rbf`, the byte source
must come from **outside** it. The most likely vendor paths (unconfirmed):

1. A user-mode installer/service reads `altera424b.rbf` and pushes it to the
   driver via an `IRP_MJ_DEVICE_CONTROL` (IOCTL); the driver then streams the
   bytes to the card — plausibly via `WRITE_PORT_ULONG` strobes (`+0x4`/`+0x8`)
   in passive-serial style, or a bulk `WRITE_REGISTER_BUFFER_ULONG` into a
   config window. The IOCTL dispatch was not traced to a byte-feeding loop with
   objdump alone.
2. `MotuBus.sys` or a coinstaller performs the load before `MOTUAW.sys` attaches.

No byte-feeding loop tied to the port strobes was positively identified, so the
**exact handshake (nCONFIG/nSTATUS/CONF_DONE, bit order, clocking) is OPEN.**

## Plan for the Linux driver (Phase 4.2)

- Ship/load the bitstream via `request_firmware("altera424b.rbf")` +
  `MODULE_FIRMWARE("altera424b.rbf")`; fail loudly in `dmesg` if absent. The
  user supplies the file (extracted from the vendor installer) — do **not**
  redistribute it; flag the licensing question.
- Altera passive-serial is **LSB-first** per config byte; if the bridge turns
  out to be passive-serial, feed `.rbf` bytes LSB-first while pulsing the
  `+0x4`/`+0x8` strobes and polling `+0x0` bit 1 for CONF_DONE. Treat this as a
  hypothesis to verify on a card (Phase 6.2 register diffing).
- `HDExpress_FullImageRun.bin` (PCIe `DEV_0005`) is a different, larger image
  (likely FPGA + DSP) and needs its own upload path.

## Phase 2.3 — installer contents (CONFIRMED, no card)

Extracted `MOTU Audio Installer 4.0.6.6814/SetupAudio.exe.exe` (39 MB Wix/MSI
bundle). It wraps two MSI packages as PE resources `.rsrc/RCDATA/MSI00` (17.6 MB,
64-bit) and `MSI01` (21.5 MB, 32-bit). Firmware lives in embedded LZX cabs:

- `PCIFirmware.cab` → **`HDExpress_FullImageRun.bin` only** (1218120 bytes,
  identical sha256 to `vendor/HDExpress_FullImageRun.bin`).
- `Virtex.cab`, `Media1.cab`, `PlugIns*.cab`, `VideoRes.cab`, `Redist.cab`
  contain **no** `.rbf`/bitstream/`.bin` firmware.

**Negative result — `altera424b.rbf` is NOT in this installer** (both MSIs, all
cabs, and the raw tree grep for "altera" are empty). So the classic PCI-324/424
Altera bitstream must be sourced elsewhere (an older PCI-only driver release), or
— more likely — the classic card configures its FPGA from **onboard EEPROM/flash
at power-on** and the `.rbf` path in `MOTUAW.sys` is a legacy/build artefact
(consistent with the driver having no runtime file-open for it). Only the PCIe
**HD Express** needs a host-pushed image. *Treat "classic card needs no runtime
FPGA upload" as the leading hypothesis to confirm on a card (Phase 6.2).*

## HD Express image format — `HDExpress_FullImageRun.bin` (CONFIRMED, no card)

Not a bare FPGA bitstream: it is a **container = 24-byte header + section table**,
all little-endian.

```
Header (0x18 bytes):
  +0x00  0x00100000   load base address
  +0x04  0x18         header length (24)
  +0x08  0x00129630   payload length (1218096); +0x18 == file size 1218120 ✓
  +0x0c  0x03252f5f   checksum = sum of all payload bytes mod 2^32  (VERIFIED)
  +0x10  0x00108608   entry point (base+0x8608)
  +0x14  0x0d1d041d   version/build stamp
```

Payload is a chain of sections; each starts with a 0x1c-byte descriptor
`{type, dataOff=0x1c, size, flags, tag, 0, 0, chk}`, data follows, next
descriptor at `dataOff+size`:

| # | type | file off | size | what |
|---|---|---|---|---|
| 1 | `0x1` | `0x34` | `0x36800` | **ARM firmware** — ARM32 vector table at file `0x30`/section start (`0xEA…` branches, `0xEAFFFFFE` self-loop at the reserved vector); runs at base `0x100000`, entry `0x108608` |
| 2 | `0x5` | `0x36850` | `0x210` | small config record (tag `0x7c2`); precedes/frames the bitstream |
| 3 | `0x6` | `0x36a7c` | `0xf29a0` | **Xilinx Virtex FPGA bitstream** — sync word `0xAA995566` at file `0x36888`, preceded by the `0xFFFF…AA99` preamble; raw config data, no `.bit` ASCII header |
| 4 | `0x7` | `0x129438` | `0x210` | trailer config record (mirrors #2) |

So the PCIe HD Express (`DEV_0005`) is an **ARM SoC + Xilinx Virtex FPGA** — a
different architecture from the classic PCI-324/424's Altera passive-serial FPGA.
This matches the installer shipping a `Virtex.cab` (Xilinx) rather than an Altera
bitstream. The ARM firmware presumably drives the FPGA load internally, so on the
PCIe card the host just DMAs this whole blob to the ARM and lets it self-boot.

## IOCTL interface — fully mapped (CONFIRMED, no card)

Traced the whole `IRP_MJ_DEVICE_CONTROL` path with `tools/re/xref.py`:

```
DriverEntry (init sec, VA 0x63000)
  mov [DriverObject+0x70], 0x21d40      ; MajorFunction[IRP_MJ_DEVICE_CONTROL]
0x21d40  IoAcquireRemoveLockEx; dispatch; IoReleaseRemoveLockEx; complete IRP
0x242b0  edx = Irp->CurrentStackLocation
         eax = [edx+0xc]                ; irpSp->Parameters.DeviceIoControl.IoControlCode
         func = ((eax >> 2) & 0xfff) - 0x800   ; custom-function index (0x800-based)
0x241b0  switch(func-1) via 4-entry jump table @0x242a0
```

So the device exposes exactly **four IOCTLs** (function codes `0x801..0x804`):

| Func | Handler | Role (recovered) |
|---|---|---|
| `0x801` | `0x240f0` | get/create the stream object (`0x1cab0`) at ctx `+0x54`; if a data buffer arg is present, **append its {ptr,len} descriptor to a `std::vector`** (`0x23db0` = vector grow). A generic **buffer-submit**. |
| `0x802` | `0x2424e` | arg≠0 → **start** (`0x23ff0`); arg=0 → stop/teardown (virtual close, zero ctx `+0x54/+0x58`). |
| `0x803` | `0x241cb` | control op — virtual method on the stream object. |
| `0x804` | `0x24216` | install a completion **callback** (`0x238c0`) into the caller's struct. |

**There is no dedicated "load firmware" IOCTL.** MOTUAW.sys offers only this small
generic streaming interface: submit buffers (0x801), start/stop (0x802), control
(0x803), register a callback (0x804). Combined with the earlier negatives (no file
I/O; only 3 port accesses — init/IRQ, *not* a passive-serial feed loop), the
picture is now consistent and firm:

> The **FPGA bitstream is pushed from user mode as ordinary submitted data**
> through the `0x801` buffer channel (queued into the vector, then written to a
> card window by the stream worker) — or via a user-mapped aperture. The driver
> does not "know" it is firmware; it writes bytes to a card address. This is why
> there is neither a file open nor a bit-banging port loop anywhere in the driver.

## Where the bulk card-writes actually go (CONFIRMED) — and the firmware verdict

All six `WRITE_REGISTER_BUFFER_ULONG` destinations were pinned by reading the
address computation just before each call:

| Site(s) | Destination card address | Payload |
|---|---|---|
| `0x29560` (`fn 0x29420`) | `base_B + (cardAddr & 0x3fffff)`, dynamic head | **audio PCM** into the window-B aperture |
| `0x2a22d` / `0x2a291` (`fn 0x2a190`) | `[dev+0x98] + idx*4 + 0x24`, ping-pong `[+0x90]/[+0x94]` | per-**channel/bank** block |
| `0x2c540` / `0x29ad5` / `0x29a04` | **`[dev+0x9c]`**, staged from the inline buffer `[dev+0x110]` (~45 dwords), flushed by a dirty range `[dev+0x1c4]..[dev+0x1c8]` | **CueMix mixer coefficients** |

`[dev+0x9c]` is, like `+0x98` (audio base) and `+0x88` (ack), a **card-reported
address**: at init (`fn 0x2c360`) the driver `READ_REGISTER`s a location near the
audio base and stores the returned card address in `+0x9c` (`0x2c427`). None of
these writes moves anything close to a 340 KB bitstream — the largest is ~45
dwords, and its incremental dirty-range flush is the fingerprint of a **mixer**,
not a one-shot FPGA config.

**Verdict — the classic PCI-324/424 is NOT host-FPGA-uploaded.** Every strand of
evidence now agrees: no file I/O, no firmware IOCTL (only the four streaming
IOCTLs 0x801-0x804), no passive-serial port loop, no large-buffer config write,
and the `.rbf` string has no code xref. The parallel-PCI card's Altera FPGA
therefore **self-configures from onboard flash/EEPROM at power-on**; the `.rbf`
path in `MOTUAW.sys` is a build/legacy artefact. Only the PCIe **HD Express**
(separate ARM+Xilinx image, `HDExpress_FullImageRun.bin`) takes a host upload.

**Consequence for the Linux driver:** Phase 4.2 (`request_firmware("altera424b.rbf")`)
is very likely **unnecessary for the classic card** — plan for no bitstream upload,
and verify on hardware that the card enumerates and streams with no firmware push.
Keep a `request_firmware()` path only for the PCIe HD Express variant.

## To close this phase (needs more than objdump)

- For the **classic PCI-324/424**: obtain `altera424b.rbf` from an older PCI-era
  driver release (this 4.0.6 installer is PCIe-centric), OR confirm on a card
  that the classic FPGA self-configures from flash (no host upload needed).
- Trace the IOCTL dispatch table in `MOTUAW.sys` (or use a real disassembler
  with xrefs — Ghidra/rizin, Phase 0.1) to find the routine that consumes the
  firmware buffer and drives the port strobes.
- For the **PCIe** path: decode how `HDExpress_FullImageRun.bin` is pushed
  (whole-blob to the ARM boot loader vs. section-by-section); the container
  format above is now known, the transport is not.
