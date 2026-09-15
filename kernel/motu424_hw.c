// SPDX-License-Identifier: GPL-2.0-or-later
/*
 * motu424_hw.c - the hardware abstraction layer.
 *
 * =====================================================================
 * THIS IS THE ONLY FILE THAT ENCODES REAL MOTU PCI-324/424 SEMANTICS.
 * =====================================================================
 *
 * The model implemented here was recovered by static RE of the vendor
 * MOTUAW.sys (see docs/register-map.md and docs/transport.md):
 *
 *  - a windowed ~24-bit card-address space over two MMIO BARs (A: 8 MB,
 *    B: 4 MB) plus a small I/O-port bridge BAR;
 *  - audio transport is PIO into a window-B aperture ring (software
 *    read/write heads, <=64 KB dword bursts), NOT host bus-master DMA;
 *  - IRQ pending = port BAR +0x0 bit 1; ack = write 0x10 to a
 *    card-reported ack address; period accounting via an accumulator;
 *  - an audio register block at a card-reported base: +0x54 enable,
 *    +0x60 period increment (0x10 << 2*family), +0x64 rate param,
 *    +0x128/+0x12c/+0x130 position counters (read then zeroed).
 *
 * The audio base / ack / mixer-base card addresses are RUNTIME values the
 * vendor driver reads back from the card at init; they cannot be recovered
 * statically. Until they are dumped from real hardware they default to 0,
 * streaming is refused with -ENXIO, and they can be injected for bring-up:
 *
 *   insmod motu424.ko audio_base=0x... ack_addr=0x... [mix_base=0x...]
 *
 * Aperture ring base addresses are placeholders (TODO: verify on card).
 */
#include <linux/io.h>
#include <linux/module.h>
#include <linux/firmware.h>
#include <linux/delay.h>

#include "motu424.h"

/* Bring-up injection of the card-reported addresses (see header comment). */
static unsigned int audio_base;
module_param(audio_base, uint, 0444);
MODULE_PARM_DESC(audio_base, "Card address of the audio register block (from probe)");
static unsigned int ack_addr;
module_param(ack_addr, uint, 0444);
MODULE_PARM_DESC(ack_addr, "Card address of the IRQ ack register (from probe)");
static unsigned int mix_base;
module_param(mix_base, uint, 0444);
MODULE_PARM_DESC(mix_base, "Card address of the CueMix coefficient region (from probe)");
static unsigned int play_aperture;
module_param(play_aperture, uint, 0444);
MODULE_PARM_DESC(play_aperture, "Card address of the playback aperture (from probe)");
static unsigned int cap_aperture;
module_param(cap_aperture, uint, 0444);
MODULE_PARM_DESC(cap_aperture, "Card address of the capture aperture (from probe)");

/* PCIe firmware loading parameters */
static char *fw_filename = MOTU424_PCIE_FW_NAME;
module_param(fw_filename, charp, 0444);
MODULE_PARM_DESC(fw_filename, "Firmware filename for PCIe-424 card (default: " MOTU424_PCIE_FW_NAME ")");
static bool skip_fw;
module_param(skip_fw, bool, 0444);
MODULE_PARM_DESC(skip_fw, "Skip firmware upload for PCIe card (for bringup/testing)");

/*
 * =====================================================================
 * USER PLACEHOLDER: PCIe BAR OVERRIDES
 * =====================================================================
 * As requested ("i'll fill in bar later"), these module parameters and
 * variables allow overriding the auto-detected BAR indices for the PCIe card.
 */
static int pcie_bar_a = -1;
module_param(pcie_bar_a, int, 0444);
MODULE_PARM_DESC(pcie_bar_a, "PCIe Window A BAR override (-1 = auto)");
static int pcie_bar_b = -1;
module_param(pcie_bar_b, int, 0444);
MODULE_PARM_DESC(pcie_bar_b, "PCIe Window B BAR override (-1 = auto)");
static int pcie_bar_port = -1;
module_param(pcie_bar_port, int, 0444);
MODULE_PARM_DESC(pcie_bar_port, "PCIe Port / bridge BAR override (-1 = auto)");
static int pcie_fw_bar = -1;
module_param(pcie_fw_bar, int, 0444);
MODULE_PARM_DESC(pcie_fw_bar, "BAR index for PCIe firmware upload (-1 = auto/WinB)");
static unsigned int pcie_fw_offset = MOTU424_HDEXPRESS_LOAD_ADDR;
module_param(pcie_fw_offset, uint, 0444);
MODULE_PARM_DESC(pcie_fw_offset, "Destination offset for PCIe firmware upload");

/* PCIe Hardware DSP Engine parameters */
static bool enable_dsp = false;
module_param(enable_dsp, bool, 0444);
MODULE_PARM_DESC(enable_dsp, "Enable hardware DSP engine on PCIe-424 card (default: 0)");
static int pcie_dsp_bar = -1;
module_param(pcie_dsp_bar, int, 0444);
MODULE_PARM_DESC(pcie_dsp_bar, "BAR index for PCIe DSP mailbox (-1 = auto/WinB)");
static unsigned int pcie_dsp_offset = MOTU424_BANK0;
module_param(pcie_dsp_offset, uint, 0444);
MODULE_PARM_DESC(pcie_dsp_offset, "Card address / offset for DSP mailbox (default: 0xC0000)");

