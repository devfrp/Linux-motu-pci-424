# CueMix control map (Phase 5.3) & the `motu424-ctl` management tool

The MOTU "management software" on Windows/macOS is **CueMix FX** — a no-latency
monitor mixer plus clock/format control that lives *on the card's DSP*, not in
the host audio stream. This note records the control set (reverse-engineered from
the shipped `CueMixFX-PCI-424.touchosc` layout, which drives MOTU's **CueMix
OSC API**) and maps it to the ALSA kcontrols the Linux driver will expose and the
`tools/motu424-ctl` userspace app will manage.

## Where this comes from

`MOTU Audio Installer 4.0.6.6814/CueMix FX TouchOSC Layouts 1.0/CueMixFX-PCI-424.touchosc`
is a zip containing `index.xml` (TouchOSC layout, names base64-encoded). Two
tab-pages — **mixes** and **inputs** — reference OSC addresses under the
`/CueMixOSCAPI` namespace. The addresses reveal the card's control model:

| OSC address (CueMix API)      | Control                          |
|-------------------------------|----------------------------------|
| `/in/fvInCS+N/in/trm`         | input N **trim / gain** (rotary) + `/str` readout |
| `/in/in/pad`                  | input **pad** (−20 dB) toggle    |
| `/in/in/psi`                  | input **phase invert** toggle    |
| `/in/in/st`                   | input **stereo-pair** toggle     |
| `/in/in/mute`                 | input **mute** toggle            |
| `/bin/…/fvInCS+N/pflS`        | input **PFL / solo-listen** LED  |
| `/mix/mute`                   | per-strip **mute** in a mix      |
| `/mix/solo`                   | per-strip **solo** in a mix      |
| `/bus/fvEB+0/mix/blS`         | **bus (mix) master level** LED   |
| a fader per strip             | input→mix **send level**         |
| a rotary per strip            | input→mix **pan**                |
| `/metersToggle`, `/metersLED` | level **meters** on/off + data   |
| labels `ATTEN`/`TALK`/`LISTEN`| talkback / listen-back           |

`fvEB` = "fader-view Edit Bus" (which mix bus is shown), `fvInCS` = "fader-view
Input Channel Strip" (which input) — these are just the TouchOSC paging cursors,
**not** hardware controls. The real model is a classic **input × mix-bus matrix**:
every input has a send (level+pan+mute/solo) into each output mix bus, plus per-
input analog conditioning (trim/pad/phase/stereo) and per-bus master level+mute.

The concrete channel/bus *counts* depend on the attached AudioWire interface
(2408, 24I/O, 896HD, HD192, …) and shrink with the sample-rate family (1x/2x/4x);
that enumeration is Phase 3.5 and is discovered at runtime.

## Planned ALSA kcontrol names

The driver (Phase 5.2/5.3, needs a card) will register these; `motu424-ctl`
already knows them and pretty-prints the ones it finds. `N` = input index,
`K` = mix-bus index (0-based), both zero-padded to 2 digits in the element name.

| kcontrol name (iface MIXER)     | type    | notes |
|---------------------------------|---------|-------|
| `Clock Source`                  | ENUM    | Internal, Word Clock, ADAT, SPDIF, AES/EBU, … |
| `Clock Rate`                    | INT, RO | locked rate in Hz (info/status) |
| `Sample Rate`                   | ENUM    | requested rate (44100…192000); `Clock Rate` stays the RO locked readout |
| `Meters`                        | BOOL    | enable level metering |
| `Input NN Trim Volume`          | INT     | analog trim, dB (per-iface range) |
| `Input NN Pad Switch`           | BOOL    | −20 dB pad |
| `Input NN Phase Switch`         | BOOL    | polarity invert |
| `Input NN Stereo Switch`        | BOOL    | pair NN/NN+1 |
| `Input NN Mute Switch`          | BOOL    | input mute |
| `Mix KK Master Volume`          | INT     | bus master fader |
| `Mix KK Mute Switch`            | BOOL    | bus mute |
| `Mix KK Input NN Volume`        | INT     | send level (matrix) |
| `Mix KK Input NN Pan`           | INT     | send pan, −100..+100 |
| `Mix KK Input NN Mute Switch`   | BOOL    | send mute |
| `Mix KK Input NN Solo Switch`   | BOOL    | send solo (PFL) |
| `Output NN Volume`              | INT     | output-pair monitor level |
| `Output NN Mute Switch`         | BOOL    | output-pair mute |
| `Output NN Source`              | ENUM    | patchbay: `Direct` or the mix bus feeding pair NN |
| `Patchbay Switch`               | BOOL    | patchbay bypass: off = every pair carries its direct PCM feed |

`Output NN` addresses **output pairs** (pair NN = physical channels 2NN+1 /
2NN+2 — AudioWire outputs are stereo pairs, and each mix bus is stereo).

The output/patchbay rows are **not** in the TouchOSC dump; they model what the
vendor CueMix console does when it assigns a mix bus to an output pair, plus a
Linux-side convenience: a *virtual patchbay* that can also feed a pair its
direct PCM channels, with `Patchbay Switch` as a global bypass (off = identity
routing, the safe default). They land in the same card-reported coefficient
block as the matrix sends (below).

These names are the contract between the kernel driver and `motu424-ctl`; keep
them in sync with `tools/motu424-ctl.c` (`KNOWN_*` tables) if either changes.

### Hardware sink (recovered)

The mixer coefficients ultimately land in a **card-reported region at device-ext
`+0x9c`** (read from the card at init, `fn 0x2c360`). The vendor driver stages
them in an inline buffer (`dev+0x110`, ~45 dwords) and flushes only the changed
range (`[dev+0x1c4]..[dev+0x1c8]`) via `WRITE_REGISTER_BUFFER_ULONG` (`fn 0x29aa0`).
So a Linux CueMix implementation writes each changed coefficient into that block
and pushes the dirty range — it does **not** need a per-control register. See
`register-map.md` (mixer coefficient region) and `fpga-upload.md` (full
destination table).

## `motu424-ctl` — the Linux CueMix

`tools/motu424-ctl` is a self-contained alsa-lib CLI (no card libraries beyond
`libasound`). It auto-locates the MOTU card (driver `motu424`) and manages the
kcontrols above — the same role CueMix FX plays on Windows/macOS.

```sh
motu424-ctl                    # status overview of the MOTU card
motu424-ctl list               # every kcontrol on the card (name/type/value)
motu424-ctl get 'Clock Source'
motu424-ctl set 'Clock Source' Internal
motu424-ctl set 'Mix 00 Input 03 Volume' 92
motu424-ctl -D hw:1 list       # target a specific card
```

Because the driver's mixer is Phase 5 (card-gated), the tool degrades cleanly:
with no MOTU card it says so; against a card that only exposes PCM (no mixer yet)
it lists what is there. It is written to light up automatically as the driver
grows its kcontrol set — no tool change needed for controls that follow the
naming table above.
