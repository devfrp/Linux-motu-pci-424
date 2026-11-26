// SPDX-License-Identifier: GPL-2.0-or-later
/*
 * motu424_alsa.c - the C side of the Rust motu424 driver: the ALSA card,
 * its PCM device and the module parameters.
 *
 * The kernel has no Rust bindings for ALSA, and Rust module parameters
 * are integers only, so these stay in C. Everything that touches the card
 * is Rust (motu424_main.rs, hw.rs): the PCM callbacks below only forward to
 * the motu424_rs_pcm_*() functions in alsa.rs. No register offsets appear
 * here.
 *
 * The ALSA buffer is the host ring itself: a coherent buffer the DSP reads
 * by bus-master DMA, MOTU424_RING_PERIODS periods of period_bytes each.
 * Capture is not exposed yet: where the DSP writes recorded audio on the
 * host side is still open (docs/transport.md).
 */
#include <linux/module.h>
#include <linux/pci.h>
#include <sound/core.h>
#include <sound/initval.h>
#include <sound/pcm.h>
#include <sound/pcm_params.h>

#define MOTU424_DRIVER_NAME	"motu424"
#define MOTU424_RING_BYTES	0x80000u	/* hw.rs RING_BYTES   */
#define MOTU424_RING_PERIODS	8		/* hw.rs RING_PERIODS */
#define MOTU424_RATE		48000

/* --- module parameters --- */

static int index[SNDRV_CARDS] = SNDRV_DEFAULT_IDX;
static char *id[SNDRV_CARDS] = SNDRV_DEFAULT_STR;
static bool enable[SNDRV_CARDS] = SNDRV_DEFAULT_ENABLE_PNP;

module_param_array(index, int, NULL, 0444);
MODULE_PARM_DESC(index, "Index value for the MOTU PCI-424 card.");
module_param_array(id, charp, NULL, 0444);
MODULE_PARM_DESC(id, "ID string for the MOTU PCI-424 card.");
module_param_array(enable, bool, NULL, 0444);
MODULE_PARM_DESC(enable, "Enable the MOTU PCI-424 card.");

static unsigned int frame_bytes;
module_param(frame_bytes, uint, 0444);
MODULE_PARM_DESC(frame_bytes,
		 "Host-ring bytes per frame (96 measured at 48 kHz, see docs/transport.md); 0 = streaming disabled");

static unsigned int period_bytes = 0x1800;
module_param(period_bytes, uint, 0444);
MODULE_PARM_DESC(period_bytes,
		 "Host-ring bytes the DSP fetches per interrupt (0x1800 measured at 48 kHz with irq_frames=0x40)");

static unsigned int irq_frames = 0x40;
module_param(irq_frames, uint, 0444);
MODULE_PARM_DESC(irq_frames,
		 "Frames per interrupt written to +0x60 (0x40 by default; diagnostic, see docs/transport.md)");

static bool configure = true;
module_param(configure, bool, 0444);
MODULE_PARM_DESC(configure,
		 "Write the vendor's 48 kHz parameter group at load and the channel map at stream start (default Y)");

static bool idle_stream;
module_param(idle_stream, bool, 0444);
MODULE_PARM_DESC(idle_stream,
		 "Start the vendor's idle zero-channel stream at load and whenever no PCM stream is open (default N; diagnostic)");

MODULE_FIRMWARE("motu424/pci424-serial-container.bin");
MODULE_FIRMWARE("motu424/pci424-program.bin");

/* Read by alsa.rs as struct RawParams: keep the two in step. */
struct motu424_params {
	unsigned int frame_bytes;
	unsigned int period_bytes;
	unsigned int irq_frames;
	bool configure;
	bool idle_stream;
};

void motu424_alsa_params(struct motu424_params *p);
int motu424_alsa_slot(void);
int motu424_alsa_create(struct device *dev, void *chip, const char *model,
			void **out);
void motu424_alsa_disconnect(void *alsa);
void motu424_alsa_period_elapsed(void *alsa);

