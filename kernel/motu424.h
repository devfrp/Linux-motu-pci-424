/* SPDX-License-Identifier: GPL-2.0-or-later */
/*
 * motu424 - Linux ALSA driver for the MOTU PCI-324 / PCI-424 audio card.
 *
 * Shared definitions: hardware model, driver state structures and
 * inter-module prototypes.
 *
 * ---------------------------------------------------------------------------
 * IMPORTANT - REVERSE ENGINEERING STATUS
 * ---------------------------------------------------------------------------
 * The MOTU PCI-324/424 is undocumented hardware. The model below was recovered
 * by static RE of the vendor Windows driver MOTUAW.sys (docs/register-map.md,
 * docs/transport.md, docs/vendor-driver-map.md). Structural facts (windowed
 * address space, port BAR, PIO-aperture transport, audio-register offsets,
 * IRQ ack protocol) are CONFIRMED from disassembly; anything tagged
 * "TODO: verify" is still a hypothesis, and the card-reported runtime
 * addresses (audio base / ack address) cannot be known without a card.
 *
 * All accesses are funnelled through motu424_hw.c so that hardware truth
 * lives in a single file.
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
 * Windowed card-address space - CONFIRMED (accessor VAs 0x29110/0x29160)
 * --------------------------------------------------------------------------
 * The card is not a flat register file. A 32-bit "card address" is dispatched
 * onto one of two MMIO BAR windows:
 *
 *   if ((addr & 0xff800000) == 0x01800000)   // window A (8 MB)
 *       mmio = win_a + (addr & 0x007fffff);
 *   else                                      // window B (4 MB)
 *       mmio = win_b + (addr & 0x003fffff);
 *
 * Windows A and B alias the same underlying space (0x018c0000 and 0x000c0000
 * both name bank 0); A is used for 8-dword block writes, B for ordinary
 * accesses and the audio aperture. All accesses are little-endian 32-bit.
 */
#define MOTU424_WINA_TAG	0x01800000u
#define MOTU424_WINA_TAG_MASK	0xff800000u
#define MOTU424_WINA_MASK	0x007fffffu
#define MOTU424_WINB_MASK	0x003fffffu
#define MOTU424_WINA_LEN	0x00800000u	/* 8 MB */
#define MOTU424_WINB_LEN	0x00400000u	/* 4 MB */

/* --------------------------------------------------------------------------
 * I/O-port BAR - CONFIRMED (a few dwords of bridge/GPIO control)
 * --------------------------------------------------------------------------
 * +0x0 R : status - bit 1 = IRQ pending (ISR 0x2bae0)
 * +0x0 W : control - value 4 (bit 2) = IRQ/stream enable (0x298f3)
 * +0x4 W : strobe / commit (write 1; 0x29906, 0x2c3dc)
 * +0x8 W : init value at bring-up (0x2b6c8; observed 0)
 * Start sequence (method 0x298e0): WRITE(+0x0, 4) then WRITE(+0x4, 1).
 */
#define MOTU424_PORT_STATUS	0x0
#define MOTU424_PORT_IRQ_PENDING	BIT(1)
#define MOTU424_PORT_CTRL	0x0
#define MOTU424_PORT_CTRL_ENABLE	BIT(2)
#define MOTU424_PORT_STROBE	0x4
#define MOTU424_PORT_INIT	0x8

/* --------------------------------------------------------------------------
 * Bank register file - INFERRED (window-B card addresses)
 * --------------------------------------------------------------------------
 * Two banks at 0xc0000 / 0x100000 (stride 0x40000); per-bank ctrl/status at
 * bank+0x24 (bit 6 is a r/w control flag); bank+0x08..+0x24 programmed as an
 * 8-dword unit at init via window A (WriteBlock8 0x291a0).
 */
#define MOTU424_BANK0		0x000c0000u
#define MOTU424_BANK1		0x00100000u
#define MOTU424_BANK_STRIDE	0x00040000u
#define MOTU424_BANK_CTRL	0x24u

/* --------------------------------------------------------------------------
 * Audio register block - CONFIRMED offsets, runtime base
 * --------------------------------------------------------------------------
 * The audio register base is a CARD-REPORTED card address (vendor keeps it at
 * devext+0x98, read from the card at init; likewise the IRQ-ack address at
 * devext+0x88 and the CueMix mixer base at devext+0x9c). These are runtime
 * values: they cannot be recovered statically and must be dumped on real
 * hardware (tools/motu424-probe). Until then they are 0 here and streaming is
 * refused; they can be injected via module parameters for bring-up
 * (motu424.audio_base= / ack_addr= / mix_base=).
 */
#define MOTU424_AREG_ENABLE	0x54u	/* W: 1 = stream/DMA enable          */
#define MOTU424_AREG_INCR	0x60u	/* W: period increment 0x10<<(2*fam) */
#define MOTU424_AREG_PARAM	0x64u	/* W: rate param (encoding TODO)     */
#define MOTU424_AREG_POS	0x128u	/* R->W0: position counter           */
#define MOTU424_AREG_NUM	0x12cu	/* R->W0: numerator                  */
#define MOTU424_AREG_DIV	0x130u	/* R->W0: divisor                    */

