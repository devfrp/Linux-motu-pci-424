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
 * Assign the mapped BARs to their hardware roles. The vendor driver gets the
 * assignment from the bus; we derive it from the BAR types/sizes, which is
 * hardware-determined: the I/O-port BAR is the bridge control, the 8 MB MMIO
 * BAR is window A, the 4 MB one window B. With a single MMIO BAR everything
 * routes through it as window B (A aliases B).
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
			else if (!chip->win_b)
				chip->win_b = b->ptr;
		}
	}
	/* A lone large MMIO BAR serves as window B too. */
	if (!chip->win_b && chip->win_a) {
		chip->win_b = chip->win_a;
		chip->win_a = NULL;
	}
	if (!chip->win_b) {
		dev_err(&chip->pci->dev, "no usable MMIO BAR found\n");
		return -ENODEV;
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
	if (chip->port)
		iowrite32(0, chip->port + MOTU424_PORT_CTRL);
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
 * TODO (RT latency): this runs a multi-KB memcpy_toio under chip->lock with
 * IRQs off. On the PREEMPT_RT target that is a latency spike; once the real
 * dmaPoint register is known, move the copy out of the IRQ-off region (e.g.
 * a threaded handler or a bounded per-tick burst).
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
		/*
		 * TODO: verify - the vendor's stop sequence is not yet
		 * recovered; dropping the port enable bit is the inverse of
		 * the start sequence.
		 */
		if (chip->port)
			iowrite32(0, chip->port + MOTU424_PORT_CTRL);
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

	if (!chip->port || !chip->win_b)
		return 0;

	if (!(ioread32(chip->port + MOTU424_PORT_STATUS) &
	      MOTU424_PORT_IRQ_PENDING))
		return 0;	/* not ours (shared line) */

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