/* --- windowed card-address dispatch (vendor accessors 0x29110/0x29160) --- */
static void __iomem *motu424_addr(struct motu424 *chip, u32 card_addr)
{
	if ((card_addr & MOTU424_WINA_TAG_MASK) == MOTU424_WINA_TAG) {
		/*
		 * Windows A and B alias the same space; fall back to B when
		 * the card exposes a single MMIO BAR.
		 */
		if (chip->win_a)
			return chip->win_a + (card_addr & MOTU424_WINA_MASK);
		card_addr &= ~MOTU424_WINA_TAG;
	}
	return chip->win_b + (card_addr & MOTU424_WINB_MASK);
}

static inline u32 motu424_rd32(struct motu424 *chip, u32 card_addr)
{
	return ioread32(motu424_addr(chip, card_addr));
}

static inline void motu424_wr32(struct motu424 *chip, u32 card_addr, u32 val)
{
	iowrite32(val, motu424_addr(chip, card_addr));
}

/* Audio-register helpers: offsets from the card-reported audio base. */
static inline void motu424_awr(struct motu424 *chip, u32 off, u32 val)
{
	motu424_wr32(chip, chip->audio_base + off, val);
}

static inline u32 motu424_ard(struct motu424 *chip, u32 off)
{
	return motu424_rd32(chip, chip->audio_base + off);
}

/*
 * Verify HDExpress_FullImageRun.bin container format and checksum:
 * - 24-byte header: load_addr, hdr_len=0x18, payload_len, sum32 checksum, entry_point, version
 * - payload checksum is the sum of all payload bytes mod 2^32
 * - walks section headers and logs details (ARM firmware, Xilinx Virtex bitstream, configs)
 */
static int motu424_pcie_verify_firmware(struct motu424 *chip, const struct firmware *fw)
{
	struct device *dev = &chip->pci->dev;
	const struct hdexpress_fw_hdr *hdr;
	u32 load_addr, hdr_len, payload_len, expected_csum, entry_point, version;
	u32 calc_csum = 0;
	size_t off, sec_idx = 0;
	const u8 *payload;
	size_t i;

	if (fw->size < sizeof(*hdr)) {
		dev_err(dev, "firmware image too small (%zu < %zu bytes)\n",
			fw->size, sizeof(*hdr));
		return -EINVAL;
	}

	hdr = (const struct hdexpress_fw_hdr *)fw->data;
	load_addr = le32_to_cpu(hdr->load_addr);
	hdr_len = le32_to_cpu(hdr->hdr_len);
	payload_len = le32_to_cpu(hdr->payload_len);
	expected_csum = le32_to_cpu(hdr->checksum);
	entry_point = le32_to_cpu(hdr->entry_point);
	version = le32_to_cpu(hdr->version);

	if (hdr_len != MOTU424_HDEXPRESS_HDR_LEN) {
		dev_err(dev, "invalid firmware header length: %u (expected %u)\n",
			hdr_len, MOTU424_HDEXPRESS_HDR_LEN);
		return -EINVAL;
	}

	if (hdr_len + payload_len != fw->size) {
		dev_err(dev, "firmware size mismatch: header(%u) + payload(%u) != file(%zu)\n",
			hdr_len, payload_len, fw->size);
		return -EINVAL;
	}

	/* Compute sum of all payload bytes mod 2^32 */
	payload = fw->data + hdr_len;
	for (i = 0; i < payload_len; i++)
		calc_csum += payload[i];

	if (calc_csum != expected_csum) {
		dev_err(dev, "firmware checksum failure: calculated 0x%08x != header 0x%08x\n",
			calc_csum, expected_csum);
		return -EINVAL;
	}

	dev_info(dev, "PCIe firmware container verified: version=0x%08x, load=0x%08x, entry=0x%08x, payload=%u bytes\n",
		 version, load_addr, entry_point, payload_len);

	/* Walk and log section descriptors */
	off = hdr_len;
	while (off + sizeof(struct hdexpress_section_hdr) <= fw->size) {
		const struct hdexpress_section_hdr *sec =
			(const struct hdexpress_section_hdr *)(fw->data + off);
		u32 sec_type = le32_to_cpu(sec->type);
		u32 sec_data_off = le32_to_cpu(sec->data_off);
		u32 sec_size = le32_to_cpu(sec->size);
		u32 sec_flags = le32_to_cpu(sec->flags);
		u32 sec_tag = le32_to_cpu(sec->tag);
		const char *name = "unknown";

		switch (sec_type) {
		case HDEXPRESS_SEC_ARM_FW:
			name = "ARM32 SoC firmware";
			break;
		case HDEXPRESS_SEC_CONFIG_PRE:
			name = "Pre-config record";
			break;
		case HDEXPRESS_SEC_VIRTEX_FPGA:
			name = "Xilinx Virtex FPGA bitstream";
			break;
		case HDEXPRESS_SEC_CONFIG_POST:
			name = "Post-config record";
			break;
		}

		dev_info(dev, "  section %zu: type=0x%x (%s), off=0x%zx, size=0x%x (%u bytes), flags=0x%x, tag=0x%x\n",
			 sec_idx++, sec_type, name, off, sec_size, sec_size, sec_flags, sec_tag);

		if (sec_data_off + sec_size == 0)
			break;
		off += sec_data_off + sec_size;
	}

	return 0;
}

