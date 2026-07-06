/* SPDX-License-Identifier: GPL-2.0-or-later */
/*
 * motu424 - Linux ALSA driver for the MOTU PCI-324 / PCI-424 audio card.
 *
 * Shared definitions: hardware register map, driver state structures and
 * inter-module prototypes.
 *
 * ---------------------------------------------------------------------------
 * IMPORTANT - REVERSE ENGINEERING STATUS
 * ---------------------------------------------------------------------------
 * The MOTU PCI-324/424 is undocumented hardware. Everything in the "HARDWARE
 * INTERFACE (unverified)" section below is a *hypothesis* about the register
 * layout and MUST be confirmed against a real card (see tools/motu424-probe.c
 * and docs in README.md). The PCI/DMA/IRQ/ALSA framework around it is standard
 * and correct; only these constants gate real audio.
 *
 * All accesses to these constants are funnelled through motu424_hw.c so that,
 * once the true layout is known, a single file needs changing.
 */
#ifndef MOTU424_H
#define MOTU424_H

#include <linux/pci.h>
#include <linux/io.h>
#include <linux/spinlock.h>
#include <sound/core.h>
#include <sound/pcm.h>

#define MOTU424_DRIVER_NAME	"motu424"

/* --------------------------------------------------------------------------
 * PCI identity - CONFIRMED from the vendor driver package (MOTUAW.inf,
 * "MOTU Audio Installer 4.0.6"). 0x137A is Mark of the Unicorn's real PCI
 * vendor ID (an earlier 0x1221 guess was wrong).
 *
 *   PCI\VEN_137A&DEV_0003/0004/0005  -> PCI-424 AudioWire audio (this driver)
 *   PCI\VEN_137A&DEV_0006            -> PCI video (out of scope)
 *   PCI\VEN_137A&DEV_0007            -> further variant
 * The three 0003..0005 IDs are the PCI-324 / PCI-424 / PCIe-424 (HD Express)
 * card revisions, all handled by the vendor's "DriverInstall424" section.
 */
#define MOTU424_VENDOR_ID		0x137A	/* Mark of the Unicorn */
#define MOTU424_DEV_PCI424_A		0x0003
#define MOTU424_DEV_PCI424_B		0x0004
#define MOTU424_DEV_PCI424_C		0x0005

/* --------------------------------------------------------------------------
 * HARDWARE MODEL - partially CONFIRMED from static RE of vendor MOTUAW.sys
 * --------------------------------------------------------------------------
 * (PE32 i386, image base 0x10000; src files PCI424Driver.cpp /
 * PCI424NanoDriver.cpp / AW2408.cpp). What the vendor driver actually does:
 *
 *   1. The card is NOT a small offset-based register file. It exposes a
 *      windowed ~24-bit "card address" space over TWO mapped MMIO regions,
 *      accessed with READ/WRITE_REGISTER_ULONG (LE 32-bit). A 32-bit card
 *      address is dispatched by testing (addr & 0xff800000) == 0x01800000:
 *        - true  -> window A: (addr & 0x7fffff) + base_A   (23-bit / 8 MB)
 *        - false -> window B: (addr & 0x3fffff) + base_B   (22-bit / 4 MB)
 *      Concrete card addresses observed: 0xC0024 / 0x100024 (per-bank ctrl/status
 *      at bank+0x24, bank stride 0x40000, window B); same banks via window A at
 *      0x18C0000 / 0x1900000. NB: 0xAC44 is NOT an address - it is 44100 decimal,
 *      a sample-rate constant. See docs/register-map.md and docs/transport.md.
 *
 *   2. A third, small I/O-PORT BAR (READ/WRITE_PORT_ULONG) carries a few dwords
 *      of bridge/GPIO control: read at +0x0, writes at +0x4 (value 1) and +0x8.
 *      Consistent with a PLX-style PCI local-bus bridge fronting an FPGA.
 *
 *   3. Audio transport is PIO into a card-side aperture, NOT a host bus-master
 *      ring: the driver pushes host buffers into window B with
 *      WRITE_REGISTER_BUFFER_ULONG and tracks a hardware "dmaPoint" plus
 *      software readHead/writeHead/len (vendor debug string:
 *      "ReadHead out of bounds: dmaPoint %08x, readHead %08x, writeHead %08x,
 *      len %08x"). => Handing the card runtime->dma_addr as a ring base (what
 *      motu424_hw.c currently assumes) is almost certainly WRONG.
 *
 *   4. FPGA firmware - VERDICT from exhaustive static RE: the classic PCI-324/
 *      424 is NOT host-uploaded. MOTUAW.sys has no file I/O, no code xref to the
 *      "altera424b.rbf" string (a build/legacy artefact), no firmware IOCTL and
 *      no large-buffer config write - so the parallel-PCI card's Altera FPGA
 *      self-configures from onboard flash/EEPROM at power-on. The Linux driver
 *      likely needs NO request_firmware() for the classic card (verify on hw).
 *      Only the PCIe HD Express variant (HDExpress_FullImageRun.bin, an ARM SoC
 *      + Xilinx image) takes a host upload. See docs/fpga-upload.md.
 *
 * The MOTU424_REG_*, CTRL_*, STAT_* constants below are the ORIGINAL hypothesis
 * and are retained only so the existing framework builds. They do NOT match the
 * windowed model above and must be reworked in motu424_hw.c once the concrete
 * register semantics (rate/clock/dmaPoint/IRQ) are decoded. Do not trust them.
 */
#define MOTU424_BAR		0		/* control BAR index (window B) */

