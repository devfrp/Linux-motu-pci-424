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

- [x] **0.1 Better disassembler.** **[DONE]** Started lightweight with
  `tools/re/xref.py` (capstone-based, runs in a venv): a resume-past-data linear
  sweep of `.text` answering `callers <va>`, `fn <va>`, `imm <val>`,
  `mem <disp>`, and `vcalls` — enough to list direct callers of any VA and
  locate virtual-dispatch sites by slot, but with **no type recovery**, so C++
  object identity/layout (which vtable a `this` holds) had to be inferred by
  hand. **Update (2026-07-11): `rz-ghidra` is in fact available** (Arch
  `extra/rizin` + `extra/rz-ghidra`); its `pdg` decompiler *does* the type
  recovery `xref.py` lacked and resolved the interface-object dispatch that
  closed Phase 3.5. Both tools are now in play — `xref.py` for fast callers/xref
  queries, `rz-ghidra` when a function's C++ object graph must be decompiled.
- [~] **0.2 Symbol/type recovery.** **[CLOSED for the top-level struct, no
  card — commit `2bba5b4`]** MOTUAW.sys is C++ with RTTI-ish strings and source
  names (`PCI424Driver.cpp`, `PCI424NanoDriver.cpp`, `AW2408.cpp`). Every field
  offset pinned by a concrete access site (across all RE sessions, including the
  `rz-ghidra` type-recovery pass) is now consolidated into one table in
  `docs/vendor-driver-map.md` ("Device-extension field map"):
  `+0x28` DMA adapter, `+0x38` DPC, `+0x50` status, `+0x6c` interrupt object,
  `+0x70` audio sub-object, `+0x78/+0x7c` window bases, `+0x80` port base,
  `+0x88` ack, `+0x98` audio base, `+0x9c` mix base, `+0x110`/`+0x1c4`/`+0x1c8`
  CueMix staging, `+0x260`/`+0x264` sample accounting. The `rz-ghidra` pass
  additionally identified `+0x1dc..+0x1e8` lock-debounce counters, `+0x1f4`
  routing staging buffer, `+0x258/+0x25c` mode-select word, `+0x270..+0x278`
  VU peak-hold meters, `+0x290/+0x294` SRC moduli, `+0x2a0` calibration.
  *Still open (low value, rz-ghidra-only):* the **internal** layout of the two
  embedded sub-objects (`+0x74` vtable `0x30ca4`; `+0x298` ~0x2800 bytes) —
  needs the Ghidra decompiler's type recovery over the sub-object vtables.
- [x] **0.3 Role of each binary.**
  - `MotuBus.sys` — PCI bus/function enumeration & child device creation.
  - `MOTUAW.sys` — the audio function driver (register + DMA + FPGA logic).
  - `MAWWAVE.sys` — legacy WDM/kmixer wave shim (low RE priority).
  *Exit:* one paragraph each in `docs/vendor-driver-map.md`. **[DONE]** — see
  `docs/vendor-driver-map.md` (accessor VAs, import counts, ISR-is-virtual note).

## Phase 1 — Register & control-path semantics (no card)

- [x] **1.1 Enumerate every card-address constant.** Extend the scripted scan:
  grep the disasm for immediates feeding `WRITE/READ_REGISTER_ULONG`, resolve
  the window (A if `& 0xff800000 == 0x01800000`), and tabulate. Known so far:
  `0xC0024`, `0x100024` (bank stride `0x40000`), `0x18C0000`, `0xAC44`.
  *Deliverable:* `docs/register-map.md` (addr → inferred role → evidence).
  **[DONE for statics]** — `docs/register-map.md` written. Key correction:
  `0xAC44` is 44100 (a rate constant), not an address. Clock/IRQ/dmaPoint
  register encodings remain OPEN (behind C++ vtables; need a card or xref RE).
- [x] **1.2 Reset / init sequence.** **[DONE for statics]** Traced the
  `IRP_MN_START_DEVICE` resource-list translator (`fn 0x28ee0`, via
  `xref.py`): Port resource → port-BAR base (a zero base aborts the parse
  loop early, WMI-logged as event `0x7a` — mandatory in practice, exact
  return status not pinned); Interrupt resource → Level/Vector/Affinity/mode
  (boilerplate);
  Memory resource dispatches on **exact Length** — `0x400000` → window B,
  `0x800000` → window A, both `MmMapIoSpace(..., CacheType=0)` (non-cached).
  `CmResourceTypeDma` is never handled (default/no-op case) — one more
  independent confirmation of no host bus-mastering. A separate
  `IoGetDmaAdapter` setup exists (device-ext `+0x28`) but is never referenced
  by the PIO audio path. See `docs/vendor-driver-map.md` for the full trace
  and the extended device-extension field map (`+0x28`/`+0x50`/`+0x6c`).
