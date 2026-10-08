# FPGA and DSP firmware upload (Phase 2)

Whether the classic card needs firmware pushed from the host was the central
question of this phase. The answer, now **confirmed on a real PCI-424
(`137a:0004`) on 2026-10-07** (issue #2): **yes, two images, at every device
start.** The vendor driver carries both images inside its own `.data` section
and the host uploads them before the card does anything useful.

| Image | Goes to | How |
|---|---|---|
| 37,177-byte serial container (AppleSingle wrapper around an Altera-style `.rbf`) | the FPGA | bit-banged through the DSP's GPIO pins |
| 28,272-byte TMS320C64x program | DSP internal RAM, address 0 | written through window B |

The earlier verdict in this file ("the classic card self-configures from
flash, no host upload") was wrong. The section "What the earlier analysis got
wrong" explains why each inference failed, so the same mistakes are not
repeated. VAs below are for the **32-bit** `MOTUAW.sys` (image base `0x10000`);
the 64-bit build carries identical image bytes at different offsets.

## Hardware model (recap)

The card's PCI interface is the PCI port of a **TI TMS320C6412** DSP in PCI
boot mode (board identification, issue #2, 2026-09-20; TI SPRS219J/SPRU581C):

- I/O BAR (16 bytes): `+0` HSR, `+4` HDCR, `+8` DSPP.
- Window A (BAR1, 8 MB): DSP register space at `0x01800000`. The GPIO block
  is at window-A offsets `0x300000/4/8` = GPEN / GPDIR / GPVAL (card address
  `0x01b00000`).
- Window B (BAR0, 4 MB): a page of DSP memory selected by DSPP; with
  DSPP = 0 it is the 256 KB internal RAM, `0..0x3ffff`.

The FPGA (144-pin TQFP, part number under the MOTU sticker) has **no
configuration PROM** on the board, which is why it must be loaded by the host
after every power-up.

## The embedded images (CONFIRMED, static)

All images live in `.data` of `MOTUAW.sys`; both driver builds carry the same
bytes. The `DEV_0004` path (shared with `DEV_0005`) uploads the first two; the
`DEV_0003` (PCI-324) path uploads the other three.

| Image | x86 VA | Length | sha256 (first 16) |
|---|---|---|---|
| PCI-424 serial container (FPGA) | `0x36dd0` | `0x9139` = 37,177 | `34e62cf5a75f53a4` |
| PCI-424 program → RAM 0 | `0x3ff10` | `0x6e70` = 28,272 | `3a23bc97e49154e5` |
| PCI-324 EP1K30 bitstream | `0x46e40` | `0xe74f` = 59,215 | `267b343dfc537d11` |
| PCI-324 program → RAM 0 | `0x58c10` | `0x9340` = 37,696 | `1eae8bad11249f94` |
| PCI-324 data → `0x80000000` | `0x55590` | `0x3680` = 13,952 | `02b75d8d65e0a192` |

`tools/re/extract-firmware.py` cuts all of them out of a user-supplied
`MOTUAW.sys`, verifies the hashes, and writes them to `vendor/firmware/`
(git-ignored). The image bytes are MOTU's and are **never committed or
redistributed**.

The 0004 serial image is an **AppleSingle container** (magic `00051600`,
three entries): data fork at `+0x3e`, 36,617 bytes, which is the Altera-style
`.rbf` (starts with 16 × `ff` then `6a d6 ff 40 00` ×3, ends in `ff` padding);
a 466-byte resource fork (`ckid` "Projector Data"); Finder info type `TEXT`,
creator `CWIE`. This is `altera424b.rbf` as it left the Mac build machine,
wrapper included. The driver clocks out the **whole container**, header and
resource fork too; the FPGA presumably ignores the leading non-preamble bytes
and locks on the `.rbf` sync pattern. The data fork is 292,936 bits, which
matches no uncompressed Altera part size, so it is probably a compressed
bitstream. The 324's image is 473,720 bits = exactly an **ACEX EP1K30**.

## The upload sequence (vendor `fn 0x2c150`, `DEV_0004` path)

Replayed byte for byte by `tools/motu424-bringup` and run once on hardware;
each step lists what the card did. `port` = I/O BAR, `A` = window A, `B` =
window B.

1. **WARMRESET.** `HDCR ← 1`. Hardware: HSR reads `0x14` (PCIBOOT=1,
   EEREAD=1) before and after.
2. **FPGA image through GPIO** (`fn 0x29420` → serial helper
   `0x251e0/0x25170/0x25230`):
   - start: `GPEN ← 0xe0`, `GPDIR ← 0xe0`, `GPVAL ← 0x00`, then `GPVAL ← 0x40`.
   - per byte, **LSB first**, per bit: `v = 0x40 | (bit ? 0x20 : 0)`;
     `GPVAL ← v`, `GPVAL ← v | 0x80`, `GPVAL ← v` (one DCLK pulse with data
     stable).
   - finish: 1000 × (`GPVAL ← 0xc0`, `GPVAL ← 0x40`) trailing clocks, then
     `GPDIR ← 0xc0`.
   - Pin map: **GP5 = DATA0, GP6 = nCONFIG, GP7 = DCLK.** No nSTATUS or
     CONF_DONE read-back on this path (the vendor `status` method is a no-op).
   - Hardware: 894k MMIO writes, about 8.5 s through a PCIe-to-PCI bridge.
     GPIO rest state before any write `GPEN=0xf9 GPDIR=0x00 GPVAL=0xf8`;
     after the load `0xe0/0xc0/0x40` (GP5 back to an input, reading low).
     The vendor stalls 1 µs with nCONFIG low; the tool stretches that to
     100 µs and waits 5 ms before the first DCLK.
3. **Program into DSP RAM.** `DSPP ← 0`; zero-fill `B[0..0x3ffff]`; write the
   28,272-byte program at `B[0]` as `(len>>2)+1` = 7,069 dwords (the vendor's
   bounce buffer is zeroed, so the extra dword is 0). Hardware: read-back
   0/7069 dwords differ and the cleared tail is all zero. **Window B only
   decodes after step 2**; before the FPGA is configured every window-B offset
   returns one repeated 1 KB pattern.
4. **Release the DSP and wait for the mailbox.** `HSR ← 0` (unmask PINTA#),
   `B[0x3fffc] ← 0` (the last dword of internal RAM is the mailbox),
   `HDCR ← 2` (DSPINT), then poll `B[0x3fffc]` with 5 µs stalls until it is
   non-zero. The value is **`audio_base`**. On timeout the vendor writes
   `HDCR ← 1` and fails device start. Hardware: `audio_base = 0x6fac` on the
   first poll after DSPINT; HSR stayed `0x14`, so starting the DSP raises no
   interrupt and the unmask is not needed just to get the mailbox.
5. **Read the published block.** `mix_base = B[audio_base+0]`, then `+4`,
   `+8`, `+0x14`, `+0x18` into the device extension (`register-map.md`,
   `vendor-driver-map.md`). Hardware: `0x8290, 0x87a0, 0x7988, 0x8348,
   0x7240`, with `+0x14 = mix_base + 0xb8` (right after the 45-dword mixer
   block). Decoding the rest of this block is the current RE target.

The vendor's later steps (zeroing the mixer block, rate/buffer registers,
`audio_base+0x50 ← 1`, `A+0x8 ← 0x10914221`) are stream initialisation, not
upload, and were deliberately not replayed.

For the **PCI-324** (`DEV_0003`, vtable `0x30ca8`) the same job uses the
"bank" registers instead of GPIO: `0xc0024` carries DATA0 (bit 5), nCONFIG
(bit 3) and a CONF_DONE read-back (bit 6); `0x100024` bit 0 is DCLK. Then the
37,696-byte program goes to RAM 0 and the 13,952-byte data image to
`0x80000000`. Static only; nobody has run it.

## What the earlier analysis got wrong (superseded)

The previous verdict rested on three negatives, all of which were real
observations with the wrong explanation:

1. **"No file I/O, so the `.rbf` is not read at runtime."** True, and
   irrelevant: the images are compiled into `.data`. The `.rbf` path string
   is the AppleSingle container's provenance, not a build artefact.
2. **"No port-strobe byte-feeding loop, so no passive-serial load."** The
   I/O ports are HSR/HDCR/DSPP and never carry data. The serial load is
   MMIO bit-banging on window A, which an `objdump` grep for `WRITE_PORT`
   could not find.
3. **"No `WRITE_REGISTER_BUFFER_ULONG` large enough for a bitstream."** The
   FPGA image is not block-written at all (it is clocked out a bit at a
   time), and the 28 KB program write at `fn 0x29500` was misread as a PCM
   push. `fn 0x29420` is the FPGA loader, not the audio path.

The IOCTL analysis below is still correct as far as it goes: there is indeed
no firmware IOCTL, because the driver needs none.

## Plan for the Linux driver (Phase 4.2, reopened)

- Load both images with `request_firmware()` and declare them with
  `MODULE_FIRMWARE()`; fail device start loudly in `dmesg` if either is
  missing or its hash is wrong. Suggested names match the extractor's:
  `pci424-serial-container.bin` and `pci424-program.bin`.
- The user extracts the files from their own installer with
  `tools/re/extract-firmware.py`. The images are MOTU's; they are **not**
  shipped with the driver and not committed to this repository.
- Replay steps 1–5 in `motu424_hw_init()`, then take `audio_base`,
  `mix_base` and the rest from the published block; the `audio_base=` /
  `mix_base=` module parameters become unnecessary.
- Re-run the upload on resume from suspend: the FPGA has no PROM and loses
  its configuration when the card loses power.
- `DEV_0005` (PCIe-424) uses the same code path as `DEV_0004` in the vendor
  driver, which also carries the ID string of a TI XIO2000 PCIe-to-PCI
  bridge, so a PCIe-424 is most likely a PCI-424 behind that bridge. Static
  inference only, not yet checked on a `0005` card.

## Phase 2.3 — installer contents (CONFIRMED, no card)

Extracted `MOTU Audio Installer 4.0.6.6814/SetupAudio.exe.exe` (39 MB Wix/MSI
bundle). It wraps two MSI packages as PE resources `.rsrc/RCDATA/MSI00`
(32-bit, Template `Intel;1033`) and `MSI01` (64-bit). Firmware in the
embedded LZX cabs:

- `PCIFirmware.cab` → **`HDExpress_FullImageRun.bin` only** (1218120 bytes,
  identical sha256 to `vendor/HDExpress_FullImageRun.bin`).
- `Virtex.cab`, `Media1.cab`, `PlugIns*.cab`, `VideoRes.cab`, `Redist.cab`
  contain **no** `.rbf`/bitstream/`.bin` firmware.

`altera424b.rbf` is absent from the installer **because it is inside
`MOTUAW.sys`** (see the image table above). The earlier reading of this
negative result ("so the classic card must self-configure") was the wrong
inference.

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

So the HD Express is an **ARM SoC + Xilinx Virtex FPGA**, a different
architecture from the PCI-424's C6412 + Altera-style FPGA. Given that
`MOTUAW.sys` handles `DEV_0005` on the `DEV_0004` path with the same two
embedded images, this blob most likely belongs to MOTU's HD Express **video**
product rather than to any AudioWire card, and is out of scope here.

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

There is no "load firmware" IOCTL, and none is needed: the images are in the
driver and the upload happens inside `IRP_MN_START_DEVICE`, before any IOCTL
can arrive.

## Where the bulk card-writes actually go (CONFIRMED)

All six `WRITE_REGISTER_BUFFER_ULONG` destinations were pinned by reading the
address computation just before each call:

| Site(s) | Destination card address | Payload |
|---|---|---|
| `0x29560` (`fn 0x29500`, called from bring-up step 3) | `B + 0`, DSPP = 0 | the **28,272-byte DSP program** (7,069 dwords) |
| `0x2a22d` / `0x2a291` (`fn 0x2a190`) | `[dev+0x98] + idx*4 + 0x24`, ping-pong `[+0x90]/[+0x94]` | per-**channel/bank** block |
| `0x2c540` / `0x29ad5` / `0x29a04` | **`[dev+0x9c]`** = `mix_base`, staged from the inline buffer `[dev+0x110]` (45 dwords), flushed by a dirty range `[dev+0x1c4]..[dev+0x1c8]` | **CueMix mixer coefficients** |

The first row was previously labelled "audio PCM into the window-B aperture";
that was the misreading behind the old verdict.

## To close this phase

- **Classic PCI-424 (`DEV_0004`): closed.** Images located, extracted and
  hash-verified; sequence recovered statically and reproduced on hardware
  with the card unharmed (Windows regression check passed afterwards).
- **PCI-324 (`DEV_0003`)**: sequence recovered statically (bank registers,
  EP1K30 image, two RAM images); needs a card.
- **PCIe-424 (`DEV_0005`)**: expected to be the `0004` sequence behind a
  XIO2000 bridge; needs a card (one exists in issue #1).
- Driver side: `request_firmware()` + the sequence in `motu424_hw_init()`
  (Phase 4.2).
- Curiosity, not a blocker: the format/compression of the 0004 `.rbf` data
  fork, which would identify the FPGA part. The sticker has not been lifted.