/* Implemented in alsa.rs. */
int motu424_rs_pcm_open(void *chip, unsigned int *channels,
			unsigned int *period_bytes);
void motu424_rs_pcm_close(void *chip);
void motu424_rs_pcm_prepare(void *chip);
void motu424_rs_pcm_start(void *chip, u64 ring);
void motu424_rs_pcm_stop(void *chip);
void motu424_rs_pcm_sync_stop(void *chip);
unsigned long motu424_rs_pcm_pointer(void *chip, u64 ring);

void motu424_alsa_params(struct motu424_params *p)
{
	p->frame_bytes = frame_bytes;
	p->period_bytes = period_bytes;
	p->irq_frames = irq_frames;
	p->configure = configure;
	p->idle_stream = idle_stream;
}

/* Index into the card parameter arrays; advanced per probed card. */
static int dev;

int motu424_alsa_slot(void)
{
	if (dev >= SNDRV_CARDS)
		return -ENODEV;
	if (!enable[dev]) {
		dev++;
		return -ENOENT;
	}
	return 0;
}

/* --- the card (lives in snd_card->private_data) --- */

struct motu424_alsa {
	struct snd_card *card;
	void *chip;				/* the Rust Chip */
	struct snd_pcm_substream *substream;	/* NULL when closed */
	bool registered;
};

/*
 * The frame layout on the ring is not confirmed (docs/transport.md), so the
 * format is raw 32-bit words, frame_bytes / 4 per frame; the channel count
 * and the period size are pinned in open().
 */
static const struct snd_pcm_hardware motu424_pcm_hw = {
	.info = SNDRV_PCM_INFO_MMAP |
		SNDRV_PCM_INFO_MMAP_VALID |
		SNDRV_PCM_INFO_INTERLEAVED |
		SNDRV_PCM_INFO_BLOCK_TRANSFER,
	.formats = SNDRV_PCM_FMTBIT_S32_LE,
	.rates = SNDRV_PCM_RATE_48000,
	.rate_min = MOTU424_RATE,
	.rate_max = MOTU424_RATE,
	.buffer_bytes_max = MOTU424_RING_BYTES,
	.period_bytes_min = 256,
	.period_bytes_max = MOTU424_RING_BYTES / MOTU424_RING_PERIODS,
	.periods_min = MOTU424_RING_PERIODS,
	.periods_max = MOTU424_RING_PERIODS,
};

static int motu424_pcm_open(struct snd_pcm_substream *substream)
{
	struct motu424_alsa *a = snd_pcm_substream_chip(substream);
	struct snd_pcm_runtime *runtime = substream->runtime;
	unsigned int channels, pbytes;
	int err;

	err = motu424_rs_pcm_open(a->chip, &channels, &pbytes);
	if (err < 0)
		return err;

	runtime->hw = motu424_pcm_hw;
	runtime->hw.channels_min = channels;
	runtime->hw.channels_max = channels;

	/*
	 * The DSP fetches period_bytes per interrupt and wraps after
	 * MOTU424_RING_PERIODS periods: the buffer must be exactly that.
	 */
	runtime->hw.period_bytes_min = pbytes;
	runtime->hw.period_bytes_max = pbytes;
	err = snd_pcm_hw_constraint_minmax(runtime,
					   SNDRV_PCM_HW_PARAM_PERIOD_BYTES,
					   pbytes, pbytes);
	if (err < 0)
		return err;

	WRITE_ONCE(a->substream, substream);
	return 0;
}

static int motu424_pcm_close(struct snd_pcm_substream *substream)
{
	struct motu424_alsa *a = snd_pcm_substream_chip(substream);

	WRITE_ONCE(a->substream, NULL);
	motu424_rs_pcm_close(a->chip);
	return 0;
}

static int motu424_pcm_prepare(struct snd_pcm_substream *substream)
{
	struct motu424_alsa *a = snd_pcm_substream_chip(substream);

	motu424_rs_pcm_prepare(a->chip);

	/* Start the DSP on silence; ALSA fills the buffer before START. */
	return snd_pcm_format_set_silence(substream->runtime->format,
					  substream->runtime->dma_area,
					  substream->runtime->buffer_size *
					  substream->runtime->channels);
}