- [ ] **1.3 I/O-port BAR meaning.** Decode the port block at dev-ext `+0x80`
  (read `+0x0`; write `+0x4`←1, `+0x8`). Likely a PLX/local-bus bridge: identify
  the bridge (readback IDs) and whether `+0x4`/`+0x8` are FPGA-config strobes
  (nCONFIG/DATA/DCLK) vs. IRQ control. *Exit:* each port dword named.
  **[PARTIAL]** decoded in `docs/register-map.md`: `+0x0` R (bit 1 =
  ready/pending), `+0x4` W`1` (strobe/kick), `+0x8` W (init value). Bridge
  identity (PLX part) and strobe-vs-IRQ role still need confirmation.

## Phase 2 — FPGA upload protocol (no card)

The single biggest unknown that gates *any* audio.

- [~] **2.1 Locate the upload routine.** Find the code that references the
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
- [x] **3.2 `dmaPoint` register.** Find the read that yields the HW play/capture
  position (the ALSA `.pointer` source). *Exit:* its card address + units.
  **[DONE — pinned to `audio_base + 0x2c` (reader `fn 0x2a5c0`, commit `c562c99`)].**
  The reader loads `[devext+0x98]+0x2c` via `READ_REGISTER_ULONG`, subtracts the
  cached playback aperture base `[devext+0x10]` (`= [A+0x18]`), and returns
  `>> 2` (dword offset). Units are dwords. The sibling `audio_base + 0x34`
  (reader `fn 0x2a610`) is a second counter whose direction/semantics remain
  OPEN (card-gated).
- [ ] **3.3 Clock & sample-rate encoding.** Decode the writes that select
  internal/word/ADAT/SPDIF clock and the 1x/2x/4x rate family, and how the
  fixed-frame channel count shrinks with rate. Correlate with the `0x*0024`
  bank registers. *Deliverable:* rate → register-value table. **[PARTIAL]** —
  six rates confirmed as Hz constants (`0xAC44`..`0x2EE00`); base rate validated
  as 44100/48000 only (`0x191a0`). Rate *family* (1x/2x/4x) programs
  `audiobase+0x60 = 0x10<<(2*family)` (method `0x295e0`) and `audiobase+0x64` (a
  param, `0x297f0`). Enable = `audiobase+0x54 <- 1` (`0x29660`). STILL OPEN: the
  clock-source select register/bits and the `+0x64` param encoding.
- [x] **3.4 IRQ path.** From the `IoConnectInterrupt` ISR/DPC, recover the IRQ
  status register, the ack (w1c?) write, and which bit means "period elapsed".
  *Exit:* status/ack addresses + bit meanings. **[DONE — vtable resolved by
  hand]** ISR = vtable `0x30cc0` slot `0x28` = `0x2bae0`. **Pending = port BAR
  `+0x0` bit 1** (`(read>>1)&1`). **ACK = WRITE_REGISTER(devext+0x88, 0x10)**.
  Period-elapsed = per-IRQ accumulator `[+0x264] += [+0x260]` crossing `0x800`,
  then a DPC is queued via `[[devext+0x38]]`. Enable = `WRITE_PORT(+0x0,4)` +
  `WRITE_PORT(+0x4,1)` (method `0x298e0`). See `docs/register-map.md`.
- [x] **3.5 Breakout (AudioWire) enumeration.** **[DONE, no card, via
  rz-ghidra]** The host does *not* read a static channel-count table — it
  **deserializes a descriptor blob** pulled from the connected interface. The
  config builder `fn 0x1b900` reads, through a bounded byte-stream cursor
  (adapter vtable `0x30d60`, reader slot `0x48` = `fn 0x2d870`), a 4-byte
  **count** (slot `0x6c`, gated `> 2`), then per-channel byte configs into three
  `0x1200`-byte banks + a 96-dword routing table. So channel counts are runtime
  interface data, not a static constant — a Linux driver queries them on a card
  or exposes them as config. Full detail in `docs/vendor-driver-map.md`.

