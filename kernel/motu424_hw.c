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
 *    the DSP raises one interrupt per IRQ_FRAMES frames (ack: RSTSRC INTRST)
 *    and reports its position as a host bus address at audio_base+0x2c.
 *
 * Only the PCI-424 (137a:0004) load path is implemented and tested.
 */
#include <linux/delay.h>
#include <linux/firmware.h>
#include <linux/io.h>
#include <linux/jiffies.h>
#include <linux/module.h>
#include <linux/sched.h>
#include <linux/unaligned.h>
#include <crypto/sha2.h>

#include "motu424.h"

static unsigned int frame_bytes;
module_param(frame_bytes, uint, 0444);
MODULE_PARM_DESC(frame_bytes,
		 "Host-ring bytes per frame (unconfirmed, see docs/transport.md); 0 = streaming disabled");

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
	if (!motu424_in_ram(chip->mix_base, MOTU424_MIX_DWORDS * 4)) {
		dev_err(dev, "DSP published an invalid mix_base 0x%08x\n",
			chip->mix_base);
		chip->audio_base = 0;
		return -EIO;
	}
	return 0;
}

/*
 * Step 5, the rest of the vendor init (fn 0x2c150): zero the mixer block,
 * audio_base+0x50 <- 1, then the EMIF CE0 timing the DSP needs to reach the
 * FPGA (the value a Windows-configured card reads back in CECTL0/CECTL1).
 */
static void motu424_dsp_init(struct motu424 *chip)
{
	unsigned int i;

	for (i = 0; i < MOTU424_MIX_DWORDS; i++)
		motu424_wr32(chip, chip->mix_base + 4 * i, 0);
	motu424_awr(chip, MOTU424_AREG_ENABLE_INIT, 1);
	motu424_wr32(chip, MOTU424_EMIF_CECTL0, MOTU424_CECTL0_INIT);
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

	chip->frame_bytes = frame_bytes;
	if (chip->frame_bytes && (chip->frame_bytes % 4 ||
				  MOTU424_RING_BYTES % chip->frame_bytes)) {
		dev_warn(dev, "frame_bytes=%u must be a multiple of 4 dividing 0x%x; streaming disabled\n",
			 chip->frame_bytes, MOTU424_RING_BYTES);
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
 * Start playback from the host ring at bus address @ring (MOTU424_RING_BYTES
 * long): ring base, size code, frames per interrupt, then enable; clear any
 * stale interrupt request and unmask PINTA#. Called from the atomic trigger.
 */
void motu424_hw_stream_start(struct motu424 *chip, dma_addr_t ring)
{
	unsigned long flags;

	spin_lock_irqsave(&chip->lock, flags);
	motu424_awr(chip, MOTU424_AREG_RING_BASE, lower_32_bits(ring));
	motu424_awr(chip, MOTU424_AREG_RING_SIZE, MOTU424_RING_BYTES);
	motu424_awr(chip, MOTU424_AREG_IRQ_FRAMES, MOTU424_IRQ_FRAMES_1X);
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
 * audio_base+0x2c (the vendor computes (value - ring) >> 2, in dwords).
 */
snd_pcm_uframes_t motu424_hw_stream_pointer(struct motu424 *chip, dma_addr_t ring)
{
	u32 off = motu424_ard(chip, MOTU424_AREG_DMAPOINT) - lower_32_bits(ring);

	if (off >= MOTU424_RING_BYTES)
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