#define MOTU424_ACK_MAGIC	0x10u	/* IRQ ack: write 0x10 to ack_addr   */
#define MOTU424_PERIOD_UNIT	0x800u	/* vendor period accumulator bound   */

/* --------------------------------------------------------------------------
 * PIO aperture ring - CONFIRMED shape (arm routine fn 0x2c150)
 * --------------------------------------------------------------------------
 * Audio is NOT host bus-master DMA. The vendor pushes samples into a window-B
 * aperture with WRITE_REGISTER_BUFFER_ULONG (<=64 KB bursts, dword units) and
 * runs a software readHead/writeHead/len ring against a hardware "dmaPoint".
 *
 * The aperture base + length are RUNTIME values read from the audio sub-object
 * (play: base [A+0x18], len [A+0x1c]; capture: base [A+0x28], len [A+0x2c]) -
 * card-reported, like the audio base itself, so they cannot be known without a
 * card. Before streaming a direction the driver programs a page/segment select:
 * WRITE_PORT(port+0x8, apertureBase >> 22) - the window-B 4 MB page index.
 */
#define MOTU424_APERTURE_PAGE_SHIFT	22	/* base>>22 -> port+0x8 page reg */
#define MOTU424_RING_DWORDS	0x4000u		/* 64 KB burst, power of two */
#define MOTU424_RING_BYTES	(MOTU424_RING_DWORDS * 4)

/*
 * Software IRQ bits: driver-internal contract between motu424_hw_irq_ack()
 * and the interrupt handler in motu424_main.c (NOT hardware register bits).
 */
#define MOTU424_IRQ_PLAY	BIT(0)	/* playback period elapsed */
#define MOTU424_IRQ_REC		BIT(1)	/* capture  period elapsed */

/* --------------------------------------------------------------------------
 * Audio format constants
 * --------------------------------------------------------------------------
 * The card's native wire format is 24-bit samples packed 3 bytes per channel
 * per frame ("event"). We expose S24_3LE; endianness must be confirmed on a
 * card. The fixed frame's channel count shrinks as the rate family rises
 * (1x/2x/4x).
 */
#define MOTU424_BYTES_PER_SAMPLE	3
#define MOTU424_MAX_CHANNELS		24	/* e.g. one 2408 mk3 bank */
#define MOTU424_MIN_CHANNELS		2

/*
 * Host buffer sizing bounds (bytes). The period must fit the aperture ring
 * with double-buffering, hence the period cap at half the ring.
 */
#define MOTU424_MAX_BUFFER_BYTES	(512 * 1024)
#define MOTU424_MIN_PERIOD_BYTES	1024
#define MOTU424_MAX_PERIOD_BYTES	(MOTU424_RING_BYTES / 2)
#define MOTU424_MAX_PERIODS		32

struct motu424;

/* Per-direction stream state. */
struct motu424_stream {
	struct snd_pcm_substream *substream;	/* NULL when closed        */
	bool running;
	unsigned int period_bytes;
	unsigned int buffer_bytes;
	unsigned int buffer_frames;
	/* PIO ring bookkeeping (all owned by motu424_hw.c under chip->lock) */
	unsigned int buf_pos;		/* next host-buffer byte to copy     */
	unsigned int ring_pos;		/* aperture head, in dwords          */
	unsigned int pos_frames;	/* ALSA pointer, frames in buffer    */
	unsigned int period_acc;	/* frames since last period elapsed  */
};

/*
 * One mapped PCI BAR, filled generically by motu424_main.c; motu424_hw.c
 * decides which BAR is which hardware window.
 */
struct motu424_bar {
	void __iomem *ptr;		/* NULL if absent/unmapped */
	resource_size_t len;
	unsigned long flags;		/* IORESOURCE_MEM / _IO    */
};

/* Driver instance (lives in snd_card->private_data). */
struct motu424 {
	struct snd_card *card;
	struct pci_dev *pci;

	struct motu424_bar bars[PCI_STD_NUM_BARS];	/* from main.c   */
	void __iomem *win_a;		/* 8 MB MMIO window (may be NULL) */
	void __iomem *win_b;		/* 4 MB MMIO window               */
	void __iomem *port;		/* small I/O-port bridge BAR      */
	int irq;

	spinlock_t lock;		/* guards register access + stream state */

	/* Card-reported runtime card-addresses (0 = not yet discovered). */
	u32 audio_base;			/* audio register block base    */
	u32 ack_addr;			/* IRQ ack register             */
	u32 mix_base;			/* CueMix coefficient region    */
	u32 aperture[2];		/* [0]=playback [1]=capture base */

	struct snd_pcm *pcm;
	struct motu424_stream playback;
	struct motu424_stream capture;

	unsigned int rate;		/* active sample rate (Hz)        */
	unsigned int family;		/* 0/1/2 = 1x/2x/4x               */
	unsigned int period_incr;	/* samples per IRQ, 0x10<<(2*fam) */
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