## Phase 4 — Rearchitect the Linux driver to the real model (needs card to verify)

Keep the clean 3-layer split; confine all new hardware truth to `motu424.h` +
`motu424_hw.c` (see `ARCHITECTURE.md`).

- [x] **4.1 Register accessors.** **[DONE, no card]** Windowed
  `card_addr → (window, masked offset)` dispatch (`motu424_addr()`,
  `motu424_rd32/wr32`); `main.c` maps all BARs generically,
  `motu424_assign_windows()` picks A/B/port by BAR type+size. Bulk aperture
  writes use `memcpy_toio` (the vendor's `WRITE_REGISTER_BUFFER_ULONG`).
- [x] **4.2 Firmware upload.** **[CLOSED - NOT NEEDED]** RE verdict
  (docs/fpga-upload.md): the classic card self-configures its FPGA from flash;
  no `request_firmware()`. Only the PCIe HD Express would need an upload path
  (out of scope until that variant is targeted).
- [x] **4.3 Transport rewrite.** **[DONE, no card]** `dma_addr` assumption
  removed; host buffer is `SNDRV_DMA_TYPE_VMALLOC`, periods are copied into the
  window-B aperture ring with software heads (`motu424_push_period()`), pointer
  advances by the per-IRQ sample increment. **Aperture base addresses are
  placeholders (TODO: verify)** — the real ring base/len come from the vendor
  config path (RE in progress) or a card dump.
- [x] **4.4 Clock/rate.** **[PARTIAL, no card]** family → period increment
  `0x10<<(2*family)` → `base+0x60` implemented; `base+0x64` param written as
  `2*family` (single-trace hypothesis, TODO: verify). Clock-source select
  register still unknown; per-rate channel-count constraint deferred to 5.1.
- [x] **4.5 IRQ.** **[DONE, no card]** Real path implemented: pending = port
  BAR +0x0 bit 1, ack = write 0x10 to the card-reported ack address, period
  accounting via the sample accumulator; devm teardown ordering preserved.
  The card-reported `audio_base`/`ack_addr` are runtime values — until dumped
  from a card, streaming is refused and they are injectable via module params.

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
- [~] **6.4 Cleanup for upstream** — **[PARTIAL, no card]**: `checkpatch --strict`
  clean on all four source files; SPDX GPL headers on every file; no
  `MODULE_FIRMWARE()` (verdict: classic card needs no host firmware); clean-room
  + provenance statement written (`CLEANROOM.md`); `Documentation/sound/motu424.rst`
  written (architecture, bring-up/module-param flow, current status/limitations).
  Remaining (card/upstream): the eventual patch submission itself.

---

## Immediate next actions

**Update (2026-07-11): `rz-ghidra` was available after all** (Arch
`extra/rizin` + `extra/rz-ghidra`; the earlier "not installable" note was
wrong). Its decompiler (`pdg`) resolved the C++ virtual dispatch that
`objdump`/`xref.py` could not, closing **Phase 3.5** (breakout channel
enumeration = descriptor deserialization, not a static table — see above and
`vendor-driver-map.md`). What remains open needs one of:

- **Deeper decompiler work** — the **top-level** device-extension struct
  layout (0.2) is now **DONE** (commit `2bba5b4`, `rz-ghidra` type recovery);
  only the *internal* layout of the two embedded sub-objects (`+0x74`,
  `+0x298`) remains — low value, `rz-ghidra`-only. Phase 2.2 (FPGA handshake
  bit/byte order) is **moot for the classic card**: the RE verdict is that the
  classic PCI-324/424 self-configures its Altera FPGA from onboard flash and
  the driver uploads nothing (`fpga-upload.md`); only the PCIe HD Express takes
  a host image, and that image is already dissected.
- **A card** — blocks: Phase 1.3 (bridge chip identity via readback), 3.5
  empirical confirmation of the channel counts, and all of Phases 5–6 (kcontrol
  registration, real playback/capture, soak testing). Phase 3.2 (`dmaPoint`)
  is now closed statically (`audio_base + 0x2c`, commit `c562c99`). The driver
  already refuses to stream until the card-reported runtime addresses are
  supplied via module params
  (`audio_base=`/`ack_addr=`/`mix_base=`/`play_aperture=`/`cap_aperture=`),
  obtainable from `tools/motu424-probe` once a card is attached.

The static surface is now essentially exhausted. The project's critical path
is **Phase 6.1 empirical bring-up** on real hardware.
