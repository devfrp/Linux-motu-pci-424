# CueMix FX – mechanics sheet

Source program: `CueMix_FX.exe` 4.0.6.6814 (32-bit MSVC, stripped), found inside
`SetupAudio.exe` → embedded `MSI01` (x64 MSI) → `Media1.cab`.
Ghidra could not open it; addresses below come from the built-in lifter and objdump,
class names from the program's own RTTI (`.?AVValueFaderFX@@` etc.).

## Fader (ValueFaderFX, vtable 0x99c4bc)
- Gain is the square of the fader position: `gain = p²` (0x44f0b0), `p = sqrt(gain)` (0x44f090 → sqrt at 0x423140).
- Hardware word: `hw = p · 32768` (0x44f0d0 / 0x44f110, constant 32768.0 at 0x9af270).
- Console readout: formatter object (code table 0x99bfcc, installed by the fader constructor 0x44efd0) → 0x4770c0 → 0x422fa0: `dB = 20·log10(gain)` printed `%0.1f`, or `inf` with a `-` prefix when not finite (strings 0x99a9d0 / 0x99a9d8 / 0x99a994), then the suffix " dB" (0x99c70c). No plus sign.
- OSC readout: `dB = 40·log10(hw/32768)`, shows `-inf` below **-90 dB** (0x5a7250: 2^-15 at 0x9af030, 40.0 at 0x996bf0, -90.0 at 0x9af410), `%+.1f`/`%+.0f` for |dB|≥0.1.
- Top of the fader: **0 dB**. The base value setup 0x472b20 sets min 0 (+0x1c) and max 1 (+0x20, `fld1`), and the fader constructor 0x44efd0 never changes them, so gain ≤ 1. This matches the hardware word topping out at 32768 (0x44f0d0) and the OSC readout's 2^-15 scale (0x5a7250).
- Default: 0 dB (estimated).

## Pan (ValuePanFX, vtable 0x99eec4)
- Internal −1 … +1, normalised `(pan+1)/2` (0x4751e0), hardware `(pan+1)·128·0.5` → 0…128 (0x475280, 128.0 at 0x9ade60).
- Readout: ValuePanFX slot 20 = 0x475650 prints the −1 … +1 value with `%0.2f` (0x99ea5c). Pan law (equal-power) **estimated**.
- Balance/width preference is a 0/1 switch (0x44f620).

## Trim (ValueTrimFX, vtable 0x99c534)
- Range is read from the device (virtual at +0x140); hardware word `64 + (x−min)·191/(max−min)` → 64…255 (0x44f1d0 / 0x44f210, 191.0 at 0x9af3d8, 64.0 at 0x9af3e0).
- Range used here: **estimated** 0 … +53 dB for mic inputs, −96 … +22 dB for line inputs.

## EQ (ValueFreq / ValueGain / ValueQ)
- Frequency 20 … 20 000 Hz, logarithmic: `n = (log f − log 20)/(log 20000 − log 20)` (0x474e70 → 0x474980). Readout `%.1f Hz` / `%.3f kHz` (0x476780).
- Gain −20 … +20 dB linear (0x474fa0, -20.0 at 0x9af398, 40.0 at 0x996bf0). Readout `%.2f dB`.
- Q 0.01 … 3.00 linear (0x4750f0, 0.01 at 0x9af0f0, 2.99 at 0x9af390). Readout `%.2f`.
- Bands (TouchOSC mk3 layout): HPF (6/12 dB/oct), low (I / II / shelf), lo-mid, mid, hi-mid (I / II), high (I / II / shelf), LPF (6/12). Default band freqs **estimated** (60, 250, 1k, 4k, 12k Hz).

## Compressor / leveler
- Threshold −48 … 0 dB (ValueThreshold 0x4746c0, -48.0 at 0x9af3a8).
- Ratio 1 … 10 : 1 (ValueRatio 0x474650, 9.0 at 0x9af3b0), readout `%0.1f:1` (0x477a50).
- Attack 10 … 100 ms (ValueCompAttack ctor 0x44ec75, 10.0 at 0x997ecc, 100.0 at 0x997e90).
- Release 10 … 2000 ms (ValueCompRelease ctor 0x44ed45, 2000.0 at 0x9980e8).
- Compressor trim −6 … 0 dB (ValueCompTrim ctor 0x44ee0f, −6.0 at 0x9980f0).
- Leveler gain reduction 0 … 100 %, makeup gain 0 … 100 % (scalers at 0x456693/0x456793, 100.0 at 0x997e90). Peak/RMS detector, comp/limit leveler mode.

