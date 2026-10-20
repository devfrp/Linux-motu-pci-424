// SPDX-License-Identifier: GPL-2.0-or-later
/*
 * motu424_hw.c - the hardware abstraction layer.
 *
 * =====================================================================
 * THIS IS THE ONLY FILE THAT ENCODES REAL MOTU PCI-324/424 SEMANTICS.
 * =====================================================================
 *
 * The model implemented here (docs/fpga-upload.md, docs/transport.md,
 * docs/register-map.md) was recovered from MOTUAW.sys and the card's DSP
 * program and checked on a real PCI-424 (issue #2):
 *
 *  - the card is a TMS320C6412 DSP in PCI boot mode plus an FPGA with no
 *    config PROM. At every start the host warm-resets the DSP, clocks the
 *    FPGA image out through the DSP's GPIO, writes the DSP program into
 *    internal RAM, releases the CPU and reads audio_base from a mailbox;
 *  - audio is DSP bus-master DMA: the host hands the DSP a coherent ring,
 *    the DSP raises one interrupt per period (ack: RSTSRC INTRST), wraps
 *    after RING_PERIODS periods and reports its position as a host bus
 *    address at audio_base+0x2c;
 *  - the device configure (issue #2, 2026-10-20) is the vendor's 48 kHz
 *    parameter group at load, an optional mixer program handed over by the
 *    relock sequence, and the channel map at every stream start.
 *
 * Only the PCI-424 (137a:0004) load path is implemented and tested.
 */
#include <linux/delay.h>
#include <linux/dma-mapping.h>
#include <linux/firmware.h>
#include <linux/io.h>
#include <linux/iopoll.h>
#include <linux/jiffies.h>
#include <linux/module.h>
#include <linux/sched.h>
#include <linux/unaligned.h>
#include <crypto/sha2.h>

#include "motu424.h"

static unsigned int frame_bytes;
module_param(frame_bytes, uint, 0444);
MODULE_PARM_DESC(frame_bytes,
		 "Host-ring bytes per frame (96 measured at 48 kHz, see docs/transport.md); 0 = streaming disabled");

static unsigned int period_bytes = MOTU424_PERIOD_BYTES;
module_param(period_bytes, uint, 0444);
MODULE_PARM_DESC(period_bytes,
		 "Host-ring bytes the DSP fetches per interrupt (0x1800 measured at 48 kHz with irq_frames=0x40)");

static unsigned int irq_frames = MOTU424_IRQ_FRAMES_1X;
module_param(irq_frames, uint, 0444);
MODULE_PARM_DESC(irq_frames,
		 "Frames per interrupt written to +0x60 (0x40 by default; diagnostic, see docs/transport.md)");

static bool configure = true;
module_param(configure, bool, 0444);
MODULE_PARM_DESC(configure,
		 "Write the vendor's 48 kHz parameter group at load and the channel map at stream start (default Y)");

MODULE_FIRMWARE(MOTU424_FW_FPGA);
MODULE_FIRMWARE(MOTU424_FW_PROG);

static const u8 motu424_fpga_sha256[SHA256_DIGEST_SIZE] = {
	0x34, 0xe6, 0x2c, 0xf5, 0xa7, 0x5f, 0x53, 0xa4,
	0x10, 0x04, 0x12, 0x9f, 0xf1, 0xfe, 0x6d, 0xc4,
	0x35, 0xa8, 0x7e, 0x14, 0x61, 0x4d, 0x79, 0xed,
	0xc6, 0xce, 0x39, 0x16, 0xe1, 0x8e, 0x04, 0xe2
};

static const u8 motu424_prog_sha256[SHA256_DIGEST_SIZE] = {
	0x3a, 0x23, 0xbc, 0x97, 0xe4, 0x91, 0x54, 0xe5,
	0xeb, 0xa8, 0x95, 0x66, 0x81, 0xee, 0x8d, 0xd0,
	0x21, 0x2c, 0xd7, 0x09, 0x1e, 0x8d, 0x67, 0x7c,
	0x27, 0x2c, 0xcb, 0x1a, 0x7f, 0xaa, 0x6e, 0x4f
};