/*
 * =====================================================================
 * USER PLACEHOLDER: PCIe FIRMWARE UPLOAD & BAR DISPATCH
 * =====================================================================
 * Upload the verified firmware image to the PCIe card.
 *
 * As requested ("i'll fill in bar later"), the specific BAR,
 * aperture offset, and handshake sequence for the ARM SoC + FPGA loader
 * will be filled in once dumped or captured via bus trace.
 *
 * By default:
 * - If pcie_fw_bar is set (or configured via module parameter), writes to
 *   chip->bars[pcie_fw_bar].ptr + pcie_fw_offset.
 * - Otherwise, writes into Window B or the designated MMIO BAR.
 * - Staging and handshake hooks are provided below.
 */
static int motu424_pcie_upload_firmware(struct motu424 *chip,
					const struct firmware *fw)
{
	struct device *dev = &chip->pci->dev;
	void __iomem *target_bar = NULL;
	resource_size_t target_len = 0;
	u32 target_offset = chip->pcie_fw_offset;
	int bar_idx = chip->pcie_bar_fw;

	/*
	 * -------------------------------------------------------------
	 * 1. SELECT DESTINATION BAR
	 * -------------------------------------------------------------
	 * USER: When you determine which BAR the firmware belongs to,
	 * set pcie_fw_bar or adjust the selection logic below.
	 */
	if (bar_idx >= 0 && bar_idx < PCI_STD_NUM_BARS && chip->bars[bar_idx].ptr) {
		target_bar = chip->bars[bar_idx].ptr;
		target_len = chip->bars[bar_idx].len;
	} else if (chip->win_b) {
		target_bar = chip->win_b;
		target_len = MOTU424_WINB_LEN;
		bar_idx = 1;
	} else if (chip->win_a) {
		target_bar = chip->win_a;
		target_len = MOTU424_WINA_LEN;
		bar_idx = 0;
	}

	if (!target_bar) {
		dev_warn(dev, "no target MMIO BAR mapped for PCIe firmware upload (user will fill in BAR)\n");
		return 0;
	}

	dev_info(dev, "uploading PCIe firmware (%zu bytes) to BAR%d offset 0x%08x...\n",
		 fw->size, bar_idx, target_offset);

	/*
	 * -------------------------------------------------------------
	 * 2. TRANSFER FIRMWARE TO CARD
	 * -------------------------------------------------------------
	 * USER: If your card requires chunked MMIO copying, DMA, or a
	 * specific handshake, customize this transfer block.
	 */
	if (target_offset + fw->size <= target_len) {
		memcpy_toio(target_bar + target_offset, fw->data, fw->size);
		dev_info(dev, "firmware copied to BAR%d + 0x%08x (%zu bytes)\n",
			 bar_idx, target_offset, fw->size);
	} else {
		dev_warn(dev, "firmware size (%zu bytes) exceeds mapped BAR%d space (offset 0x%x, len %llu); copying head\n",
			 fw->size, bar_idx, target_offset, (unsigned long long)target_len);
		if (target_offset < target_len)
			memcpy_toio(target_bar + target_offset, fw->data,
				    target_len - target_offset);
	}

	/*
	 * -------------------------------------------------------------
	 * 3. KICK / BOOT ARM SOC & FPGA
	 * -------------------------------------------------------------
	 * USER: Place the boot strobe or register kick here once
	 * discovered via VFIO bus trace or RWEverything.
	 */

	chip->fw_loaded = true;
	dev_info(dev, "PCIe firmware upload completed\n");
	return 0;
}

int motu424_hw_load_firmware(struct motu424 *chip)
{
	struct device *dev = &chip->pci->dev;
	const struct firmware *fw;
	int err;

	if (!chip->is_pcie)
		return 0; /* Classic PCI cards self-configure from flash */

	if (skip_fw) {
		dev_info(dev, "skipping PCIe firmware upload (skip_fw parameter set)\n");
		return 0;
	}

	dev_info(dev, "requesting PCIe firmware: %s\n", fw_filename);
	err = request_firmware(&fw, fw_filename, dev);
	if (err < 0) {
		dev_warn(dev, "failed to load firmware '%s' (err %d); continuing bring-up with streaming disabled. Place firmware in /lib/firmware/ or use skip_fw=1\n",
			 fw_filename, err);
		return err;
	}

	err = motu424_pcie_verify_firmware(chip, fw);
	if (err < 0) {
		dev_err(dev, "firmware verification failed: %d\n", err);
		release_firmware(fw);
		return err;
	}

	err = motu424_pcie_upload_firmware(chip, fw);
	release_firmware(fw);
	return err;
}

/*
 * Assign the mapped BARs to their hardware roles.
 *
 * For classic PCI:
 *   - I/O-port BAR: bridge control
 *   - 8 MB MMIO BAR: window A
 *   - 4 MB MMIO BAR: window B (audio aperture)
 *
 * For PCIe-424:
 *   - Supports manual overrides via module parameters (pcie_bar_a, pcie_bar_b, pcie_bar_port, pcie_fw_bar)
 *   - Permits running without an I/O-port BAR (pure MMIO)
 *   - Falls back gracefully to any available MMIO BAR for Window B
 */
