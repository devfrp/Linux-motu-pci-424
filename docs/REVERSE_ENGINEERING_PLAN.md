# Reverse-engineering plan — MOTU PCI-324/424, from scratch to a 100% Linux driver

Goal: a clean-room-documented, fully functional out-of-tree ALSA driver for the
MOTU PCI-324 / PCI-424 (and PCIe "HD Express") AudioWire host cards, driving the
breakout interfaces (2408, 24I/O, 828, HD192, 896HD, …), with correct clocking,
multi-rate playback + capture, and a mixer where feasible.

Legal basis: reverse engineering **for interoperability** with hardware we own.
Only *facts* (offsets, formats, sequences) enter our GPL sources; the vendor
binaries in `vendor/` are never redistributed (`.gitignore`).

This plan is phased. Each task lists **method**, **deliverable**, and **exit
criterion**. Static RE (phases 0–3) needs no hardware; phases 4–6 need a card.
Tick items as they land and fold confirmed facts into `kernel/motu424.h`,
`kernel/motu424_hw.c`, and the README RE section.

---

## Status snapshot (what is already established)

Recorded in `kernel/motu424.h` and the README; see also the RE memory notes.

- **PCI identity — CONFIRMED** (`MOTUAW.inf`): `VEN_137A`, `DEV_0003/0004/0005`
  (PCI-324 / PCI-424 / PCIe-424). `DEV_0006/0007` are out of scope (video/variant).
- **Access model — CONFIRMED** (`MOTUAW.sys`): windowed ~24-bit card-address
  space over two MMIO regions + a small I/O-port BAR; `READ/WRITE_REGISTER_ULONG`
  and `WRITE_REGISTER_BUFFER_ULONG`.
- **Transport — CONFIRMED shape**: PIO into a card aperture (window B) with a HW
  `dmaPoint` + SW `readHead/writeHead/len` ring. **Not** a host bus-master ring.
- **FPGA — CONFIRMED, two architectures**: classic PCI-324/424 = Altera
  passive-serial FPGA (`altera424b.rbf`, *not* in the 4.0.6 installer — may
  self-configure from flash). PCIe HD Express (`DEV_0005`) = ARM SoC + Xilinx
  Virtex, shipped as `HDExpress_FullImageRun.bin` (container format decoded in
  `docs/fpga-upload.md`).

Everything below turns these shapes into exact, implementable semantics.

---

## Phase 0 — Tooling & ground truth (no card)

- [x] **0.1 Better disassembler.** **[DONE, lightweight]** — rather than
  Ghidra/rizin (not installable here), built `tools/re/xref.py` (capstone-based,
  runs in a venv). It does a resume-past-data linear sweep of `.text` and answers
  `callers <va>`, `fn <va>`, `imm <val>`, `mem <disp>`, and `vcalls`. *Exit met:*
  can list direct callers of any VA and locate virtual-dispatch sites by slot.
  **Caveat:** it has no type recovery, so C++ object *identity/layout* (which
  vtable a `this` register holds) still must be inferred by hand and stays
  error-prone — the reason the fine-grained rate encoding below is still OPEN.
- [ ] **0.2 Symbol/type recovery.** MOTUAW.sys is C++ with RTTI-ish strings and
  source names (`PCI424Driver.cpp`, `PCI424NanoDriver.cpp`, `AW2408.cpp`). Map
  the class layout of the device-extension struct (fields already seen: `+0x78`
  window-B base, `+0x7c` window-A base, `+0x80` I/O-port base, `+0x84/+0xa8/+0xb0`
  DMA descriptor state). *Deliverable:* a commented struct in `docs/`.
- [ ] **0.3 Role of each binary.**
  - `MotuBus.sys` — PCI bus/function enumeration & child device creation.
  - `MOTUAW.sys` — the audio function driver (register + DMA + FPGA logic).
  - `MAWWAVE.sys` — legacy WDM/kmixer wave shim (low RE priority).
  *Exit:* one paragraph each in `docs/vendor-driver-map.md`. **[DONE]** — see
  `docs/vendor-driver-map.md` (accessor VAs, import counts, ISR-is-virtual note).