/* The last three dwords of every CueMix mixer program the vendor builds. */
static const u32 motu424_mixprog_trailer[] = {
	0x001c00e2, 0x079803e2, 0x00008000
};

/*
 * The vendor's parameter group for 48 kHz at 1x (bring-up step 16, hw slot
 * 0x30: 0x2a820/0x2a710/0x2a9e0/0x2aab0) with its constructor defaults:
 * internal clock, latency 0, P = Q = 3000. K1/K2 are the firmware constants
 * at +0x44/+0x48. Committed by +0x114, which DSP routine 0x45e0 consumes.
 */
struct motu424_wr {
	u32 off;
	u32 val;
};

static const struct motu424_wr motu424_cfg_head[] = {
	{ 0x88, 1 },
	{ 0x8c, MOTU424_RING_BYTES },
	{ 0x90, 0 },
};

/* Written twice, before and after the +0x98 rate word. */
static const struct motu424_wr motu424_cfg_rate[] = {
	{ 0xc4, 0 },		/* latency: ms * rate / 1000 */
	{ 0xc8, 0 },
	{ 0x9c, 0x1e },		/* (Q + 50) / 100 */
	{ 0xa4, 0x4650 },	/* 6Q */
	{ 0xa0, 0x708 },	/* 6Q / 10 */
	{ 0xb0, 0x1e },		/* Q / 100 */
	{ 0xb4, 0xcccd },	/* (8P * 2^20 / rate + 5) / 10 */
	{ 0xb8, 0x492b4000 },	/* K1 * rate * 10 / 8P */
	{ 0xbc, 0x640 },	/* rate * 100 / P */
	{ 0xc0, 0 },
	{ 0xe0, 0x9c180 },	/* K2 (halved at 2x/4x) */
	{ 0xe4, 0xc31e00 },	/* (K2 * rate * 10 + 4P) / 8P */
	{ 0xf0, 0x223 },	/* 0x40000 / (K2 * rate / 64e6) */
	{ 0x114, 1 },		/* commit */
};

static const struct motu424_wr motu424_cfg_tail[] = {
	{ 0x84, 0 },		/* internal clock */
	{ 0xa8, 0 },
	{ 0x114, 1 },		/* commit again */
	{ 0x118, 1 },		/* the DSP clears it */
	{ 0xcc, 0 },		/* clock-source words */
	{ 0xd0, 0 },
	{ 0xd4, 0 },
	{ 0xdc, 0x7fff },	/* restored user setting */
};

#define MOTU424_RATE_WORD_48K	0x05355555u	/* +0x98, as the firmware boots */

/*
 * Channel map at [+0x14], as a Windows-configured card with a 2408mk3 and a
 * 24I/O held it: the 14 (= +0x68) playback channels 24..37, one byte each
 * as ch + ch / 24, at byte index i ^ 3.
 */
static const u32 motu424_chmap[MOTU424_CHMAP_DWORDS] = {
	0x191a1b1c, 0x1d1e1f20, 0x21222324, 0x25260000,
};

/* --- windowed card-address dispatch (vendor accessors 0x29110/0x29160) --- */
static bool motu424_is_wina(u32 card_addr)
{
	return (card_addr & MOTU424_WINA_TAG_MASK) == MOTU424_WINA_TAG;
}

static void __iomem *motu424_addr(struct motu424 *chip, u32 card_addr)
{
	if (motu424_is_wina(card_addr))
		return chip->win_a + (card_addr & MOTU424_WINA_MASK);
	return chip->win_b + (card_addr & MOTU424_WINB_MASK);
}

/* Window B is only DSP internal RAM here (DSPP = 0); the rest is reserved. */
static bool motu424_in_ram(u32 card_addr, u32 len)
{
	return !(card_addr & 3) && card_addr < MOTU424_RAM_LEN &&
	       len <= MOTU424_RAM_LEN - card_addr;
}