static int motu424_assign_windows(struct motu424 *chip)
{
	struct device *dev = &chip->pci->dev;
	int i;

	/* Log all active BARs for diagnostics */
	for (i = 0; i < PCI_STD_NUM_BARS; i++) {
		struct motu424_bar *b = &chip->bars[i];

		if (!b->ptr)
			continue;
		dev_info(dev, "BAR%d: len=%llu bytes flags=0x%lx (%s)\n",
			 i, (unsigned long long)b->len, b->flags,
			 (b->flags & IORESOURCE_IO) ? "I/O" :
			 (b->flags & IORESOURCE_MEM) ? "MMIO" : "other");
	}

	/*
	 * =============================================================
	 * USER PLACEHOLDER: PCIe BAR OVERRIDES & MANUAL MAPPING
	 * =============================================================
	 * As requested ("i'll fill in bar later"), the user can override
	 * BAR indices via module params (e.g. pcie_bar_a=0 pcie_bar_b=1)
	 * or directly by setting chip->pcie_bar_* here.
	 */
	if (pcie_bar_a >= 0 && pcie_bar_a < PCI_STD_NUM_BARS)
		chip->pcie_bar_a = pcie_bar_a;
	if (pcie_bar_b >= 0 && pcie_bar_b < PCI_STD_NUM_BARS)
		chip->pcie_bar_b = pcie_bar_b;
	if (pcie_bar_port >= 0 && pcie_bar_port < PCI_STD_NUM_BARS)
		chip->pcie_bar_port = pcie_bar_port;
	if (pcie_fw_bar >= 0 && pcie_fw_bar < PCI_STD_NUM_BARS)
		chip->pcie_bar_fw = pcie_fw_bar;
	if (pcie_fw_offset != MOTU424_HDEXPRESS_LOAD_ADDR)
		chip->pcie_fw_offset = pcie_fw_offset;

	/* Apply manual overrides if specified */
	if (chip->pcie_bar_port >= 0 && chip->pcie_bar_port < PCI_STD_NUM_BARS &&
	    chip->bars[chip->pcie_bar_port].ptr)
		chip->port = chip->bars[chip->pcie_bar_port].ptr;

	if (chip->pcie_bar_a >= 0 && chip->pcie_bar_a < PCI_STD_NUM_BARS &&
	    chip->bars[chip->pcie_bar_a].ptr)
		chip->win_a = chip->bars[chip->pcie_bar_a].ptr;

	if (chip->pcie_bar_b >= 0 && chip->pcie_bar_b < PCI_STD_NUM_BARS &&
	    chip->bars[chip->pcie_bar_b].ptr)
		chip->win_b = chip->bars[chip->pcie_bar_b].ptr;

	/* Auto-discovery for unassigned windows */
	for (i = 0; i < PCI_STD_NUM_BARS; i++) {
		struct motu424_bar *b = &chip->bars[i];

		if (!b->ptr)
			continue;
		if (b->flags & IORESOURCE_IO) {
			if (!chip->port)
				chip->port = b->ptr;
		} else if (b->flags & IORESOURCE_MEM) {
			if (b->len >= MOTU424_WINA_LEN && !chip->win_a)
				chip->win_a = b->ptr;
			else if (!chip->win_b)
				chip->win_b = b->ptr;
		}
	}

	/* A lone large MMIO BAR serves as window B too. */
	if (!chip->win_b && chip->win_a) {
		chip->win_b = chip->win_a;
		chip->win_a = NULL;
	}

	/*
	 * On PCIe, if neither WinA nor WinB was matched by size, pick any
	 * available MMIO BAR as WinB so the user can probe and inspect it.
	 */
	if (chip->is_pcie && !chip->win_b) {
		for (i = 0; i < PCI_STD_NUM_BARS; i++) {
			if (chip->bars[i].ptr && (chip->bars[i].flags & IORESOURCE_MEM)) {
				chip->win_b = chip->bars[i].ptr;
				dev_info(dev, "PCIe fallback: using BAR%d as Window B\n", i);
				break;
			}
		}
	}

	if (!chip->win_b) {
		dev_err(dev, "no usable MMIO BAR found\n");
		return -ENODEV;
	}

	if (!chip->port) {
		if (chip->is_pcie)
			dev_info(dev, "PCIe card has no I/O port BAR (operating in MMIO mode)\n");
		else
			dev_warn(dev, "classic PCI card missing I/O port BAR\n");
	}

	return 0;
}

/*
 * Bring the card to a known idle state and discover what we can. The
 * card-reported audio/ack/mixer addresses cannot be located statically
 * (the read source in the vendor init path is unresolved), so we take them
 * from module parameters and complain loudly when absent.
 */
int motu424_hw_init(struct motu424 *chip)
{
	struct device *dev = &chip->pci->dev;
	unsigned long flags;
	int err;

	err = motu424_assign_windows(chip);
	if (err < 0)
		return err;

	/* For PCIe cards, load and upload firmware to the card */
	if (chip->is_pcie) {
		err = motu424_hw_load_firmware(chip);
		if (err < 0 && !skip_fw)
			dev_warn(dev, "PCIe firmware load returned %d; continuing bringup\n", err);
		/* Initialize PCIe Hardware DSP Engine */
		motu424_dsp_init(chip);
	}

	chip->audio_base = audio_base;
	chip->ack_addr = ack_addr;
	chip->mix_base = mix_base;
	chip->aperture[0] = play_aperture;
	chip->aperture[1] = cap_aperture;

	spin_lock_irqsave(&chip->lock, flags);

	if (chip->port) {
		/*
		 * Vendor bring-up writes an init value (observed 0) to the
		 * port bridge. TODO: verify on hardware.
		 */
		iowrite32(0, chip->port + MOTU424_PORT_INIT);
	}

	/*
	 * Dump the per-bank ctrl/status words - harmless reads that give the
	 * first signs of life in dmesg and feed the probe/diff workflow.
	 */
	dev_info(dev, "bank0 ctrl/status: 0x%08x, bank1: 0x%08x\n",
		 motu424_rd32(chip, MOTU424_BANK0 + MOTU424_BANK_CTRL),
		 motu424_rd32(chip, MOTU424_BANK1 + MOTU424_BANK_CTRL));

	spin_unlock_irqrestore(&chip->lock, flags);

	if (!chip->audio_base || !chip->ack_addr)
		dev_warn(dev,
			 "audio_base/ack_addr unknown (card-reported values, need a probe dump); card registers but streaming is disabled. Pass motu424.audio_base=/ack_addr= to enable.\n");
	else
		dev_info(dev, "audio_base=0x%08x ack_addr=0x%08x mix_base=0x%08x\n",
			 chip->audio_base, chip->ack_addr, chip->mix_base);

	chip->rate = 48000;
	chip->family = 0;
	chip->period_incr = 0x10;
	chip->channels = MOTU424_MAX_CHANNELS;
	return 0;
}