## Phase 1 — Register & control-path semantics (no card)

- [ ] **1.1 Enumerate every card-address constant.** Extend the scripted scan:
  grep the disasm for immediates feeding `WRITE/READ_REGISTER_ULONG`, resolve
  the window (A if `& 0xff800000 == 0x01800000`), and tabulate. Known so far:
  `0xC0024`, `0x100024` (bank stride `0x40000`), `0x18C0000`, `0xAC44`.
  *Deliverable:* `docs/register-map.md` (addr → inferred role → evidence).
  **[DONE for statics]** — `docs/register-map.md` written. Key correction:
  `0xAC44` is 44100 (a rate constant), not an address. Clock/IRQ/dmaPoint
  register encodings remain OPEN (behind C++ vtables; need a card or xref RE).
- [ ] **1.2 Reset / init sequence.** Trace `AddDevice`→`StartDevice` (the
  `IoConnectInterrupt`, `MmMapIoSpace`, `IoGetDmaAdapter` call site at ~`0x28e94`)
  to recover the ordered register writes that bring the card up. *Exit:* a
  numbered power-on sequence.
- [ ] **1.3 I/O-port BAR meaning.** Decode the port block at dev-ext `+0x80`
  (read `+0x0`; write `+0x4`←1, `+0x8`). Likely a PLX/local-bus bridge: identify
  the bridge (readback IDs) and whether `+0x4`/`+0x8` are FPGA-config strobes
  (nCONFIG/DATA/DCLK) vs. IRQ control. *Exit:* each port dword named.
  **[PARTIAL]** decoded in `docs/register-map.md`: `+0x0` R (bit 1 =
  ready/pending), `+0x4` W`1` (strobe/kick), `+0x8` W (init value). Bridge
  identity (PLX part) and strobe-vs-IRQ role still need confirmation.

## Phase 2 — FPGA upload protocol (no card)

The single biggest unknown that gates *any* audio.

- [ ] **2.1 Locate the upload routine.** Find the code that references the
  `altera424b.rbf` string (file off `0x2e877`) and follow it to the byte-feeding
  loop. Determine transport: passive-serial via the I/O-port strobes, or a
  bulk `WRITE_REGISTER_BUFFER_ULONG` into a config window.
  **[LARGELY DONE]** `docs/fpga-upload.md`: `MOTUAW.sys` has no file-I/O and no
  xref to the `.rbf` string — bytes arrive from user mode via IOCTL, now fully
  mapped. `DriverEntry@0x63000` sets `MajorFunction[DEVICE_CONTROL]=0x21d40`;
  dispatcher `0x242b0` computes `func=((code>>2)&0xfff)-0x800` and switches
  (`0x241b0`, 4 entries) over **four IOCTLs 0x801-0x804**: submit-buffer / start-
  stop / control / register-callback. **No dedicated firmware opcode** — the
  bitstream is pushed through the generic `0x801` buffer channel. The port
  `+0x4`/`+0x8` strobes are IRQ/enable, **not** a passive-serial feed. Remaining:
  the card address the submitted bytes land at (the vector consumer / stream
  worker → `WRITE_REGISTER_BUFFER`).
- [ ] **2.2 Decode the handshake.** nCONFIG assert, poll nSTATUS/CONF_DONE,
  bit/byte order (Altera passive-serial is LSB-first), post-config init clocks.
  *Deliverable:* `docs/fpga-upload.md` pseudocode. **[OPEN]** — needs the
  installer's real `.rbf` + a real disassembler (Ghidra/rizin) or a card.
