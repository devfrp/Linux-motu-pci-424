# motu-pci-424
**For PCIe please refeer to : https://github.com/devfrp/Linux-motu-pci-424/tree/branche-PCIe**

*Read this in: **English** · [Français](README.fr.md)*

🌐 **Website:** <https://devfrp.github.io/Linux-motu-pci-424/>

A from-scratch Linux ALSA driver for the **MOTU PCI-324 / PCI-424** audio card
and its AudioWire breakout interfaces (2408, 24I/O, 828, HD192, 896HD, …).

> **Status: the card boots under Linux; audio has not played yet.** The
> PCI / IRQ / ALSA machinery is complete. On a real PCI-424 the driver's load
> sequence (FPGA image through the DSP's GPIO, DSP program, mailbox) has been
> run with `tools/motu424-bringup`, and the driver now does the same when it binds
> with `request_firmware()` (see [Firmware](#firmware)). Audio is DSP
> bus-master DMA from a host ring; the stream path is written but untested,
> and stays off until the ring's frame size is known (`motu424.frame_bytes=`).
> Playback only, 48 kHz only, PCI-424 (`137a:0004`) only for now.

## Quick start

```sh
curl -fsSL https://raw.githubusercontent.com/devfrp/Linux-motu-pci-424/main/get.sh | sh
```

Installs on any distro (deps + DKMS module + tools). Details and options under
[Install](#install-any-distro).

## Layout

| Path | Role |
|------|------|
| `kernel/motu424.h` | Shared defs + the **RE'd hardware model** (single source of truth) |
| `kernel/motu424_main.c` | PCI attach/detach, resource + IRQ management, interrupt handler |
| `kernel/motu424_hw.c` | **Hardware abstraction — the only file with real register semantics** |
| `kernel/motu424_pcm.c` | ALSA PCM callbacks (playback; capture once the ring layout is known) |
| `tools/motu424-probe.c` | Userspace BAR enumerator/dumper (windowed model) for reverse engineering |
| `tools/motu424-portregs.py` | Read-only decode of the three I/O-port registers (HSR/HDCR/DSPP, TI SPRU581C) |
| `tools/motu424-bringup.c` | **Bring-up replay** (dry-run by default): FPGA load via DSP GPIO, DSP program load, DSPINT, mailbox poll |
| `tools/motu424-ctl.c` | **CueMix-style management CLI** (clock/format + monitor mixer) over alsa-lib |
| `tools/motu424-gui` | **GTK4 mixing console** in the CueMix FX style (a front-end over `motu424-ctl`) |
| `tools/re/` | Static-RE helpers (`vtable-scan.py`, capstone `xref.py`, `extract-firmware.py`, C64x `dis-c64x.py`) |
| `get.sh` | **`curl \| sh` bootstrap** — fetch sources + run the installer |
| `install.sh` | **Cross-distro installer** (deps + DKMS + tools) |
| `ARCHITECTURE.md` | Design notes: the 3-layer split + hardware-confinement rule |
| `CLEANROOM.md` | Clean-room & provenance statement (legal basis, RE method) |
| `docs/index.html` | Project website (GitHub Pages landing page) |
| `dkms.conf` | DKMS packaging for automatic rebuilds across kernels |

The design goal: **all uncertainty is confined to `motu424.h` + `motu424_hw.c`.**
Once the true register layout is known, only those two files change.

## Install (any distro)

One line — fetches the sources and runs the installer:

```sh
curl -fsSL https://raw.githubusercontent.com/devfrp/Linux-motu-pci-424/main/get.sh | sh
# pass installer options through:
curl -fsSL https://raw.githubusercontent.com/devfrp/Linux-motu-pci-424/main/get.sh | sh -s -- --no-dkms -y
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

Under `curl | sh`, the installer detects the absence of a terminal and
automatically switches package installs to non-interactive mode; sudo still
prompts for your password on the terminal as usual.

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

#### Graphical console

`tools/motu424-gui` is a GTK4 **CueMix FX-style mixing console** — a front-end
over `motu424-ctl`, so the tested CLI stays the single source of truth. It
rebuilds the CueMix model from the kcontrol names and renders it like the real
console: an Inputs tab (channel strips combining per-input analog conditioning
— trim, pad, phase, stereo pairs — with each channel's send into the console's
one mix: fader, peak-hold meter, rotary pan pot, mute/solo/gang, with the mix
master pinned on the right outside the scroll), an Outputs tab (mono monitor
strips, stereo-linkable like the inputs), a Patchbay tab (optional, no cords
by default — drawn like a real normalled bay: every patchable output sits at
its normal, direct PCM feed; main outs are hard-normalled to the system's
stereo program instead and aren't patchable, so desktop sound always reaches
the monitors; drag virtual cables to patch anything else, hovering a jack
tells you what it carries, "Unpatch all" pulls every cord in one undoable
step, and a global switch bypasses the bay back onto the normals), a Clock &
format tab, and a Diagnostics tab that works even with no card and no driver
loaded. On cards that report a hardware DSP insert engine (PCIe-424 / HD
Express — not yet attached by this driver, so it's a preview), each strip
also gets a virtual FX insert: a local 3-band EQ curve, not wired to any
kcontrol yet.

The whole layout adapts to the converters attached to the PCI-424's AudioWire
slots: the driver names every channel per slot and bank (analog, ADAT, TDIF,
AES/EBU, main out, phones — see `docs/cuemix-control-map.md`), and the console
regroups its strips and patchbay jacks under "slot · model — bank" headers.
A 24I/O brings 24 analog I/O, a 1224 brings 8 analog I/O plus an AES/EBU pair
and main outs — that combination is exactly what `--demo` simulates, and
`--rig 24io,2408` hangs other converters (24io, 1224, 2408, hd192) off the
four AudioWire slots instead. The
console also follows the card over time: when the registered control set
changes (module load/unload, converters hot-plugged, channel counts shrinking
in the 2x/4x rate families) it rebuilds itself on the next poll, keeping the
tab you were on — flip the sample rate in `--demo` to watch the shrink live.

On top of the basics: stereo-pair and gang linking, JSON mix snapshots
(Ctrl+S / Ctrl+O), Ctrl+Z undo of mix-wide
operations, editable channel names, and a "Shortcuts & tips" dialog on F1.
Control writes are coalesced through a worker thread and the hardware is
re-polled in the background, so the console follows changes made elsewhere
(alsamixer, a second instance) without fighting the control you are touching.

```sh
./install.sh --gui        # installs the launcher + its runtime deps
motu424-gui               # or launch from your app menu ("MOTU CueMix")
motu424-gui --demo        # full console against a synthetic 24I/O + 1224 rig
```

Needs `python-gobject` + `gtk4` (added automatically by `--gui`). The mixer
kcontrols are Phase 5 (card-gated): with no MOTU card the console has nothing
to populate — preview it with `--demo`; on real hardware it fills in
automatically once the driver registers its controls.

##### Screenshots

`--demo` against the synthetic 24I/O + 1224 rig, one tab at a time:

**Inputs** — channel strips scrolled from slot A (24I/O, analog) into slot B
(1224, AES/EBU), with the mix master pinned on the right:

<img src="docs/screenshots/inputs-1-slot-a.png" width="49%"> <img src="docs/screenshots/inputs-2-slot-b.png" width="49%">

**Outputs** — mono monitor strips, scrolled across the 24I/O's analog outs:

<img src="docs/screenshots/outputs-1.png" width="49%"> <img src="docs/screenshots/outputs-2.png" width="49%">

**Patchbay** — no cords by default; every jack sits at its normal:

![Patchbay tab](docs/screenshots/patchbay.png)

**Clock & format** and **Diagnostics** (the latter works with no card and no
driver loaded — this is a real report from the machine these were taken on):

<img src="docs/screenshots/clock-format.png" width="49%"> <img src="docs/screenshots/diagnostics.png" width="49%">

## Firmware

The PCI-424's FPGA has no configuration PROM, so the host loads it, and the
DSP program, every time the driver binds (as the Windows driver does). Both
images belong to MOTU and are **not** shipped here. Extract them from your own
copy of the vendor driver and install them:

```sh
python3 tools/re/extract-firmware.py path/to/MOTUAW.sys   # 32-bit driver; writes vendor/firmware/ (git-ignored)
sudo install -Dm644 vendor/firmware/pci424-serial-container.bin \
    /lib/firmware/motu424/pci424-serial-container.bin
sudo install -Dm644 vendor/firmware/pci424-program.bin \
    /lib/firmware/motu424/pci424-program.bin
```

The driver checks both by length and SHA-256 and refuses to probe if either is
missing or different. The load took about 15 s from userspace on the test card (894k GPIO writes), then
`dmesg` shows `DSP running: audio_base=0x6fac ...`.

## Reverse engineering

The hardware model comes from the vendor Windows driver `MOTUAW.sys` (static
RE with `objdump`, `tools/re/vtable-scan.py`, `tools/re/xref.py`), from the
card's own DSP program (`tools/re/dis-c64x.py`), and from runs on a real
PCI-424 in issue #2:

- **The board** is a TI **TMS320C6412 DSP** in PCI boot mode plus an FPGA with
  no PROM. The I/O BAR is the DSP's Host Port Interface (HSR/HDCR/DSPP); the
  8 MB BAR (window A) is the DSP's peripheral registers, the 4 MB one
  (window B) its memory, with internal RAM at `0..0x3ffff`.
- **Bring-up** (vendor `fn 0x2c150`, run on hardware): WARMRESET, the FPGA
  image clocked out through the DSP's GPIO, the 28,272-byte C64x program
  written into internal RAM, DSPINT, then the program publishes `audio_base`
  in the mailbox at `0x3fffc` (`0x6fac`).
- **The audio block** at `audio_base` is the DSP program's global data:
  `mix_base` at `+0`, the host ring address at `+0x5c`, the DSP's position at
  `+0x2c`, start/stop at `+0x54`/`+0xf4`, frames per interrupt at `+0x60`.
- **Transport is DSP bus-master DMA.** The host hands the DSP a coherent ring
  (512 KB on Windows); the DSP fetches audio from it and raises an interrupt
  every 64 frames. IRQ pending = HSR bit 1; ack = `0x10` (INTRST) to the DSP's
  RSTSRC register.
- **CueMix control set** decoded from the shipped `CueMixFX-PCI-424.touchosc`
  (MOTU's CueMix OSC API): an input×mix-bus monitor matrix + per-input analog
  conditioning + clock/format. Drives `tools/motu424-ctl` — see
  [`docs/cuemix-control-map.md`](docs/cuemix-control-map.md).

Detailed, evidence-tagged write-ups live in `docs/`:
[`register-map.md`](docs/register-map.md), [`transport.md`](docs/transport.md),
[`fpga-upload.md`](docs/fpga-upload.md),
[`cuemix-control-map.md`](docs/cuemix-control-map.md),
[`vendor-driver-map.md`](docs/vendor-driver-map.md).

Still open: the frame layout on the host ring (bytes per frame, sample
format, channel order), where capture lands on the host side, rate changes
(the vendor relock sequence), and the PCI-324 / PCIe-424 load paths.

The full phased roadmap from here to a 100% working driver lives in
[`docs/REVERSE_ENGINEERING_PLAN.md`](docs/REVERSE_ENGINEERING_PLAN.md).

## License

GPL-2.0-or-later. Kernel modules linking GPL-only ALSA symbols must be GPL.