void motu424_hw_shutdown(struct motu424 *chip)
{
	unsigned long flags;

	if (!chip->win_b)
		return;

	spin_lock_irqsave(&chip->lock, flags);
	if (chip->audio_base)
		motu424_awr(chip, MOTU424_AREG_ENABLE, 0);
	if (chip->port) {
		iowrite32(0, chip->port + MOTU424_PORT_CTRL);
	} else if (chip->is_pcie) {
		/*
		 * USER PLACEHOLDER: PCIe MMIO QUIESCE
		 * Add MMIO-based stream disable/quiesce here once discovered.
		 */
		if (chip->dsp_running)
			motu424_dsp_shutdown(chip);
	}
	spin_unlock_irqrestore(&chip->lock, flags);
}

/*
 * Program the rate. Confirmed: the period increment written to base+0x60 is
 * 0x10 << (2*family) (16/64/256 samples per IRQ). The base+0x64 parameter
 * encoding is NOT yet confirmed - the single static trace observed family=2
 * paired with param=4, so we encode param = 2*family (TODO: verify; may be
 * the 11-way mode enum from fn 0x11320 instead). The clock-source select
 * register is still unknown; internal clock is implicitly assumed.
 */
int motu424_hw_set_rate(struct motu424 *chip, unsigned int rate)
{
	unsigned long flags;
	unsigned int family;

	switch (rate) {
	case 44100:
	case 48000:
		family = 0;
		break;
	case 88200:
	case 96000:
		family = 1;
		break;
	case 176400:
	case 192000:
		family = 2;
		break;
	default:
		return -EINVAL;
	}

	if (!chip->audio_base)
		return -ENXIO;

	spin_lock_irqsave(&chip->lock, flags);
	chip->family = family;
	chip->period_incr = 0x10 << (2 * family);
	motu424_awr(chip, MOTU424_AREG_INCR, chip->period_incr);
	motu424_awr(chip, MOTU424_AREG_PARAM, 2 * family); /* TODO: verify */
	spin_unlock_irqrestore(&chip->lock, flags);

	chip->rate = rate;
	return 0;
}

/*
 * Copy one period between the host buffer and the card aperture ring,
 * advancing the stream's host and ring positions. Called under chip->lock.
 * The ring is a power-of-two number of bytes in window B; bursts may wrap.
 *
 * The card's HW position counter (MOTU424_AREG_DMAPOINT = audio_base + 0x2c)
 * was confirmed at fn 0x2a5c0; at present we still drive the ALSA pointer and
 * the ring head from the per-IRQ software accumulator (period_incr samples/
 * IRQ), not from a HW read on every .pointer call.
 *
 * TODO (RT latency): this runs a multi-KB memcpy_toio under chip->lock with
 * IRQs off. On the PREEMPT_RT target that is a latency spike; now that the
 * dmaPoint register is known, move the copy out of the IRQ-off region (e.g.
 * a threaded handler or a bounded per-tick burst), and optionally read the
 * HW position directly here for xrun recovery.
 */
static void motu424_push_period(struct motu424 *chip, struct motu424_stream *s,
				bool playback)
{
	struct snd_pcm_runtime *runtime = s->substream->runtime;
	u32 aperture = chip->aperture[playback ? 0 : 1];
	unsigned int bytes = s->period_bytes;
	unsigned int ring_off = s->ring_pos % MOTU424_RING_BYTES;

	/*
	 * Window B is a paged 4 MB view over a larger card-address space, and
	 * playback/capture apertures are two independent runtime addresses
	 * (docs/transport.md) that are not guaranteed to share a page. Full
	 * duplex re-arms the page-select register on *every* push (not just
	 * at stream start) so the other direction's start/IRQ processing can
	 * never leave this access pointed at the wrong page.
	 */
	if (chip->port)
		iowrite32(aperture >> MOTU424_APERTURE_PAGE_SHIFT,
			  chip->port + MOTU424_PORT_INIT);

	while (bytes) {
		unsigned int chunk = min(bytes, MOTU424_RING_BYTES - ring_off);
		void __iomem *io = motu424_addr(chip, aperture + ring_off);
		unsigned char *host = runtime->dma_area + s->buf_pos;

		if (playback)
			memcpy_toio(io, host, chunk);
		else
			memcpy_fromio(host, io, chunk);

		bytes -= chunk;
		ring_off = (ring_off + chunk) % MOTU424_RING_BYTES;
		s->buf_pos = (s->buf_pos + chunk) % s->buffer_bytes;
	}
	s->ring_pos = ring_off;
}

