# Next Steps: MOTU PCI / PCIe-424 Hardware Bring-Up Guide

This guide details what is already implemented, the remaining technical requirements, and the exact step-by-step procedure to bring up and stream audio with the MOTU PCI-324, PCI-424, or PCIe-424 ("HD Express") cards under Linux.

---

## 1. Status Snapshot (What is Already Completed)

1. **ALSA Driver Architecture**:
   - Clean 3-layer architecture separating PCI management, ALSA PCM callbacks, and hardware registers.
   - Managed devres device attachment, resource reservation (`pcim_iomap_region`), IRQ vector allocation, and teardown in [`kernel/motu424_main.c`](../kernel/motu424_main.c).
   - Standard ALSA PCM callbacks (open, close, prepare, trigger, pointer) in [`kernel/motu424_pcm.c`](../kernel/motu424_pcm.c).
2. **Reverse-Engineered Hardware Model**:
   - Dual-window MMIO card-address space: Window A (8 MB) and Window B (4 MB), with address decode routing via `(addr & 0xff800000) == 0x01800000`.
   - Small I/O-port bridge BAR (`READ/WRITE_PORT_ULONG`): `port + 0x0` bit 1 = IRQ pending, `+0x0` &larr; 4 (IRQ enable), `+0x4` &larr; 1 (commit strobe).
   - Real IRQ ack protocol: writing `0x10` to the card-reported ack address.
   - Audio transport: PIO copying (`memcpy_toio`) into the Window B aperture ring buffer; hardware position tracking via `dmaPoint` (`audio_base + 0x2c`).
3. **Firmware Support**:
   - Proprietary container firmware `HDExpress_FullImageRun.bin` (ARM32 SoC + Xilinx Virtex FPGA bitstream) verified and integrated into [`install.sh`](../install.sh) and [`Makefile`](../Makefile) to deploy to `/lib/firmware/`.
4. **Userspace Control & Tooling**:
   - [`tools/motu424-probe`](../tools/motu424-probe.c): Userspace BAR enumerator and memory dumper.
   - [`tools/motu424-ctl`](../tools/motu424-ctl.c): CueMix CLI for clock source, rate, and monitor matrix controls.
   - [`tools/motu424-gui`](../tools/motu424-gui): GTK4 CueMix studio console with virtual normalled patchbay, channel strips, diagnostics, and headless container verification (`--verify-fw`).

---

## 2. The Gaps Blocking Audio Streaming

Before audio can pass through the card, three hardware-dependent items must be addressed:

### Gap 1: PCIe Firmware Target BAR & Strobe (PCIe-424 `DEV_0005` only)
- Classic PCI cards (`DEV_0003`, `DEV_0004`) self-configure their Altera FPGA from onboard flash at power-on.
- The PCIe-424 (`DEV_0005`) boots an on-board ARM SoC and configures a Xilinx Virtex FPGA from `HDExpress_FullImageRun.bin`.
- **Requirement**: Identify which BAR receives the firmware data (default assumes Window B or Window A) and whether a boot kick strobe is needed. The driver provides parameters to test this:
  - `pcie_fw_bar=<0..5>` (destination BAR index)
  - `pcie_fw_offset=<hex>` (offset within destination BAR, default `0x100000`)

### Gap 2: Card-Reported Runtime Addresses (`audio_base` & `ack_addr`)
- In the vendor Windows driver (`MOTUAW.sys`), the card does not hardcode static register locations for the audio engine or the IRQ ack register. Instead, it reads an internal card address at init time to discover where the audio register block and ack register are located in Window B.
- **Requirement**: The Linux driver refuses to start a stream (`-ENXIO` in `motu424_hw_stream_prepare`) until these addresses are supplied:
  - `audio_base`: Base address of the audio registers (stream enable `+0x54`, rate family `+0x60`, position counter `+0x2c`).
  - `ack_addr`: Card address where `0x10` must be written to clear the interrupt.
  - `play_aperture`: Window B card offset for playback DMA buffer copy.
  - `cap_aperture`: Window B card offset for capture DMA buffer copy.

### Gap 3: Physical AudioWire Breakout Connection
- The host PCI/PCIe card has no analog audio jacks (only 4 FireWire-style AudioWire ports).
- An AudioWire interface (**2408mk3**, **24I/O**, **HD192**, **1224**, etc.) must be connected with an IEEE 1394 cable to port 1, 2, 3, or 4.
- When connected, the front panel LEDs on the interface will indicate clock lock once the bus communicates with the card.