static int motu424_pcm_trigger(struct snd_pcm_substream *substream, int cmd)
{
	struct motu424_alsa *a = snd_pcm_substream_chip(substream);

	switch (cmd) {
	case SNDRV_PCM_TRIGGER_START:
		motu424_rs_pcm_start(a->chip, substream->runtime->dma_addr);
		return 0;
	case SNDRV_PCM_TRIGGER_STOP:
		motu424_rs_pcm_stop(a->chip);
		return 0;
	default:
		return -EINVAL;
	}
}

static int motu424_pcm_sync_stop(struct snd_pcm_substream *substream)
{
	struct motu424_alsa *a = snd_pcm_substream_chip(substream);

	motu424_rs_pcm_sync_stop(a->chip);
	return 0;
}

static snd_pcm_uframes_t motu424_pcm_pointer(struct snd_pcm_substream *substream)
{
	struct motu424_alsa *a = snd_pcm_substream_chip(substream);

	return motu424_rs_pcm_pointer(a->chip, substream->runtime->dma_addr);
}

static const struct snd_pcm_ops motu424_pcm_ops = {
	.open = motu424_pcm_open,
	.close = motu424_pcm_close,
	.prepare = motu424_pcm_prepare,
	.trigger = motu424_pcm_trigger,
	.sync_stop = motu424_pcm_sync_stop,
	.pointer = motu424_pcm_pointer,
};

/*
 * Create and register the card for @chip. The card is devm-managed: it is
 * freed with the device, after the Rust driver data, which disconnects it
 * first (motu424_alsa_disconnect()).
 */
int motu424_alsa_create(struct device *device, void *chip, const char *model,
			void **out)
{
	struct pci_dev *pci = to_pci_dev(device);
	struct motu424_alsa *a;
	struct snd_card *card;
	struct snd_pcm *pcm;
	int irq = pci_irq_vector(pci, 0);
	int err;

	err = snd_devm_card_new(device, index[dev], id[dev], THIS_MODULE,
				sizeof(*a), &card);
	if (err < 0)
		return err;

	a = card->private_data;
	a->card = card;
	a->chip = chip;
	if (irq >= 0)
		card->sync_irq = irq;

	err = snd_pcm_new(card, MOTU424_DRIVER_NAME, 0, 1, 0, &pcm);
	if (err < 0)
		return err;
	pcm->private_data = a;
	strscpy(pcm->name, model, sizeof(pcm->name));
	snd_pcm_set_ops(pcm, SNDRV_PCM_STREAM_PLAYBACK, &motu424_pcm_ops);

	/* The host ring: coherent, 32-bit addressable, read by the DSP. */
	snd_pcm_set_managed_buffer_all(pcm, SNDRV_DMA_TYPE_DEV, device,
				       MOTU424_RING_BYTES, MOTU424_RING_BYTES);

	strscpy(card->driver, MOTU424_DRIVER_NAME, sizeof(card->driver));
	strscpy(card->shortname, model, sizeof(card->shortname));
	snprintf(card->longname, sizeof(card->longname), "%s at %s, irq %d",
		 model, pci_name(pci), irq);

	err = snd_card_register(card);
	if (err < 0)
		return err;
	a->registered = true;

	dev_info(device, "%s registered as ALSA card %d\n", model,
		 card->number);
	dev++;
	*out = a;
	return 0;
}

/* Disconnect and wait for every open file: no PCM callback runs after this. */
void motu424_alsa_disconnect(void *alsa)
{
	struct motu424_alsa *a = alsa;

	if (a->registered)
		snd_card_disconnect_sync(a->card);
}

/* From the Rust IRQ handler, while playback runs. */
void motu424_alsa_period_elapsed(void *alsa)
{
	struct motu424_alsa *a = alsa;
	struct snd_pcm_substream *substream = READ_ONCE(a->substream);

	if (substream)
		snd_pcm_period_elapsed(substream);
}