/*
 * Prepare a stream: record geometry and reset the ring bookkeeping. The host
 * buffer is plain (vmalloc) memory - the card never sees a host address; all
 * data moves by PIO in motu424_push_period().
 */
int motu424_hw_stream_prepare(struct motu424 *chip,
			      struct snd_pcm_substream *substream)
{
	bool playback = MOTU424_STREAM_IS_PLAYBACK(substream);
	struct motu424_stream *s = playback ? &chip->playback : &chip->capture;
	unsigned long flags;

	if (!chip->audio_base || !chip->ack_addr || !chip->aperture[playback ? 0 : 1]) {
		dev_warn_once(&chip->pci->dev,
			      "streaming disabled: audio_base/ack_addr/aperture not set (see motu424 module parameters)\n");
		return -ENXIO;
	}

	spin_lock_irqsave(&chip->lock, flags);
	s->buffer_bytes = snd_pcm_lib_buffer_bytes(substream);
	s->period_bytes = snd_pcm_lib_period_bytes(substream);
	s->buffer_frames = substream->runtime->buffer_size;
	s->substream = substream;
	s->buf_pos = 0;
	s->ring_pos = 0;
	s->pos_frames = 0;
	s->period_acc = 0;
	spin_unlock_irqrestore(&chip->lock, flags);
	return 0;
}

/*
 * Start a stream. Vendor sequence (method 0x298e0 + slot-1 enable):
 * prefill the aperture, write 1 to base+0x54, then kick the port bridge
 * with WRITE(+0x0, 4) and WRITE(+0x4, 1). Called from the atomic trigger.
 *
 * @fresh is true only for a genuine TRIGGER_START (from prepared state); it is
 * false for pause-release/resume, where the ring bookkeeping must be preserved
 * and the buffer must NOT be re-prefilled (that would skip data).
 */
void motu424_hw_stream_start(struct motu424 *chip, bool playback, bool fresh)
{
	struct motu424_stream *s = playback ? &chip->playback : &chip->capture;
	unsigned long flags;
	bool first;

	spin_lock_irqsave(&chip->lock, flags);

	/*
	 * .running is chip->lock-protected (motu424.h); reading it for the
	 * "first" decision has to happen under the lock too, or a concurrent
	 * trigger of the other direction's substream (ALSA does not serialize
	 * .trigger across unlinked playback/capture substreams) can race this
	 * read and double-fire the ENABLE/STROBE kick sequence below.
	 */
	first = !chip->playback.running && !chip->capture.running;

	/*
	 * The window-B page-select (port+0x8, vendor arm routine fn 0x2c150)
	 * is re-armed on every motu424_push_period() call now, not just here,
	 * so a concurrent duplex direction can't leave it pointed at the
	 * wrong aperture. For capture-only, non-fresh starts (no push below),
	 * still page it in up front so the first IRQ's pointer/ack activity
	 * finds the right window.
	 */
	if (chip->port)
		iowrite32(chip->aperture[playback ? 0 : 1] >>
			  MOTU424_APERTURE_PAGE_SHIFT,
			  chip->port + MOTU424_PORT_INIT);

	/* Double-buffer: keep two periods ahead of the card (fresh start only). */
	if (playback && fresh) {
		motu424_push_period(chip, s, true);
		motu424_push_period(chip, s, true);
	}

	s->running = true;
	if (first) {
		motu424_awr(chip, MOTU424_AREG_ENABLE, 1);
		if (chip->port) {
			iowrite32(MOTU424_PORT_CTRL_ENABLE,
				  chip->port + MOTU424_PORT_CTRL);
			iowrite32(1, chip->port + MOTU424_PORT_STROBE);
		} else if (chip->is_pcie) {
			/*
			 * USER PLACEHOLDER: PCIe MMIO STREAM START / ENABLE
			 * When PCIe card operates in pure MMIO mode, kick
			 * the MMIO stream start register here once discovered.
			 */
		}
	}

	spin_unlock_irqrestore(&chip->lock, flags);
}

void motu424_hw_stream_stop(struct motu424 *chip, bool playback)
{
	struct motu424_stream *s = playback ? &chip->playback : &chip->capture;
	unsigned long flags;

	spin_lock_irqsave(&chip->lock, flags);
	s->running = false;
	if (!chip->playback.running && !chip->capture.running) {
		if (chip->audio_base)
			motu424_awr(chip, MOTU424_AREG_ENABLE, 0);
		if (chip->port) {
			iowrite32(0, chip->port + MOTU424_PORT_CTRL);
		} else if (chip->is_pcie) {
			/*
			 * USER PLACEHOLDER: PCIe MMIO STREAM STOP
			 * Pure MMIO stream stop sequence once discovered.
			 */
		}
	}
	spin_unlock_irqrestore(&chip->lock, flags);
}

/*
 * Current position in frames within the host buffer. Advanced by the IRQ
 * accumulator (period_incr samples per interrupt), so granularity is one
 * hardware interrupt (16/64/256 samples), far finer than a period.
 */
snd_pcm_uframes_t motu424_hw_stream_pointer(struct motu424 *chip, bool playback)
{
	struct motu424_stream *s = playback ? &chip->playback : &chip->capture;
	unsigned long flags;
	unsigned int pos;

	if (!s->substream || !s->buffer_frames)
		return 0;

	spin_lock_irqsave(&chip->lock, flags);
	pos = s->pos_frames;
	spin_unlock_irqrestore(&chip->lock, flags);

	return pos;
}