static u32 motu424_rd32(struct motu424 *chip, u32 card_addr)
{
	if (!motu424_is_wina(card_addr) && !motu424_in_ram(card_addr, 4)) {
		dev_WARN_ONCE(&chip->pci->dev, 1,
			      "refusing read of reserved card address 0x%08x\n",
			      card_addr);
		return ~0u;
	}
	return ioread32(motu424_addr(chip, card_addr));
}

/*
 * Allow-list for writes: DSP internal RAM in window B, and in window A only
 * the five peripheral registers the driver needs. Anything else, the DSP
 * EEPROM controller (the card's PCI identity) included, is refused.
 */
static bool motu424_write_allowed(u32 card_addr)
{
	if (!motu424_is_wina(card_addr))
		return motu424_in_ram(card_addr, 4);

	switch (card_addr) {
	case MOTU424_EMIF_CECTL0:
	case MOTU424_GPIO_GPEN:
	case MOTU424_GPIO_GPDIR:
	case MOTU424_GPIO_GPVAL:
	case MOTU424_PCI_RSTSRC:
		return true;
	default:
		return false;
	}
}

static void motu424_wr32(struct motu424 *chip, u32 card_addr, u32 val)
{
	if (!motu424_write_allowed(card_addr)) {
		dev_WARN_ONCE(&chip->pci->dev, 1,
			      "refusing write to card address 0x%08x (not on the allow-list)\n",
			      card_addr);
		return;
	}
	iowrite32(val, motu424_addr(chip, card_addr));
}

/* Audio-block helpers: offsets from the DSP-published audio_base. */
static inline void motu424_awr(struct motu424 *chip, u32 off, u32 val)
{
	motu424_wr32(chip, chip->audio_base + off, val);
}

static inline u32 motu424_ard(struct motu424 *chip, u32 off)
{
	return motu424_rd32(chip, chip->audio_base + off);
}

static inline void motu424_port_wr(struct motu424 *chip, u32 off, u32 val)
{
	iowrite32(val, chip->port + off);
}

/*
 * Assign the mapped BARs to their hardware roles from their types and sizes:
 * the I/O BAR is the HPI register set, the 8 MB MMIO BAR is window A (DSP
 * peripherals), the 4 MB one window B (DSP memory). All three are required.
 */
static int motu424_assign_windows(struct motu424 *chip)
{
	int i;

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
			else if (b->len >= MOTU424_WINB_LEN && !chip->win_b)
				chip->win_b = b->ptr;
		}
	}
	if (!chip->port || !chip->win_a || !chip->win_b) {
		dev_err(&chip->pci->dev,
			"expected an I/O BAR, an 8 MB and a 4 MB MMIO BAR\n");
		return -ENODEV;
	}
	return 0;
}

static int motu424_fw_get(struct motu424 *chip, const struct firmware **fw,
			  const char *name, size_t len, const u8 *sha)
{
	struct device *dev = &chip->pci->dev;
	u8 digest[SHA256_DIGEST_SIZE];
	int err;

	err = request_firmware(fw, name, dev);
	if (err < 0) {
		dev_err(dev,
			"cannot load %s (%d): extract it from your own MOTUAW.sys with tools/re/extract-firmware.py and install it under /lib/firmware/motu424/\n",
			name, err);
		return err;
	}
	if ((*fw)->size != len) {
		dev_err(dev, "%s: %zu bytes, expected %zu\n", name,
			(*fw)->size, len);
		goto bad;
	}
	sha256((*fw)->data, len, digest);
	if (memcmp(digest, sha, sizeof(digest))) {
		dev_err(dev, "%s: SHA-256 mismatch, not the expected image\n",
			name);
		goto bad;
	}
	return 0;

bad:
	release_firmware(*fw);
	*fw = NULL;
	return -EINVAL;
}

/*
 * Step 2: clock the FPGA image out through the DSP GPIO (vendor fn 0x251e0,
 * 0x25170, 0x25230): nCONFIG low then high, then per byte LSB first one DCLK
 * pulse per bit with DATA0 stable, then 1000 trailing clocks. The vendor
 * reads no nSTATUS/CONF_DONE; whether the FPGA took the image shows in
 * step 3, since window B only decodes once it is configured.
 */
