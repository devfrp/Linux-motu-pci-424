# cuemix-studio

A cross-platform (Linux / macOS / Windows) CueMix FX-style mixing console written
in Rust with egui. It is a sibling of `tools/motu424-gui`: same control contract,
but a single native binary that also runs where GTK4 and the driver don't.

- **On Linux with the `motu424` driver**, it drives the card through
  `motu424-ctl list` / `motu424-ctl set` (found in `PATH`, or set
  `MOTU424_CTL=/path/to/motu424-ctl`), using the kcontrol names of
  [`docs/cuemix-control-map.md`](../../docs/cuemix-control-map.md). Writes are
  coalesced on a worker thread; the card is re-polled every second and the
  console rebuilds itself when the control set changes.
- **Anywhere else** (or with no card / no mixer controls yet) it runs a
  simulated PCI-424 rig — 24I/O in slot A + 1224 in slot B by default, slots
  A–D selectable (24I/O, 1224, 2408mk3, HD192), bank counts shrinking at 2x/4x
  rates — with generated test signals driving the meters.
- A second device, **828mk3 (CueMix FX with effects)**, adds the channel /
  dynamics, EQ and reverb pages, with ranges taken from MOTU's
  `CueMix FX.exe` 4.0.6.6814 (see `MECHANICS.md`, facts and addresses only).

Pages: mixes (send fader, pan, solo, mute, peak-hold meters, bus master, mix
assign), inputs (trim, pad, phase, stereo, mute, pre/post meters), outputs
(volume, mute, stereo link, patchbay switch), clock & format (clock source,
sample rate by family, locked rate, meters, talkback dim, AudioWire slots, card
link status). Undo/redo per gesture, copy/paste mix bus, named configurations
(saved in the user config dir), import/export to JSON, peak-hold time menu.

## Build & run

```sh
cd tools/cuemix-studio
cargo build --release
./target/release/cuemix-studio              # real card if motu424-ctl finds one, else simulated
./target/release/cuemix-studio --simulate   # never touch the card
./target/release/cuemix-studio --828mk3     # the effects-capable device
./target/release/cuemix-studio --screenshot out.png [--tab mixes|inputs|outputs|clock|channel|eq|reverb]
```

## Open points

- Volume kcontrols are `0..127` with a vendor default of 100; the console maps
  the top step to the fader top (0 dB, as in CueMix FX) linearly in fader
  position (estimated until the coefficient format is confirmed on the card).
- The 828mk3 trim ranges are reported by the device at run time in the
  original; the values used here are estimates.