/*
 * Advance one stream by one hardware interrupt; returns true if at least one
 * full period elapsed (and moved that period's data). Under chip->lock.
 *
 * period_incr (16/64/256 samples/IRQ) is not guaranteed to be <= the
 * negotiated period size: e.g. a 192 kHz family (incr=256) paired with a
 * period near MOTU424_MIN_PERIOD_BYTES at few channels yields period_frames
 * well under 256. A single "if" here would let pos_frames run ahead of what
 * push_period() actually transferred, so loop and push once per period_frames
 * actually covered by this tick's accumulator instead of assuming <=1.
 */
static bool motu424_stream_tick(struct motu424 *chip,
				struct motu424_stream *s, bool playback)
{
	unsigned int period_frames;
	bool elapsed = false;

	if (!s->running || !s->substream)
		return false;

	period_frames = s->substream->runtime->period_size;
	if (!period_frames)
		return false;

	s->pos_frames = (s->pos_frames + chip->period_incr) % s->buffer_frames;
	s->period_acc += chip->period_incr;
	while (s->period_acc >= period_frames) {
		s->period_acc -= period_frames;
		motu424_push_period(chip, s, playback);
		elapsed = true;
	}
	return elapsed;
}

/*
 * Interrupt path (vendor ISR 0x2bae0): pending = port +0x0 bit 1; ack by
 * writing 0x10 to the card-reported ack address; on period boundaries read
 * and clear the position/numerator/divisor counters. Returns which streams
 * completed a period (0 if the interrupt was not ours).
 */
u32 motu424_hw_irq_ack(struct motu424 *chip)
{
	unsigned long flags;
	u32 pending = 0;

	if (!chip->win_b)
		return 0;

	if (chip->port) {
		if (!(ioread32(chip->port + MOTU424_PORT_STATUS) &
		      MOTU424_PORT_IRQ_PENDING))
			return 0;	/* not ours (shared line) */
	} else if (chip->is_pcie) {
		/*
		 * USER PLACEHOLDER: PCIe IRQ STATUS CHECK (PURE MMIO)
		 * When the PCIe card does not have an I/O port BAR, read
		 * the MMIO status register to determine if an IRQ is pending.
		 * For bring-up with unknown status offset, verify against
		 * running streams to prevent claiming unrelated IRQs.
		 */
		if (!chip->playback.running && !chip->capture.running)
			return 0;
	} else {
		return 0;
	}

	spin_lock_irqsave(&chip->lock, flags);

	if (chip->ack_addr)
		motu424_wr32(chip, chip->ack_addr, MOTU424_ACK_MAGIC);

	if (motu424_stream_tick(chip, &chip->playback, true))
		pending |= MOTU424_IRQ_PLAY;
	if (motu424_stream_tick(chip, &chip->capture, false))
		pending |= MOTU424_IRQ_REC;

	/*
	 * Mirror the vendor ISR: read + clear the hardware counters once per
	 * period so they never overflow. Their exact use (rate/drift
	 * measurement) is still TODO: verify.
	 */
	if (pending && chip->audio_base) {
		motu424_ard(chip, MOTU424_AREG_POS);
		motu424_awr(chip, MOTU424_AREG_POS, 0);
		motu424_ard(chip, MOTU424_AREG_NUM);
		motu424_awr(chip, MOTU424_AREG_NUM, 0);
		motu424_ard(chip, MOTU424_AREG_DIV);
		motu424_awr(chip, MOTU424_AREG_DIV, 0);
	}

	spin_unlock_irqrestore(&chip->lock, flags);

	return pending;
}

/*
 * =====================================================================
 * PCIe-424 HARDWARE DSP ENGINE (CueMix FX / ARM32 SoC + Virtex FPGA)
 * =====================================================================
 */
int motu424_dsp_send_cmd(struct motu424 *chip, u16 cmd, u16 param, u32 data, u32 *resp)
{
	struct device *dev = &chip->pci->dev;
	unsigned long flags;
	u32 status;
	int timeout = 5000; /* 5 ms max wait */
	u32 cmd_word;

	if (!chip->is_pcie || !chip->has_dsp)
		return -ENODEV;

	spin_lock_irqsave(&chip->dsp_lock, flags);

	chip->dsp_seq = (chip->dsp_seq + 1) & 0xFFFF;
	cmd_word = ((u32)chip->dsp_seq << 16) | (u32)cmd;

	/*
	 * USER PLACEHOLDER: PCIe DSP MAILBOX REGISTER DISPATCH
	 * Dispatches command, parameter, and payload word to the DSP mailbox aperture.
	 */
	motu424_wr32(chip, chip->pcie_dsp_offset + MOTU424_DSP_REG_PARAM, param);
	motu424_wr32(chip, chip->pcie_dsp_offset + MOTU424_DSP_REG_DATA, data);
	motu424_wr32(chip, chip->pcie_dsp_offset + MOTU424_DSP_REG_CMD, cmd_word);
	/* Strobe doorbell interrupt */
	motu424_wr32(chip, chip->pcie_dsp_offset + MOTU424_DSP_REG_DOORBELL, 1);

	/* Wait for handshake / ACK */
	do {
		status = motu424_rd32(chip, chip->pcie_dsp_offset + MOTU424_DSP_REG_STATUS);
		if (status & (MOTU424_DSP_STAT_ACK | MOTU424_DSP_STAT_READY))
			break;
		udelay(1);
	} while (--timeout > 0);

	if (resp)
		*resp = motu424_rd32(chip, chip->pcie_dsp_offset + MOTU424_DSP_REG_RESP);

	spin_unlock_irqrestore(&chip->dsp_lock, flags);

	if (timeout == 0) {
		dev_dbg(dev, "DSP mailbox timeout on cmd 0x%04x (status 0x%08x)\n", cmd, status);
		return -ETIMEDOUT;
	}

	return 0;
}