static void motu424_fpga_load(struct motu424 *chip, const u8 *img, size_t len)
{
	const u32 pins = MOTU424_GP_DATA0 | MOTU424_GP_NCONFIG | MOTU424_GP_DCLK;
	size_t i;
	int bit;

	motu424_wr32(chip, MOTU424_GPIO_GPEN, pins);
	motu424_wr32(chip, MOTU424_GPIO_GPDIR, pins);
	motu424_wr32(chip, MOTU424_GPIO_GPVAL, 0);
	motu424_rd32(chip, MOTU424_GPIO_GPVAL);		/* flush posted writes */
	usleep_range(100, 200);
	motu424_wr32(chip, MOTU424_GPIO_GPVAL, MOTU424_GP_NCONFIG);
	motu424_rd32(chip, MOTU424_GPIO_GPVAL);
	usleep_range(5000, 6000);

	for (i = 0; i < len; i++) {
		for (bit = 0; bit < 8; bit++) {
			u32 v = MOTU424_GP_NCONFIG |
				((img[i] >> bit) & 1 ? MOTU424_GP_DATA0 : 0);

			motu424_wr32(chip, MOTU424_GPIO_GPVAL, v);
			motu424_wr32(chip, MOTU424_GPIO_GPVAL, v | MOTU424_GP_DCLK);
			motu424_wr32(chip, MOTU424_GPIO_GPVAL, v);
		}
		cond_resched();
	}
	for (i = 0; i < 1000; i++) {
		motu424_wr32(chip, MOTU424_GPIO_GPVAL,
			     MOTU424_GP_NCONFIG | MOTU424_GP_DCLK);
		motu424_wr32(chip, MOTU424_GPIO_GPVAL, MOTU424_GP_NCONFIG);
	}
	motu424_wr32(chip, MOTU424_GPIO_GPDIR,
		     MOTU424_GP_NCONFIG | MOTU424_GP_DCLK);
}

/*
 * Step 3: zero DSP internal RAM and write the program at 0, then read it
 * back. The vendor writes (len >> 2) + 1 dwords from a zeroed buffer; the
 * extra dword is already zero after the fill.
 */
static int motu424_prog_load(struct motu424 *chip, const u8 *prog, size_t len)
{
	unsigned int bad = 0;
	u32 off;

	motu424_port_wr(chip, MOTU424_PORT_DSPP, 0);
	for (off = 0; off < MOTU424_RAM_LEN; off += 4) {
		motu424_wr32(chip, off, 0);
		if (!(off & 0xfff))
			cond_resched();
	}
	for (off = 0; off + 4 <= len; off += 4)
		motu424_wr32(chip, off, get_unaligned_le32(prog + off));

	for (off = 0; off + 4 <= len; off += 4)
		if (motu424_rd32(chip, off) != get_unaligned_le32(prog + off))
			bad++;
	if (bad) {
		dev_err(&chip->pci->dev,
			"DSP program read-back: %u of %zu dwords differ (FPGA not configured?)\n",
			bad, len / 4);
		return -EIO;
	}
	return 0;
}

/*
 * Step 4: clear the mailbox, release the CPU with DSPINT and wait for the
 * program to publish audio_base there (the card did so on the first poll).
 */
static int motu424_dsp_start(struct motu424 *chip)
{
	struct device *dev = &chip->pci->dev;
	unsigned long timeout;
	u32 ab;

	motu424_wr32(chip, MOTU424_MAILBOX, 0);
	if (motu424_rd32(chip, MOTU424_MAILBOX)) {
		dev_err(dev, "mailbox did not clear\n");
		return -EIO;
	}
	motu424_port_wr(chip, MOTU424_PORT_HDCR, MOTU424_PORT_HDCR_DSPINT);

	timeout = jiffies + 3 * HZ;
	while (!(ab = motu424_rd32(chip, MOTU424_MAILBOX))) {
		if (time_after(jiffies, timeout)) {
			dev_err(dev, "DSP did not publish audio_base\n");
			return -ETIMEDOUT;
		}
		usleep_range(10, 50);
	}
	if (!motu424_in_ram(ab, 0x140)) {
		dev_err(dev, "DSP published an invalid audio_base 0x%08x\n", ab);
		return -EIO;
	}
	chip->audio_base = ab;

	chip->mix_base = motu424_ard(chip, MOTU424_AREG_MIXBASE);
	chip->chmap_base = motu424_ard(chip, MOTU424_AREG_CHMAP);
	if (!motu424_in_ram(chip->mix_base, MOTU424_MIX_DWORDS * 4) ||
	    !motu424_in_ram(chip->chmap_base, MOTU424_CHMAP_DWORDS * 4)) {
		dev_err(dev, "DSP published an invalid mix_base 0x%08x or channel map 0x%08x\n",
			chip->mix_base, chip->chmap_base);
		chip->audio_base = 0;
		return -EIO;
	}
	return 0;
}

