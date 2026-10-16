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
 * The MOTU PCI-324/424 is undocumented hardware. The model below comes from
 * static RE of the vendor Windows driver MOTUAW.sys and of the card's own DSP
 * program, checked on a real PCI-424 (issue #2; docs/register-map.md,
 * docs/transport.md, docs/fpga-upload.md). The board is a TI TMS320C6412 DSP
 * (its PCI port is the card's PCI interface) plus an FPGA with no config
 * PROM, so the host loads both on every start. The DSP is the PCI bus
 * master: it moves audio to and from a host ring by itself. Anything tagged
 * "TODO: verify" is still a hypothesis.
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
 * A 32-bit "card address" is a TMS320C6412 address, dispatched onto one of
 * two MMIO BAR windows:
 *
 *   if ((addr & 0xff800000) == 0x01800000)   // window A (8 MB)
 *       mmio = win_a + (addr & 0x007fffff);  // DSP peripheral registers
 *   else                                      // window B (4 MB)
 *       mmio = win_b + (addr & 0x003fffff);  // DSP memory page set by DSPP
 *
 * With DSPP = 0, window B offsets 0..0x3ffff are the DSP's internal RAM; the
 * rest of that page is reserved on the C6412 and is never touched. All
 * accesses are little-endian 32-bit.
 */
#define MOTU424_WINA_TAG	0x01800000u
#define MOTU424_WINA_TAG_MASK	0xff800000u
#define MOTU424_WINA_MASK	0x007fffffu
#define MOTU424_WINB_MASK	0x003fffffu
#define MOTU424_WINA_LEN	0x00800000u	/* 8 MB */
#define MOTU424_WINB_LEN	0x00400000u	/* 4 MB */
#define MOTU424_RAM_LEN		0x00040000u	/* C6412 internal RAM, DSPP = 0 */

/*
 * The only DSP peripheral registers the driver writes (SPRS219J / SPRU581C /
 * SPRU234C). Every other window-A write is refused by motu424_wr32(); that
 * keeps the DSP EEPROM controller (card 0x01c20000, which holds the card's
 * PCI identity) out of reach by construction.
 */
#define MOTU424_EMIF_CECTL0	0x01800008u	/* EMIF CE0 space control      */
#define MOTU424_GPIO_GPEN	0x01b00000u	/* GPIO enable                 */
#define MOTU424_GPIO_GPDIR	0x01b00004u	/* GPIO direction              */
#define MOTU424_GPIO_GPVAL	0x01b00008u	/* GPIO value                  */
#define MOTU424_PCI_RSTSRC	0x01c00000u	/* PCI reset source / IRQ ack  */

#define MOTU424_CECTL0_INIT	0x10914221u	/* 32-bit async CE0, vendor value */
#define MOTU424_RSTSRC_INTRST	0x10u		/* bit 4: clear the DSP's INTREQ  */

/* FPGA serial-configuration pins on the DSP GPIO (vendor fn 0x251e0). */
#define MOTU424_GP_DATA0	BIT(5)
#define MOTU424_GP_NCONFIG	BIT(6)
#define MOTU424_GP_DCLK		BIT(7)

/* --------------------------------------------------------------------------
 * I/O-port BAR = TMS320C6412 Host Port Interface (HPI) registers, per the
 * public TI docs (SPRU581C sec. "Host Port Interface", SPRS219J):
 *
 *   +0x0  HSR  (Host Status Register)          - bit 2 INTAM masks PINTA#
 *         when SET; the host must clear it (write 0) to unmask/enable the
 *         card's interrupt to the host. Bit 1 INTAVAL = interrupt pending.
 *   +0x4  HDCR (Host-to-DSP Control Register)  - bit 0 WARMRESET resets the
 *         DSP core; bit 1 DSPINT releases a reset core to execute from
 *         address 0 once the program has been loaded there.
 *   +0x8  DSPP (DSP Page Register)             - selects the window-B page;
 *         always 0 here (internal RAM).
 */
#define MOTU424_PORT_HSR	0x0
#define MOTU424_PORT_IRQ_PENDING	BIT(1)
#define MOTU424_PORT_HSR_INTAM	BIT(2)	/* 1 = mask PINTA#, 0 = unmask */
#define MOTU424_PORT_HDCR	0x4
#define MOTU424_PORT_HDCR_WARMRESET	BIT(0)
#define MOTU424_PORT_HDCR_DSPINT	BIT(1)
#define MOTU424_PORT_DSPP	0x8

/* --------------------------------------------------------------------------
 * Host-loaded firmware - CONFIRMED on hardware (docs/fpga-upload.md)
 * --------------------------------------------------------------------------
 * Both images are MOTU's and are not shipped: the user extracts them from
 * their own MOTUAW.sys with tools/re/extract-firmware.py and installs them
 * under /lib/firmware/motu424/. Lengths and SHA-256 are checked before any
 * byte reaches the card.
 */
#define MOTU424_FW_FPGA		"motu424/pci424-serial-container.bin"
#define MOTU424_FW_FPGA_LEN	37177
#define MOTU424_FW_PROG		"motu424/pci424-program.bin"
#define MOTU424_FW_PROG_LEN	28272

/* The DSP program publishes audio_base in the last dword of internal RAM. */
#define MOTU424_MAILBOX		0x0003fffcu

/* --------------------------------------------------------------------------
 * Audio block - the DSP program's global data, published at audio_base
 * --------------------------------------------------------------------------
 * audio_base is read from the mailbox after the program starts (0x6fac with
 * the current image: B14 + 0x13c). Offsets CONFIRMED against the DSP code
 * and a Windows-configured dump (issue #2, 2026-10-15).
 */
#define MOTU424_AREG_MIXBASE	0x00u	/* R: CueMix coefficient block (45 dwords) */
#define MOTU424_AREG_DMAPOINT	0x2cu	/* R: DSP's current host bus address    */
#define MOTU424_AREG_ENABLE_INIT 0x50u	/* W 1: vendor init, end of bring-up    */
#define MOTU424_AREG_ENABLE	0x54u	/* W: 1 = start, 0 = stop the stream     */
#define MOTU424_AREG_RING_BASE	0x5cu	/* W: host ring bus address              */
#define MOTU424_AREG_IRQ_FRAMES	0x60u	/* W: frames per interrupt               */
#define MOTU424_AREG_RING_SIZE	0x8cu	/* W: host ring size code (bytes)        */
#define MOTU424_AREG_STOP_ACK	0xf4u	/* W 0 before stop; DSP sets !0 when done */

#define MOTU424_MIX_DWORDS	45

/*
 * Software IRQ bits: driver-internal contract between motu424_hw_irq_ack()
 * and the interrupt handler in motu424_main.c (NOT hardware register bits).
 */
#define MOTU424_IRQ_OURS	BIT(0)	/* the card interrupted; acked       */
#define MOTU424_IRQ_PLAY	BIT(1)	/* ... while playback was running    */

/* --------------------------------------------------------------------------
 * Host ring - CONFIRMED shape, format OPEN (docs/transport.md)
 * --------------------------------------------------------------------------
 * One coherent host buffer, 512 KB as on Windows (+0x8c size code 0x80000).
 * The DSP fetches from it as PCI master and raises one interrupt per
 * IRQ_FRAMES frames (0x40 at 1x). Only the 1x / 48 kHz state the program
 * boots into is used: rate changes go through the relock sequence, which is
 * not implemented.
 *
 * The bytes per frame on the ring are NOT known yet (the DSP side hints at
 * 48), so the frame size is a module parameter and streaming stays off until
 * it is set; it must divide the ring size. Samples are exposed as 32-bit
 * words (S32_LE), frame_bytes / 4 of them per frame. TODO: verify both with a
 * mid-stream dump of the ring.
 */
#define MOTU424_RING_BYTES	0x80000u
#define MOTU424_IRQ_FRAMES_1X	0x40u
#define MOTU424_RATE		48000
#define MOTU424_MAX_PERIODS	1024

struct motu424;

/* Per-direction stream state. */
struct motu424_stream {
	struct snd_pcm_substream *substream;	/* NULL when closed        */
	bool running;
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
	void __iomem *win_a;		/* 8 MB MMIO window               */
	void __iomem *win_b;		/* 4 MB MMIO window               */
	void __iomem *port;		/* HPI register BAR (16 bytes)    */
	int irq;

	spinlock_t lock;		/* guards register access + stream state */

	/* Published by the DSP program after the load (0 = not running). */
	u32 audio_base;			/* audio block (DSP global data) */
	u32 mix_base;			/* CueMix coefficient block      */

	unsigned int frame_bytes;	/* host-ring bytes per frame, 0 = unknown */

	struct snd_pcm *pcm;
	struct motu424_stream playback;

	char model[32];			/* human-readable model string */
};

/* --- motu424_hw.c : the only file that touches real register semantics --- */
int  motu424_hw_init(struct motu424 *chip);
void motu424_hw_shutdown(struct motu424 *chip);
void motu424_hw_stream_start(struct motu424 *chip, dma_addr_t ring);
void motu424_hw_stream_stop(struct motu424 *chip);
int  motu424_hw_stream_sync_stop(struct motu424 *chip);
snd_pcm_uframes_t motu424_hw_stream_pointer(struct motu424 *chip, dma_addr_t ring);
u32  motu424_hw_irq_ack(struct motu424 *chip);	/* returns MOTU424_IRQ_* bits */

/* --- motu424_pcm.c --- */
int motu424_pcm_create(struct motu424 *chip);

#endif /* MOTU424_H */