---

## 3. Step-by-Step Bring-Up Workflow

```
[1. Hardware Install] ──> [2. Firmware Deploy] ──> [3. Probe Card BARs]
                                                           │
[6. Audio Streaming] <── [5. Inject Addresses] <── [4. Module Load]
```

### Step 1: Verify Hardware Presence
Verify that the card is seated in the PCI/PCIe slot and detected on the PCI bus:
```bash
lspci -nn | grep -i 137a
```
Expected IDs:
- `137a:0003` &mdash; MOTU PCI-324
- `137a:0004` &mdash; MOTU PCI-424
- `137a:0005` &mdash; MOTU PCIe-424 ("HD Express")

### Step 2: Ensure Firmware is Installed
If using a PCIe-424 card, ensure the container image is in `/lib/firmware`:
```bash
sudo cp vendor/HDExpress_FullImageRun.bin /lib/firmware/
```
Verify the container with the userspace tool:
```bash
./tools/motu424-gui --verify-fw
```

### Step 3: Run the Hardware Probe Tool (Driver Unloaded)
With the `motu424` kernel module **not** loaded:
```bash
sudo ./tools/motu424-probe
```
This tool scans `/sys/bus/pci`, prints all BARs, and classifies them:
- An ~8 MB MMIO BAR &rarr; Window A
- A ~4 MB MMIO BAR &rarr; Window B (the audio aperture)
- A small I/O-port BAR &rarr; Bridge control (classic PCI)
- For PCIe cards, it will identify the available MMIO apertures for firmware and streaming.

To take a targeted dump of Window B bank registers:
```bash
sudo ./tools/motu424-probe <PCI_BDF> 0xc0000 0x400
```
*(Replace `<PCI_BDF>` with the PCI address, e.g., `0000:02:00.0`)*

### Step 4: Load Driver and Check Kernel Logs
Load the compiled module:
```bash
sudo make load
# or:
sudo modprobe motu424
```
Inspect kernel messages:
```bash
sudo dmesg | grep -i motu
```
Look for:
- Card model recognition: `MOTU PCIe-424 registered as ALSA card N`
- PCIe firmware verification and upload messages
- Bank status words: `bank0 ctrl/status: 0x...`

### Step 5: Supply Runtime Addresses
Once `audio_base`, `ack_addr`, `play_aperture`, and `cap_aperture` are obtained (from probe diffing, VM trace, or Windows dual-boot), inject them via module parameters:
```bash
sudo rmmod motu424
sudo insmod kernel/motu424.ko \
    audio_base=0x000c0000 \
    ack_addr=0x000c0088 \
    play_aperture=0x00000000 \
    cap_aperture=0x00020000
```
*(Note: Addresses above are illustrative placeholders; replace with captured values).*

To persist these parameters across reboots, create `/etc/modprobe.d/motu424.conf`:
```ini
options motu424 audio_base=0x... ack_addr=0x... play_aperture=0x... cap_aperture=0x...
```

### Step 6: Test Playback and Recording
Verify ALSA detects the device:
```bash
aplay -l
arecord -l
```
Test audio playback with a test WAV:
```bash
aplay -D hw:1,0 /usr/share/sounds/alsa/Front_Center.wav
```
*(Replace `hw:1,0` with the actual card index shown in `aplay -l`)*

### Step 7: Launch the Console & Diagnostics
Launch the CueMix GTK4 Studio Console:
```bash
motu424-gui
```
Or check mixer status from CLI:
```bash
motu424-ctl
motu424-ctl list
```

---

## 4. Useful Diagnostic Commands

| Task | Command |
|---|---|
| Check PCI device | `lspci -vnn -d 137a:` |
| Check loaded module | `lsmod \| grep motu424` |
| View active driver parameters | `cat /sys/module/motu424/parameters/*` |
| Monitor live driver dmesg | `sudo dmesg -w \| grep -i motu` |
| Verify firmware container | `./tools/motu424-gui --verify-fw` |
| Dump hardware registers | `sudo ./tools/motu424-probe` |
| List ALSA playback devices | `aplay -l` |
| List ALSA capture devices | `arecord -l` |
| Unload driver cleanly | `sudo make unload` or `sudo modprobe -r motu424` |