static void motu424_awr_list(struct motu424 *chip,
			     const struct motu424_wr *w, size_t n)
{
	for (; n; n--, w++)
		motu424_awr(chip, w->off, w->val);
}

/*
 * The vendor's parameter group for 48 kHz (bring-up step 16), in its order.
 * Half the values derive from +0x44/+0x48, so nothing is written unless the
 * program left the constants they were computed from.
 */
static void motu424_dsp_configure(struct motu424 *chip)
{
	struct device *dev = &chip->pci->dev;
	u32 k1 = motu424_ard(chip, MOTU424_AREG_FW_K1);
	u32 k2 = motu424_ard(chip, MOTU424_AREG_FW_K2);

	if (k1 != MOTU424_FW_K1_VAL || k2 != MOTU424_FW_K2_VAL) {
		dev_warn(dev, "+0x44/+0x48 = 0x%x/0x%x, expected 0x%x/0x%x: parameter group not written\n",
			 k1, k2, MOTU424_FW_K1_VAL, MOTU424_FW_K2_VAL);
		return;
	}
	motu424_awr_list(chip, motu424_cfg_head, ARRAY_SIZE(motu424_cfg_head));
	motu424_awr_list(chip, motu424_cfg_rate, ARRAY_SIZE(motu424_cfg_rate));
	motu424_awr(chip, 0x98, MOTU424_RATE_WORD_48K);
	motu424_awr_list(chip, motu424_cfg_rate, ARRAY_SIZE(motu424_cfg_rate));
	motu424_awr_list(chip, motu424_cfg_tail, ARRAY_SIZE(motu424_cfg_tail));
	dev_info(dev, "48 kHz parameter group written\n");
}

/*
 * Step 5, the rest of the vendor init (fn 0x2c150): zero the mixer block,
 * the parameter group, audio_base+0x50 <- 1, then the EMIF CE0 timing the
 * DSP needs to reach the FPGA (the value a Windows-configured card reads
 * back in CECTL0/CECTL1).
 */
static void motu424_dsp_init(struct motu424 *chip)
{
	unsigned int i;

	for (i = 0; i < MOTU424_MIX_DWORDS; i++)
		motu424_wr32(chip, chip->mix_base + 4 * i, 0);
	if (configure)
		motu424_dsp_configure(chip);
	motu424_awr(chip, MOTU424_AREG_ENABLE_INIT, 1);
	motu424_wr32(chip, MOTU424_EMIF_CECTL0, MOTU424_CECTL0_INIT);
}

/*
 * The optional mixer program: a multiple of 4 bytes that fits a DSP program
 * buffer and ends with the vendor's trailer. It goes into a coherent buffer
 * now, while the DSP cannot master the bus yet; motu424_hw_relock() hands it
 * over. The buffer is devres-managed and registered before the shutdown
 * action, so it outlives any DSP access to it.
 */