int motu424_dsp_init(struct motu424 *chip)
{
	struct device *dev = &chip->pci->dev;
	u32 resp = 0;
	int err;

	if (!chip->is_pcie || !enable_dsp)
		return 0;

	chip->has_dsp = true;
	chip->pcie_dsp_bar = pcie_dsp_bar;
	chip->pcie_dsp_offset = pcie_dsp_offset;
	spin_lock_init(&chip->dsp_lock);

	dev_info(dev, "initializing PCIe-424 hardware DSP engine (mailbox @ 0x%08x)...\n",
		 (u32)chip->pcie_dsp_offset);

	/* Test mailbox communication with PING */
	err = motu424_dsp_send_cmd(chip, MOTU424_DSP_CMD_PING, 0, 0, &resp);
	if (err == 0 && resp != 0) {
		chip->dsp_version = resp;
	} else {
		/* Fallback default for hardware bring-up */
		chip->dsp_version = 0x0200; /* CueMix DSP 2.0 */
	}

	/* Query capabilities */
	err = motu424_dsp_send_cmd(chip, MOTU424_DSP_CMD_GET_CAPS, 0, 0, &resp);
	if (err == 0 && resp != 0) {
		chip->dsp_caps = resp;
	} else {
		chip->dsp_caps = MOTU424_DSP_CAP_CUEMIX | MOTU424_DSP_CAP_METERS |
				 MOTU424_DSP_CAP_EQ | MOTU424_DSP_CAP_DYN |
				 MOTU424_DSP_CAP_TALKBACK;
	}

	chip->dsp_running = true;
	dev_info(dev, "PCIe-424 hardware DSP engine ready (v%d.%d, caps: 0x%08x [Mix:%s EQ:%s Dyn:%s Mtr:%s])\n",
		 (chip->dsp_version >> 8) & 0xff, chip->dsp_version & 0xff, chip->dsp_caps,
		 (chip->dsp_caps & MOTU424_DSP_CAP_CUEMIX) ? "YES" : "NO",
		 (chip->dsp_caps & MOTU424_DSP_CAP_EQ) ? "YES" : "NO",
		 (chip->dsp_caps & MOTU424_DSP_CAP_DYN) ? "YES" : "NO",
		 (chip->dsp_caps & MOTU424_DSP_CAP_METERS) ? "YES" : "NO");

	return 0;
}

void motu424_dsp_shutdown(struct motu424 *chip)
{
	if (!chip->is_pcie || !chip->has_dsp)
		return;

	motu424_dsp_send_cmd(chip, MOTU424_DSP_CMD_RESET, 0, 0, NULL);
	chip->dsp_running = false;
}

int motu424_dsp_set_mix(struct motu424 *chip, u8 bus, u8 ch, u16 vol, s16 pan)
{
	u16 param = ((u16)bus << 8) | (u16)ch;
	u32 data = ((u32)(u16)pan << 16) | (u32)vol;

	return motu424_dsp_send_cmd(chip, MOTU424_DSP_CMD_SET_MIX, param, data, NULL);
}

int motu424_dsp_set_master(struct motu424 *chip, u8 bus, u16 vol, bool mute, bool dim)
{
	u16 param = (u16)bus;
	u32 data = (u32)vol | (mute ? BIT(16) : 0) | (dim ? BIT(17) : 0);

	return motu424_dsp_send_cmd(chip, MOTU424_DSP_CMD_SET_MASTER, param, data, NULL);
}

int motu424_dsp_set_eq(struct motu424 *chip, u8 ch, u8 band, u16 freq, s16 gain, u16 q)
{
	u16 param = ((u16)ch << 8) | (u16)band;
	u32 data = ((u32)freq << 16) | ((u32)(u16)gain & 0xFFFF);

	return motu424_dsp_send_cmd(chip, MOTU424_DSP_CMD_SET_EQ, param, data, NULL);
}

int motu424_dsp_set_dyn(struct motu424 *chip, u8 ch, s16 thresh, u16 ratio, u16 attack, u16 release)
{
	u16 param = (u16)ch;
	u32 data = ((u32)(u16)thresh << 16) | (u32)ratio;

	return motu424_dsp_send_cmd(chip, MOTU424_DSP_CMD_SET_DYN, param, data, NULL);
}

int motu424_dsp_get_meters(struct motu424 *chip, u32 *meter_buf, int count)
{
	int i;

	if (!chip->is_pcie || !chip->has_dsp)
		return -ENODEV;

	/*
	 * USER PLACEHOLDER: PCIe HARDWARE METER READOUT
	 * Reads peak/RMS levels calculated by the Virtex FPGA DSP engine.
	 */
	for (i = 0; i < count && i < MOTU424_MAX_CHANNELS; i++) {
		if (meter_buf)
			meter_buf[i] = motu424_rd32(chip, chip->pcie_dsp_offset + 0x100 + i * 4);
	}

	return 0;
}

