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