static void motu424_mixer_get(struct motu424 *chip)
{
	struct device *dev = &chip->pci->dev;
	const size_t tlen = sizeof(motu424_mixprog_trailer);
	const struct firmware *fw;
	size_t i, n;

	if (request_firmware_direct(&fw, MOTU424_FW_MIXER, dev)) {
		dev_info(dev, "no %s: the DSP runs without a mixer program\n",
			 MOTU424_FW_MIXER);
		return;
	}
	n = fw->size;
	if (n % 4 || n < tlen || n > MOTU424_MIXPROG_LEN)
		goto bad;
	for (i = 0; i < ARRAY_SIZE(motu424_mixprog_trailer); i++)
		if (get_unaligned_le32(fw->data + n - tlen + 4 * i) !=
		    motu424_mixprog_trailer[i])
			goto bad;

	chip->mixer_buf = dmam_alloc_coherent(dev, MOTU424_MIXPROG_LEN,
					      &chip->mixer_bus, GFP_KERNEL);
	if (chip->mixer_buf) {
		memcpy(chip->mixer_buf, fw->data, n);
		chip->mixer_len = n;
	}
	release_firmware(fw);
	return;

bad:
	dev_warn(dev, "%s: %zu bytes, not a mixer program (multiple of 4, at most 0x%x, ending with the CueMix trailer); ignored\n",
		 MOTU424_FW_MIXER, n, MOTU424_MIXPROG_LEN);
	release_firmware(fw);
}

/*
 * Load and start the card: the vendor DEV_0004 sequence (docs/fpga-upload.md),
 * as run on hardware by tools/motu424-bringup. Sleeps; called from probe
 * before the IRQ is requested, with PINTA# masked throughout.
 */
int motu424_hw_init(struct motu424 *chip)
{
	struct device *dev = &chip->pci->dev;
	const struct firmware *fpga = NULL, *prog = NULL;
	int err;

	err = motu424_assign_windows(chip);
	if (err < 0)
		return err;

	err = motu424_fw_get(chip, &fpga, MOTU424_FW_FPGA,
			     MOTU424_FW_FPGA_LEN, motu424_fpga_sha256);
	if (err < 0)
		return err;
	err = motu424_fw_get(chip, &prog, MOTU424_FW_PROG,
			     MOTU424_FW_PROG_LEN, motu424_prog_sha256);
	if (err < 0)
		goto out;

	motu424_port_wr(chip, MOTU424_PORT_HSR, MOTU424_PORT_HSR_INTAM);
	motu424_port_wr(chip, MOTU424_PORT_HDCR, MOTU424_PORT_HDCR_WARMRESET);
	usleep_range(1000, 2000);

	motu424_fpga_load(chip, fpga->data, fpga->size);
	err = motu424_prog_load(chip, prog->data, prog->size);
	if (!err)
		err = motu424_dsp_start(chip);
	if (err) {
		motu424_port_wr(chip, MOTU424_PORT_HDCR,
				MOTU424_PORT_HDCR_WARMRESET);
		goto out;
	}
	motu424_dsp_init(chip);
	motu424_mixer_get(chip);

	chip->frame_bytes = frame_bytes;
	chip->period_bytes = period_bytes;
	chip->irq_frames = irq_frames;
	if (!chip->irq_frames || chip->irq_frames > MOTU424_IRQ_FRAMES_MAX) {
		dev_warn(dev, "irq_frames=0x%x out of range (1..0x%x); using 0x%x\n",
			 chip->irq_frames, MOTU424_IRQ_FRAMES_MAX,
			 MOTU424_IRQ_FRAMES_1X);
		chip->irq_frames = MOTU424_IRQ_FRAMES_1X;
	}
	if (chip->frame_bytes && (chip->frame_bytes % 4 || !chip->period_bytes ||
				  chip->period_bytes % chip->frame_bytes ||
				  chip->period_bytes > MOTU424_RING_BYTES / MOTU424_RING_PERIODS)) {
		dev_warn(dev, "frame_bytes=%u must be a multiple of 4 dividing period_bytes=0x%x (at most 0x%x); streaming disabled\n",
			 chip->frame_bytes, chip->period_bytes,
			 MOTU424_RING_BYTES / MOTU424_RING_PERIODS);
		chip->frame_bytes = 0;
	}
	dev_info(dev, "DSP running: audio_base=0x%x mix_base=0x%x%s\n",
		 chip->audio_base, chip->mix_base,
		 chip->frame_bytes ? "" : "; streaming disabled until frame_bytes is set");

out:
	release_firmware(prog);
	release_firmware(fpga);
	return err;
}