- [x] **2.3 Source the bitstream.** **[DONE for statics]** — see
  `docs/fpga-upload.md`. Extracted `SetupAudio.exe.exe` (Wix/MSI → embedded cabs).
  `altera424b.rbf` is **not present anywhere** in the 4.0.6 installer; the only
  PCI firmware is `HDExpress_FullImageRun.bin` (PCIe `DEV_0005`), now fully
  characterised as a 24-byte-header container = **ARM firmware + Xilinx Virtex
  bitstream + config records**, sum32 payload checksum verified. Open: obtain
  `altera424b.rbf` from an older PCI-era release, or confirm the classic card
  self-configures from flash (leading hypothesis).

## Phase 3 — Audio transport, clock & IRQ semantics (no card)

- [ ] **3.1 Ring/aperture geometry.** From the `WRITE_REGISTER_BUFFER_ULONG`
  sites (e.g. `0x29560`, `0x29a04`) and the `ReadHead out of bounds` guard,
  recover: aperture base/size in window B, frame stride, channel packing
  (24-bit / 3 bytes, endianness), and how `dmaPoint`/`readHead`/`writeHead`/`len`
  advance. *Deliverable:* the ring state machine in `docs/transport.md`.
  **[PARTIAL]** `docs/transport.md`: PIO push confirmed (≤64 KB bursts, dword
  units, window-B aperture, 'MOTU'-tagged bounce buffer); ring is SW
  readHead/writeHead/len over the aperture, `len` power-of-two in dwords.
- [ ] **3.2 `dmaPoint` register.** Find the read that yields the HW play/capture
  position (the ALSA `.pointer` source). *Exit:* its card address + units.
  **[OPEN]** — `dmaPoint` is read by the caller and passed into the bounds guard
  (`0x25500`); its concrete card address was not pinned. Units are dwords.
- [ ] **3.3 Clock & sample-rate encoding.** Decode the writes that select
  internal/word/ADAT/SPDIF clock and the 1x/2x/4x rate family, and how the
  fixed-frame channel count shrinks with rate. Correlate with the `0x*0024`
  bank registers. *Deliverable:* rate → register-value table. **[PARTIAL]** —
  six rates confirmed as Hz constants (`0xAC44`..`0x2EE00`); base rate validated
  as 44100/48000 only (`0x191a0`). Rate *family* (1x/2x/4x) programs
  `audiobase+0x60 = 0x10<<(2*family)` (method `0x295e0`) and `audiobase+0x64` (a
  param, `0x297f0`). Enable = `audiobase+0x54 <- 1` (`0x29660`). STILL OPEN: the
  clock-source select register/bits and the `+0x64` param encoding.
- [ ] **3.4 IRQ path.** From the `IoConnectInterrupt` ISR/DPC, recover the IRQ
  status register, the ack (w1c?) write, and which bit means "period elapsed".
  *Exit:* status/ack addresses + bit meanings. **[DONE — vtable resolved by
  hand]** ISR = vtable `0x30cc0` slot `0x28` = `0x2bae0`. **Pending = port BAR
  `+0x0` bit 1** (`(read>>1)&1`). **ACK = WRITE_REGISTER(devext+0x88, 0x10)**.
  Period-elapsed = per-IRQ accumulator `[+0x264] += [+0x260]` crossing `0x800`,
  then a DPC is queued via `[[devext+0x38]]`. Enable = `WRITE_PORT(+0x0,4)` +
  `WRITE_PORT(+0x4,1)` (method `0x298e0`). See `docs/register-map.md`.
- [ ] **3.5 Breakout (AudioWire) enumeration.** How the host learns which
  interface(s) are attached and their channel maps (`AW2408.cpp`). Enough to set
  channel counts correctly; full per-interface control can be a later milestone.

## Phase 4 — Rearchitect the Linux driver to the real model (needs card to verify)

Keep the clean 3-layer split; confine all new hardware truth to `motu424.h` +
`motu424_hw.c` (see `ARCHITECTURE.md`).