## Reverb (parameter ids 0x7000004 … 0x700000f, set up in 0x4546c0 … 0x4551a0)
| param | range | default |
|---|---|---|
| pre-delay | 0 … 100 ms | 0 |
| shelf frequency | 1 000 … 20 000 Hz | 16 000 |
| shelf cut | −40 … 0 dB | 0 |
| reverb time (ValueReverbPrimaryTime 0x4768a0) | 100 … 60 000 ms, curve centre 2 000 | 2 000 (estimated default) |
| reflections size | 50 … 400 % | 100 |
| reflections level | 0 … 1 (linear) | 1 |
| time low / mid / high | 0 … 100 % | 100 / 50 / 25 |
| crossover low | 100 … 20 000 Hz | 500 |
| crossover high | 1 000 … 20 000 Hz | 2 000 |
| width | −1 … +1 | +1 |
- Readouts (0x4769d0): when the value is ≥ 1000 ms (0x9af1a8) it is divided by 1000 and printed `%0.2f sec`, otherwise `%0.0f ms`.
- Early reflection sets A–E; split point "mixes" / "outputs".
- Reverb send / return readouts in dB via 0x422fa0 (`-inf`).

## Meters
- Peak hold choices: Off, 2 Seconds, 4 Seconds, 10 Seconds, 1 Minute, 5 Minutes, Infinite (strings near "Peak Hold Time").
- Pre/post-FX input meters and RMS option (kInputLevelPreRms / kInputLevelPostRms / kOutputLevelRms, 0x469fe0).
- TouchOSC meter LEDs split at 0.25/0.5/0.75/0.95 of full scale (layout file).

## Structure (TouchOSC mk3 layout + strings)
- Tabs: mixes, inputs, outputs, channel / dynamics, eq, reverb.
- Mix strip: mute, solo, pan, fader with readout, focus; bus: master fader, bus mute, mix assign (output), reverb send/return.
- Global: MONITOR level, TALK, LISTEN (talkback / listenback).
- Edit: Undo/Redo, Copy/Paste Mix Bus, Edit Channel/Bus/Output Name.
- Configurations: Load, Factory Defaults, Create New, Save, Save To, Delete, Import, Export (file `CueMixConfigurations.xml`).
- 828mk3 inputs (estimated from channel-name strings): Mic 1-2, Analog 1-8, S/PDIF 1-2, ADAT A 1-8; 8 mix buses (estimated).

## Rebuild-only
- No MOTU driver exists outside Windows, so the rebuild drives a simulated 828mk3 whose inputs carry generated test signals.

## PCI-424 rig (from the person's own Linux driver project, Linux-motu-pci-424)
- Default device is a PCI-424 with a 24I/O in slot A and a 1224 in slot B (their `motu424-gui` demo rig); slots A–D accept 24I/O, 1224, 2408mk3, HD192.
- Analog/ADAT/TDIF banks halve at 2x rates and quarter at 4x (their `demo_controls`).
- On Linux the console reads and writes the card through their `motu424-ctl list` / `set`, using the kcontrol names in their `docs/cuemix-control-map.md`.
- Volume controls are 0…127 with a vendor default of 100; the rebuild maps the top step 127 to the fader top (0 dB, see above) linearly in fader position (estimated). Pan is −100…+100. Trim range comes from the control (demo 0…60 dB).
- Patchbay Switch ships off: outputs sit at their normals (Main = computer playback). Assigning a mix to an output turns it on.
- The PCI-424 has no CueMix effects, so EQ / dynamics / reverb tabs appear only for the 828mk3 device.

## Generic value readout
- Value slot 20 = 0x473440: if the value carries a formatter object (+0xc8) it is used, otherwise the number is printed `%0.2f` (0x99ea5c).
- Compressor trim formatter (code table 0x99bffc) → 0x477ec0: `%0.1f dB` (0x99f34c).