/*
 * Hand the DSP the mixer program, if one was loaded (vendor relock
 * 0x2b7f0, once at load here). The DSP copies it by bus-master DMA from the
 * host buffer into the next of its two program buffers (+0x0c/+0x10, used
 * in turn), reading host memory only, then switches to it at a safe point.
 * Probe calls this after pci_set_master() and the shutdown action. A
 * failure is reported and leaves the DSP on no program, as before.
 */
void motu424_hw_relock(struct motu424 *chip)
{
	struct device *dev = &chip->pci->dev;
	u32 dst, v;
	int err;

	if (!chip->mixer_len || !chip->audio_base)
		return;

	chip->mixer_idx ^= 1;
	dst = motu424_ard(chip, MOTU424_AREG_MIXPROG_BUF + 4 * chip->mixer_idx);
	if (!motu424_in_ram(dst, MOTU424_MIXPROG_LEN)) {
		dev_warn(dev, "invalid DSP mixer-program buffer 0x%08x\n", dst);
		return;
	}

	motu424_awr(chip, MOTU424_AREG_COPY_SRC, lower_32_bits(chip->mixer_bus));
	motu424_awr(chip, MOTU424_AREG_COPY_DST, dst);
	motu424_awr(chip, MOTU424_AREG_COPY_LEN, chip->mixer_len);
	motu424_awr(chip, MOTU424_AREG_COPY_ARM, 1);
	/* The vendor polls every 20 us, at most 10000 times. */
	err = read_poll_timeout(motu424_ard, v, !v, 20, 200000, false,
				chip, MOTU424_AREG_COPY_LEN);
	motu424_awr(chip, MOTU424_AREG_COPY_ARM, 0);
	if (err) {
		dev_warn(dev, "DSP did not finish the mixer-program copy (+0x100 = 0x%x)\n",
			 v);
		return;
	}
	err = read_poll_timeout(motu424_ard, v, v, 5, 100000, false,
				chip, MOTU424_AREG_MIXPROG_READY);
	if (err) {
		dev_warn(dev, "DSP never signalled a safe mixer-program swap (+0x108)\n");
		return;
	}
	motu424_awr(chip, MOTU424_AREG_MIXPROG_READY, 0);
	motu424_awr(chip, MOTU424_AREG_MIXPROG, dst);
	motu424_awr(chip, MOTU424_AREG_RELOCK_W, MOTU424_RELOCK_W_1X);
	motu424_awr(chip, MOTU424_AREG_MIXPROG_SWAP, 1);
	dev_info(dev, "mixer program: 0x%zx bytes handed to the DSP at 0x%x\n",
		 chip->mixer_len, dst);
}

/*
 * Quiesce the card before its IRQ and the host ring go away: stop the
 * stream, mask PINTA# and warm-reset the DSP, which also ends its bus-master
 * accesses to host memory.
 */
void motu424_hw_shutdown(struct motu424 *chip)
{
	unsigned long flags;

	if (!chip->port)
		return;

	spin_lock_irqsave(&chip->lock, flags);
	if (chip->audio_base) {
		motu424_awr(chip, MOTU424_AREG_STOP_ACK, 0);
		motu424_awr(chip, MOTU424_AREG_ENABLE, 0);
	}
	motu424_port_wr(chip, MOTU424_PORT_HSR, MOTU424_PORT_HSR_INTAM);
	motu424_port_wr(chip, MOTU424_PORT_HDCR, MOTU424_PORT_HDCR_WARMRESET);
	chip->audio_base = 0;
	chip->playback.running = false;
	spin_unlock_irqrestore(&chip->lock, flags);
}

/*
 * Start playback from the host ring at bus address @ring: what the vendor's
 * configure path (0x26760) writes at every stream start, the channel map and
 * fetch geometry, then ring base, size code, frames per interrupt and
 * periods per wrap, then enable; clear any stale interrupt request and
 * unmask PINTA#. Called from the atomic trigger.
 */
