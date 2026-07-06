// SPDX-License-Identifier: GPL-2.0-or-later
/*
 * motu424_pcm.c - ALSA PCM interface (playback + capture).
 *
 * Standard PCM plumbing: it wires ALSA's callbacks to the hardware layer in
 * motu424_hw.c and imposes the card's fixed-frame constraints. No register
 * offsets appear here.
 */
#include <linux/module.h>
#include <sound/core.h>
#include <sound/pcm.h>
#include <sound/pcm_params.h>

#include "motu424.h"

/*
 * The card streams a fixed number of 24-bit channels packed 3 bytes each. We
 * advertise S24_3LE; the hardware layer is responsible for any endianness
 * repacking once the wire format is confirmed.
 */
static const struct snd_pcm_hardware motu424_pcm_hw = {
	.info = SNDRV_PCM_INFO_MMAP |
		SNDRV_PCM_INFO_MMAP_VALID |
		SNDRV_PCM_INFO_INTERLEAVED |
		SNDRV_PCM_INFO_BLOCK_TRANSFER,
	.formats = SNDRV_PCM_FMTBIT_S24_3LE,
	.rates = SNDRV_PCM_RATE_44100 | SNDRV_PCM_RATE_48000 |
		 SNDRV_PCM_RATE_88200 | SNDRV_PCM_RATE_96000 |
		 SNDRV_PCM_RATE_176400 | SNDRV_PCM_RATE_192000,
	.rate_min = 44100,
	.rate_max = 192000,
	.channels_min = MOTU424_MIN_CHANNELS,
	.channels_max = MOTU424_MAX_CHANNELS,
	.buffer_bytes_max = MOTU424_MAX_BUFFER_BYTES,
	.period_bytes_min = MOTU424_MIN_PERIOD_BYTES,
	.period_bytes_max = MOTU424_MAX_PERIOD_BYTES,
	.periods_min = 2,
	.periods_max = MOTU424_MAX_PERIODS,
};

static struct motu424_stream *motu424_substream_stream(struct motu424 *chip,
						       struct snd_pcm_substream *substream)
{
	return MOTU424_STREAM_IS_PLAYBACK(substream) ? &chip->playback
						     : &chip->capture;
}

static int motu424_pcm_open(struct snd_pcm_substream *substream)
{
	struct motu424 *chip = snd_pcm_substream_chip(substream);
	struct snd_pcm_runtime *runtime = substream->runtime;

	runtime->hw = motu424_pcm_hw;

	/*
	 * One frame must be a whole number of 3-byte samples across channels;
	 * keep the period a multiple of the frame so DMA math stays exact.
	 */
	snd_pcm_hw_constraint_step(runtime, 0,
				   SNDRV_PCM_HW_PARAM_PERIOD_BYTES,
				   MOTU424_BYTES_PER_SAMPLE * MOTU424_MIN_CHANNELS);

	motu424_substream_stream(chip, substream)->substream = substream;
	return 0;
}

static int motu424_pcm_close(struct snd_pcm_substream *substream)
{
	struct motu424 *chip = snd_pcm_substream_chip(substream);

	motu424_substream_stream(chip, substream)->substream = NULL;
	return 0;
}

static int motu424_pcm_prepare(struct snd_pcm_substream *substream)
{
	struct motu424 *chip = snd_pcm_substream_chip(substream);
	struct snd_pcm_runtime *runtime = substream->runtime;
	int err;

	err = motu424_hw_set_rate(chip, runtime->rate);
	if (err < 0)
		return err;

	chip->channels = runtime->channels;
	return motu424_hw_stream_prepare(chip, substream);
}

static int motu424_pcm_trigger(struct snd_pcm_substream *substream, int cmd)
{
	struct motu424 *chip = snd_pcm_substream_chip(substream);
	bool playback = MOTU424_STREAM_IS_PLAYBACK(substream);

	switch (cmd) {
	case SNDRV_PCM_TRIGGER_START:
	case SNDRV_PCM_TRIGGER_RESUME:
	case SNDRV_PCM_TRIGGER_PAUSE_RELEASE:
		motu424_hw_stream_start(chip, playback);
		return 0;
	case SNDRV_PCM_TRIGGER_STOP:
	case SNDRV_PCM_TRIGGER_SUSPEND:
	case SNDRV_PCM_TRIGGER_PAUSE_PUSH:
		motu424_hw_stream_stop(chip, playback);
		return 0;
	default:
		return -EINVAL;
	}
}

static snd_pcm_uframes_t motu424_pcm_pointer(struct snd_pcm_substream *substream)
{
	struct motu424 *chip = snd_pcm_substream_chip(substream);

	return motu424_hw_stream_pointer(chip,
					 MOTU424_STREAM_IS_PLAYBACK(substream));
}

static const struct snd_pcm_ops motu424_pcm_ops = {
	.open = motu424_pcm_open,
	.close = motu424_pcm_close,
	.prepare = motu424_pcm_prepare,
	.trigger = motu424_pcm_trigger,
	.pointer = motu424_pcm_pointer,
};

int motu424_pcm_create(struct motu424 *chip)
{
	struct snd_pcm *pcm;
	int err;

	err = snd_pcm_new(chip->card, MOTU424_DRIVER_NAME, 0, 1, 1, &pcm);
	if (err < 0)
		return err;

	pcm->private_data = chip;
	strscpy(pcm->name, chip->model, sizeof(pcm->name));
	chip->pcm = pcm;

	snd_pcm_set_ops(pcm, SNDRV_PCM_STREAM_PLAYBACK, &motu424_pcm_ops);
	snd_pcm_set_ops(pcm, SNDRV_PCM_STREAM_CAPTURE, &motu424_pcm_ops);

	/*
	 * Plain host memory: the card never sees a host address - all data
	 * moves by PIO into the card aperture (see docs/transport.md), so no
	 * device-visible DMA buffer is needed.
	 */
	snd_pcm_set_managed_buffer_all(pcm, SNDRV_DMA_TYPE_VMALLOC, NULL,
				       MOTU424_MAX_BUFFER_BYTES,
				       MOTU424_MAX_BUFFER_BYTES);
	return 0;
}