- [ ] **4.1 Register accessors.** Replace flat-offset helpers with the windowed
  `card_addr → (window, base, masked offset)` scheme; add `motu424_rd32/wr32`
  and a `wr_buf` for aperture writes.
- [ ] **4.2 Firmware upload.** Add `request_firmware("altera424b.rbf")` +
  `motu424_hw_fpga_load()` in the init path, before ALSA card creation; fail
  cleanly (and audibly in dmesg) if firmware is absent.
- [ ] **4.3 Transport rewrite.** Replace the bus-master `dma_addr` assumption in
  `motu424_hw_stream_prepare/start/stop/pointer` with the PIO-aperture ring:
  copy period data into window B, drive `readHead/writeHead`, read `dmaPoint`
  for `.pointer`. Revisit `snd_pcm_set_managed_buffer_all` vs. an indirect/copy
  PCM (`SNDRV_PCM_INFO_..` / `copy` callback) since the card isn't DMA-ing host RAM.
- [ ] **4.4 Clock/rate.** Implement `motu424_hw_set_rate()` from the 3.3 table,
  including the per-rate channel-count change.
- [ ] **4.5 IRQ.** Implement `motu424_hw_irq_ack()` against the real status/ack
  registers; keep the devm teardown ordering (quiesce action after IRQ request).

## Phase 5 — ALSA feature completeness (needs card)

- [ ] **5.1** Playback + capture at 44.1/48 (1x) first; then 88.2/96 (2x),
  176.4/192 (4x).
- [ ] **5.2** Clock-source control (internal/word/ADAT/SPDIF) as an ALSA kcontrol
  + rate reporting; `SNDRV_PCM_INFO_JOINT_DUPLEX` if the card requires locked
  in/out rates.
- [ ] **5.3** Channel/interface mapping controls; optional CueMix-style mixer
  (stretch — the TouchOSC/CueMix layouts in the installer hint at the control set).
  **[PARTIAL, no card]** — decoded `CueMixFX-PCI-424.touchosc` (MOTU CueMix OSC
  API) into the full control set and mapped it to planned ALSA kcontrol names in
  `docs/cuemix-control-map.md`. Built the userspace management app
  `tools/motu424-ctl` (alsa-lib): auto-finds the MOTU card, `list`/`get`/`set`/
  `status` over the mixer kcontrols; verified end-to-end against a generic card.
  Remaining (card): the driver actually registering those kcontrols in
  `motu424_hw.c` + the runtime channel/bus enumeration (Phase 3.5).
- [ ] **5.4** Suspend/resume (re-upload FPGA on resume if needed).

## Phase 6 — Validation & hardening (needs card)

- [ ] **6.1 Empirical bring-up** on a real card: `make load`, `dmesg`,
  `aplay -l`/`arecord -l`, then a real playback/capture loopback with a known
  signal; verify no `XRUN` storms and correct pitch (rate correctness).
- [ ] **6.2 Register diffing** with `tools/motu424-probe` (driver unbound): dump
  idle vs. streaming to confirm the `dmaPoint`/status offsets from phase 3.
- [ ] **6.3 Soak & edge cases**: all rates, both directions simultaneously,
  start/stop churn, unplug/replug of the breakout, module reload.
- [ ] **6.4 Cleanup for upstream**: checkpatch, GPL headers, `MODULE_FIRMWARE()`,
  a `Documentation/` note, and a clean-room statement (facts-only from RE).

---

## Immediate next actions (do these first, no card required)

1. Phase 2.1–2.2 — decode the FPGA upload (gates everything). 
2. Phase 3.3 + 3.4 — rate/clock table and the IRQ status/ack registers.
3. Phase 1.1 — finish the full `docs/register-map.md` table.

Only after 1–3 are documented does the phase-4 rewrite become low-risk; until
then the existing framework stays as-is with its constants flagged untrustworthy.