void motu424_hw_stream_start(struct motu424 *chip, dma_addr_t ring)
{
	unsigned long flags;
	unsigned int i;

	spin_lock_irqsave(&chip->lock, flags);
	if (configure)
		for (i = 0; i < MOTU424_CHMAP_DWORDS; i++)
			motu424_wr32(chip, chip->chmap_base + 4 * i,
				     motu424_chmap[i]);
	motu424_awr(chip, MOTU424_AREG_CFG_C, MOTU424_CFG_C);
	motu424_awr(chip, MOTU424_AREG_XFER_A, MOTU424_XFER_A);
	motu424_awr(chip, MOTU424_AREG_XFER_B, MOTU424_XFER_B);
	motu424_awr(chip, MOTU424_AREG_RING_BASE, lower_32_bits(ring));
	motu424_awr(chip, MOTU424_AREG_RING_SIZE, MOTU424_RING_BYTES);
	motu424_awr(chip, MOTU424_AREG_IRQ_FRAMES, chip->irq_frames);
	motu424_awr(chip, MOTU424_AREG_RING_PERIODS, MOTU424_RING_PERIODS);
	motu424_awr(chip, MOTU424_AREG_ENABLE, 1);
	motu424_wr32(chip, MOTU424_PCI_RSTSRC, MOTU424_RSTSRC_INTRST);
	motu424_port_wr(chip, MOTU424_PORT_HSR, 0);
	chip->playback.running = true;
	spin_unlock_irqrestore(&chip->lock, flags);
}

/*
 * Stop: clear the stop acknowledge, disable, mask PINTA#. The DSP sets
 * +0xf4 again once it has really stopped; motu424_hw_stream_sync_stop()
 * waits for that outside the atomic trigger.
 */
void motu424_hw_stream_stop(struct motu424 *chip)
{
	unsigned long flags;

	spin_lock_irqsave(&chip->lock, flags);
	chip->playback.running = false;
	if (chip->audio_base) {
		motu424_awr(chip, MOTU424_AREG_STOP_ACK, 0);
		motu424_awr(chip, MOTU424_AREG_ENABLE, 0);
	}
	motu424_port_wr(chip, MOTU424_PORT_HSR, MOTU424_PORT_HSR_INTAM);
	spin_unlock_irqrestore(&chip->lock, flags);
}

int motu424_hw_stream_sync_stop(struct motu424 *chip)
{
	unsigned long timeout = jiffies + msecs_to_jiffies(100);

	if (!chip->audio_base)
		return 0;
	while (!motu424_ard(chip, MOTU424_AREG_STOP_ACK)) {
		if (time_after(jiffies, timeout)) {
			dev_warn(&chip->pci->dev, "DSP did not acknowledge the stop\n");
			return -ETIMEDOUT;
		}
		usleep_range(100, 200);
	}
	return 0;
}

/*
 * Position: the DSP stores the host bus address it has reached at
 * audio_base+0x2c (the vendor computes (value - ring) >> 2, in dwords) and
 * resets it to the ring base every RING_PERIODS periods.
 */
snd_pcm_uframes_t motu424_hw_stream_pointer(struct motu424 *chip, dma_addr_t ring)
{
	u32 off = motu424_ard(chip, MOTU424_AREG_DMAPOINT) - lower_32_bits(ring);

	if (off >= MOTU424_RING_PERIODS * chip->period_bytes)
		return 0;
	return off / chip->frame_bytes;
}

/*
 * Interrupt (vendor ISR 0x2bae0): pending = HSR bit 1; ack by writing INTRST
 * to RSTSRC, which clears the request the DSP raised with INTREQ.
 */
u32 motu424_hw_irq_ack(struct motu424 *chip)
{
	u32 pending = MOTU424_IRQ_OURS;

	if (!(ioread32(chip->port + MOTU424_PORT_HSR) &
	      MOTU424_PORT_IRQ_PENDING))
		return 0;	/* not ours (shared line) */

	spin_lock(&chip->lock);
	motu424_wr32(chip, MOTU424_PCI_RSTSRC, MOTU424_RSTSRC_INTRST);
	if (chip->playback.running)
		pending |= MOTU424_IRQ_PLAY;
	spin_unlock(&chip->lock);

	return pending;
}