#define MOTU424_REG_CONTROL	0x00	/* rw: global control (see CTRL_* bits) */
#define MOTU424_REG_STATUS	0x04	/* r : global status (see STAT_* bits)  */
#define MOTU424_REG_CLOCK	0x08	/* rw: clock source / sample-rate mode  */
#define MOTU424_REG_IRQ_STATUS	0x0c	/* r/w1c: pending interrupt sources     */
#define MOTU424_REG_IRQ_MASK	0x10	/* rw: interrupt enable mask            */
#define MOTU424_REG_DMA_PLAY	0x14	/* rw: playback DMA ring base (bus addr)*/
#define MOTU424_REG_DMA_REC	0x18	/* rw: capture  DMA ring base (bus addr)*/
#define MOTU424_REG_DMA_PLAY_POS 0x1c	/* r : playback DMA byte position       */
#define MOTU424_REG_DMA_REC_POS	0x20	/* r : capture  DMA byte position       */
#define MOTU424_REG_PLAY_SIZE	0x24	/* rw: playback ring size in bytes      */
#define MOTU424_REG_REC_SIZE	0x28	/* rw: capture  ring size in bytes      */
#define MOTU424_REG_PLAY_PERIOD	0x2c	/* rw: playback period (IRQ) size bytes */
#define MOTU424_REG_REC_PERIOD	0x30	/* rw: capture  period (IRQ) size bytes */

/* MOTU424_REG_CONTROL bits */
#define MOTU424_CTRL_RESET	(1u << 0)	/* soft reset the card          */
#define MOTU424_CTRL_PLAY_EN	(1u << 1)	/* enable playback DMA engine   */
#define MOTU424_CTRL_REC_EN	(1u << 2)	/* enable capture DMA engine    */
#define MOTU424_CTRL_IRQ_EN	(1u << 3)	/* master interrupt enable      */

/* MOTU424_REG_STATUS bits */
#define MOTU424_STAT_LOCKED	(1u << 0)	/* clock PLL locked             */

/* MOTU424_REG_IRQ_STATUS / IRQ_MASK bits */
#define MOTU424_IRQ_PLAY	(1u << 0)	/* playback period elapsed      */
#define MOTU424_IRQ_REC		(1u << 1)	/* capture  period elapsed      */
#define MOTU424_IRQ_MASK_ALL	(MOTU424_IRQ_PLAY | MOTU424_IRQ_REC)

/* MOTU424_REG_CLOCK: sample-rate family. The card runs 1x/2x/4x families; the
 * channel count of the fixed frame shrinks as the rate family rises. */
#define MOTU424_CLK_INTERNAL	(0u << 4)	/* internal clock source        */
#define MOTU424_CLK_RATE_1X	(0u << 0)	/* 44.1 / 48 kHz                */
#define MOTU424_CLK_RATE_2X	(1u << 0)	/* 88.2 / 96 kHz                */
#define MOTU424_CLK_RATE_4X	(2u << 0)	/* 176.4 / 192 kHz              */
#define MOTU424_CLK_BASE_44100	(1u << 2)	/* 44.1 kHz family (else 48)    */

/* --------------------------------------------------------------------------
 * Audio format constants
 * --------------------------------------------------------------------------
 * The card's native wire format is 24-bit big-endian samples packed 3 bytes
 * per channel per frame ("event"). We expose S24_3LE and repack, or S24_3BE
 * directly; kept as one place so it can be tuned once the format is confirmed.
 */
#define MOTU424_BYTES_PER_SAMPLE	3
#define MOTU424_MAX_CHANNELS		24	/* e.g. one 2408 mk3 bank */
#define MOTU424_MIN_CHANNELS		2

/* DMA ring sizing bounds (bytes). */
#define MOTU424_MAX_BUFFER_BYTES	(512 * 1024)
#define MOTU424_MIN_PERIOD_BYTES	1024
#define MOTU424_MAX_PERIODS		32

struct motu424;

/* Per-direction stream state. */
struct motu424_stream {
	struct snd_pcm_substream *substream;	/* NULL when closed        */
	bool running;
	unsigned int period_bytes;
	unsigned int buffer_bytes;
};

/* Driver instance (lives in snd_card->private_data). */
struct motu424 {
	struct snd_card *card;
	struct pci_dev *pci;

	void __iomem *mmio;		/* mapped BAR0 */
	int irq;

	spinlock_t lock;		/* guards register access + stream state */

	struct snd_pcm *pcm;
	struct motu424_stream playback;
	struct motu424_stream capture;

	unsigned int rate;		/* active sample rate (Hz) */
	unsigned int channels;		/* active channel count per frame */

	char model[32];			/* human-readable model string */
};

#define MOTU424_STREAM_IS_PLAYBACK(s) \
	((s)->stream == SNDRV_PCM_STREAM_PLAYBACK)

/* --- motu424_hw.c : the only file that touches real register semantics --- */
int  motu424_hw_init(struct motu424 *chip);
void motu424_hw_shutdown(struct motu424 *chip);
int  motu424_hw_set_rate(struct motu424 *chip, unsigned int rate);
int  motu424_hw_stream_prepare(struct motu424 *chip,
			       struct snd_pcm_substream *substream);
void motu424_hw_stream_start(struct motu424 *chip, bool playback);
void motu424_hw_stream_stop(struct motu424 *chip, bool playback);
snd_pcm_uframes_t motu424_hw_stream_pointer(struct motu424 *chip, bool playback);
u32  motu424_hw_irq_ack(struct motu424 *chip);	/* returns MOTU424_IRQ_* bits */

/* --- motu424_pcm.c --- */
int motu424_pcm_create(struct motu424 *chip);

#endif /* MOTU424_H */
