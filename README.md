# motu-pci-424

*Read this in: **English** · [Français](README.fr.md)*

A from-scratch Linux ALSA driver for the **MOTU PCI-324 / PCI-424** audio card
and its AudioWire breakout interfaces (2408, 24I/O, 828, HD192, 896HD, …).

> **Status: framework complete, hardware protocol unconfirmed.**
> The PCI / DMA / IRQ / ALSA machinery is real and complete. The MOTU
> PCI-324/424 is undocumented, so the *register map and DMA/transport protocol*
> are a documented hypothesis that must be validated against a real card. Until
> then the module loads and creates an ALSA device but will not produce correct
> audio. See [Reverse engineering](#reverse-engineering).

## Quick start

```sh
curl -fsSL https://raw.githubusercontent.com/devfrp/motu-pci-424/main/get.sh | sh
```

Installs on any distro (deps + DKMS module + tools). Details and options under
[Install](#install-any-distro).

## Layout

| Path | Role |
|------|------|
| `kernel/motu424.h` | Shared defs + the **hypothesised register map** (single source of truth) |
| `kernel/motu424_main.c` | PCI attach/detach, resource + IRQ management, interrupt handler |
| `kernel/motu424_hw.c` | **Hardware abstraction — the only file with real register semantics** |
| `kernel/motu424_pcm.c` | ALSA PCM callbacks (playback + capture) |
| `tools/motu424-probe.c` | Userspace BAR0 dumper for reverse engineering |
| `tools/motu424-ctl.c` | **CueMix-style management CLI** (clock/format + monitor mixer) over alsa-lib |
| `tools/motu424-gui` | **GTK4 GUI** control panel (a front-end over `motu424-ctl`) |
| `tools/re/` | Static-RE helpers (`vtable-scan.py`, capstone `xref.py`) |
| `get.sh` | **`curl \| sh` bootstrap** — fetch sources + run the installer |
| `install.sh` | **Cross-distro installer** (deps + DKMS + tools) |
| `ARCHITECTURE.md` | Design notes: the 3-layer split + hardware-confinement rule |
| `dkms.conf` | DKMS packaging for automatic rebuilds across kernels |

The design goal: **all uncertainty is confined to `motu424.h` + `motu424_hw.c`.**
Once the true register layout is known, only those two files change.

## Install (any distro)

One line — fetches the sources and runs the installer:

```sh
curl -fsSL https://raw.githubusercontent.com/devfrp/motu-pci-424/main/get.sh | sh
# pass installer options through:
curl -fsSL https://raw.githubusercontent.com/devfrp/motu-pci-424/main/get.sh | sh -s -- --no-dkms -y
```

Or clone and run the installer directly. It detects your package manager
(pacman/apt/dnf/yum/zypper/apk/xbps), pulls the build dependencies, installs the
module via **DKMS** (so it survives kernel upgrades) and installs the tools:

```sh
./install.sh              # deps + DKMS + tools + load  (uses sudo as needed)
./install.sh -y           # non-interactive package installs
./install.sh --no-dkms    # plain in-tree build + install instead of DKMS
./install.sh --uninstall  # remove module + tools
./install.sh -h           # all options
```

## Build (manual)

Requires kernel headers for the running kernel (Arch: `linux-rt-headers` for an
RT kernel, or `linux-headers` otherwise) and `alsa-lib` for `motu424-ctl`.

```sh
make            # build module + tools
make load       # insmod kernel/motu424.ko
dmesg | tail    # look for "MOTU PCI-4xx registered as ALSA card N"
aplay -l        # the card should appear
make unload
```

### Managing the card (CueMix for Linux)

`tools/motu424-ctl` is the userspace control app — the Linux equivalent of MOTU's
CueMix FX. It auto-locates the MOTU card and manages the driver's ALSA mixer
kcontrols (clock source, sample rate, per-input trim/pad/phase, and the input×bus
monitor matrix). Control set + naming: [`docs/cuemix-control-map.md`](docs/cuemix-control-map.md).

```sh
make tools                              # builds motu424-ctl if alsa-lib is present
./tools/motu424-ctl                     # CueMix-style status overview
./tools/motu424-ctl list                # every control
./tools/motu424-ctl set 'Clock Source' Internal
./tools/motu424-ctl set 'Mix 00 Input 03 Volume' 92
```

The mixer controls are Phase 5 (card-gated); the app is written to light up
automatically as the driver registers them, and degrades cleanly when absent.

#### Graphical panel

`tools/motu424-gui` is a GTK4 control panel — a thin front-end over
`motu424-ctl`, so the CLI stays the single source of truth. It picks the card,
then renders each control by type (switch / slider / dropdown), grouped the
CueMix way (clock & format, inputs, per-bus mix matrix).

```sh
./install.sh --gui        # installs the launcher + its runtime deps
motu424-gui               # or launch from your app menu ("MOTU CueMix")
```

Needs `python-gobject` + `gtk4` (added automatically by `--gui`). Like the CLI,
it opens against any ALSA card and fills in the full CueMix layout once the
driver exposes its kcontrols.

## Reverse engineering

The card is not present on the development machine, so the register offsets in
`kernel/motu424.h` (marked `TODO: verify`) are hypotheses. To confirm them:

1. **Identify the card.** `lspci -nn | grep -i 137a` (0x137A = Mark of the Unicorn).
   Update `PCI_DEVICE_ID_MOTU_PCI*` if the reported device ID differs.
2. **Dump BAR0** with the vendor driver *unbound*:
   ```sh
   sudo ./tools/motu424-probe
   ```
3. **Diff** a dump taken while idle against one taken while the card streams
   audio under the vendor OS (macOS/Windows) to locate the status, DMA-position
   and IRQ registers.
4. **Trace the DMA ring / event format** (channel count per sample-rate family,
   24-bit packing, endianness).
5. Update the constants in `motu424.h` and the encoders in `motu424_hw.c`.

Much of the protocol has **already** been recovered statically from the vendor
driver (no card needed) — the confirmed facts below supersede the header's
guesses; the remaining gaps are what steps 1–5 close on real hardware.

### Findings from vendor driver RE (static, no card)

Established by disassembling the vendor Windows driver `MOTUAW.sys`, using
`objdump`, `tools/re/vtable-scan.py` (resolves C++ vtable slots) and
`tools/re/xref.py` (capstone-based cross-references — callers, function bodies,
virtual-dispatch sites). These **supersede the hypothesised register map** in
`motu424.h`:

- **Two MMIO windows, not a small register file.** The card exposes a windowed
  ~24-bit "card address" space. A 32-bit address is routed by
  `(addr & 0xff800000) == 0x01800000`: if true → window A `(addr & 0x7fffff)`
  (8 MB), else → window B `(addr & 0x3fffff)` (4 MB). Per-bank ctrl/status regs
  at `0xC0024` / `0x100024` (bank+`0x24`, stride `0x40000`), reachable via
  window A at `0x18C0000` / `0x1900000`. (`0xAC44` is **not** an address — it is
  44100 decimal; all six rates appear as Hz constants `0xAC44`..`0x2EE00` in the
  divisor math.)
- **A small I/O-port BAR** (`READ/WRITE_PORT_ULONG`) holds bridge/GPIO control:
  read `+0x0` (bit 1 = IRQ pending), write `+0x0`←4 (IRQ/stream enable), write
  `+0x4`←1 (strobe/commit). A PLX-style local-bus bridge in front of the FPGA.
- **IRQ path — confirmed** (ISR = vtable `0x30cc0` slot `0x28` = `0x2bae0`):
  pending = port BAR `+0x0` bit 1; **ack = write `0x10`** to the card address in
  device-ext `+0x88`; "period elapsed" fires when a per-IRQ accumulator crosses
  `0x800`, then a DPC is queued.
- **Audio register block — confirmed** (base = a card address in device-ext
  `+0x98`): `base+0x54`←1 stream enable, `base+0x60` period increment
  (`0x10 << 2*family`), `base+0x64` rate/clock parameter, `base+0x128/12c/130`
  position counters (read then zeroed each period).
- **Audio transport is PIO into a card aperture, not host bus-master DMA.** The
  push helper (`fn 0x29420`) copies host buffers into window B via
  `WRITE_REGISTER_BUFFER_ULONG` (a `'MOTU'`-tagged 64 KB bounce buffer) and
  tracks a hardware `dmaPoint` + software `readHead`/`writeHead`/`len`.
- **FPGA firmware.** Two architectures: the classic PCI-324/424 uses an Altera
  passive-serial FPGA (`altera424b.rbf`, **not** present in the 4.0.6 vendor
  installer — likely self-configured from onboard flash); the PCIe **HD Express**
  uses an ARM SoC + Xilinx Virtex, shipped as `HDExpress_FullImageRun.bin` (a
  24-byte-header container, checksum-verified). `MOTUAW.sys` has no file-I/O, so
  a Linux driver will supply firmware via `request_firmware()` where needed.
- **CueMix control set** decoded from the shipped `CueMixFX-PCI-424.touchosc`
  (MOTU's CueMix OSC API): an input×mix-bus monitor matrix + per-input analog
  conditioning + clock/format. Drives `tools/motu424-ctl` — see
  [`docs/cuemix-control-map.md`](docs/cuemix-control-map.md).

Detailed, evidence-tagged write-ups live in `docs/`:
[`register-map.md`](docs/register-map.md), [`transport.md`](docs/transport.md),
[`fpga-upload.md`](docs/fpga-upload.md),
[`cuemix-control-map.md`](docs/cuemix-control-map.md),
[`vendor-driver-map.md`](docs/vendor-driver-map.md).

Still open (need the card, Ghidra-grade type recovery, or deeper RE): the
clock-source select register/bits and the `base+0x64` parameter encoding, the
runtime numeric values of the audio base / ack addresses, and the FPGA upload
handshake.

The full phased roadmap from here to a 100% working driver lives in
[`docs/REVERSE_ENGINEERING_PLAN.md`](docs/REVERSE_ENGINEERING_PLAN.md).

## License

GPL-2.0-or-later. Kernel modules linking GPL-only ALSA symbols must be GPL.
