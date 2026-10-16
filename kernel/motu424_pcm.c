// SPDX-License-Identifier: GPL-2.0-or-later
/*
 * motu424_pcm.c - ALSA PCM interface (playback).
 *
 * Standard PCM plumbing: it wires ALSA's callbacks to the hardware layer in
 * motu424_hw.c. No register offsets appear here.
 *
 * The ALSA buffer is the host ring itself: a coherent buffer the DSP reads
 * by bus-master DMA, exactly MOTU424_RING_BYTES long. Capture is not exposed
 * yet: where the DSP writes recorded audio on the host side is still open
 * (docs/transport.md).
 */
#include <linux/module.h>
#include <sound/core.h>
#include <sound/pcm.h>
#include <sound/pcm_params.h>

#include "motu424.h"

/*
 * The frame layout on the ring is not confirmed (docs/transport.md), so the
 * format is raw 32-bit words, frame_bytes / 4 per frame; the channel count is
 * pinned in open().
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
	.period_bytes_max = MOTU424_RING_BYTES / 2,
	.periods_min = 2,
	.periods_max = MOTU424_MAX_PERIODS,
};

static int motu424_pcm_open(struct snd_pcm_substream *substream)
{
	struct motu424 *chip = snd_pcm_substream_chip(substream);
	struct snd_pcm_runtime *runtime = substream->runtime;
	unsigned int channels = chip->frame_bytes / 4;
	int err;

	if (!chip->audio_base || !chip->frame_bytes) {
		dev_warn_once(&chip->pci->dev,
			      "streaming disabled: the host-ring frame size is not known yet (motu424.frame_bytes=)\n");
		return -ENXIO;
	}

	runtime->hw = motu424_pcm_hw;
	runtime->hw.channels_min = channels;
	runtime->hw.channels_max = channels;

	/* The DSP wraps at the ring size: the buffer must be exactly that. */
	err = snd_pcm_hw_constraint_minmax(runtime,
					   SNDRV_PCM_HW_PARAM_BUFFER_BYTES,
					   MOTU424_RING_BYTES,
					   MOTU424_RING_BYTES);
	if (err < 0)
		return err;
	err = snd_pcm_hw_constraint_integer(runtime,
					    SNDRV_PCM_HW_PARAM_PERIODS);
	if (err < 0)
		return err;

	chip->playback.substream = substream;
	return 0;
}

static int motu424_pcm_close(struct snd_pcm_substream *substream)
{
	struct motu424 *chip = snd_pcm_substream_chip(substream);

	chip->playback.substream = NULL;
	return 0;
}

static int motu424_pcm_prepare(struct snd_pcm_substream *substream)
{
	/* Start the DSP on silence; ALSA fills the buffer before START. */
	return snd_pcm_format_set_silence(substream->runtime->format,
					  substream->runtime->dma_area,
					  substream->runtime->buffer_size *
					  substream->runtime->channels);
}

static int motu424_pcm_trigger(struct snd_pcm_substream *substream, int cmd)
{
	struct motu424 *chip = snd_pcm_substream_chip(substream);

	switch (cmd) {
	case SNDRV_PCM_TRIGGER_START:
		motu424_hw_stream_start(chip, substream->runtime->dma_addr);
		return 0;
	case SNDRV_PCM_TRIGGER_STOP:
		motu424_hw_stream_stop(chip);
		return 0;
	default:
		return -EINVAL;
	}
}

static int motu424_pcm_sync_stop(struct snd_pcm_substream *substream)
{
	motu424_hw_stream_sync_stop(snd_pcm_substream_chip(substream));
	return 0;
}

static snd_pcm_uframes_t motu424_pcm_pointer(struct snd_pcm_substream *substream)
{
	return motu424_hw_stream_pointer(snd_pcm_substream_chip(substream),
					 substream->runtime->dma_addr);
}

static const struct snd_pcm_ops motu424_pcm_ops = {
	.open = motu424_pcm_open,
	.close = motu424_pcm_close,
	.prepare = motu424_pcm_prepare,
	.trigger = motu424_pcm_trigger,
	.sync_stop = motu424_pcm_sync_stop,
	.pointer = motu424_pcm_pointer,
};

int motu424_pcm_create(struct motu424 *chip)
{
	struct snd_pcm *pcm;
	int err;

	err = snd_pcm_new(chip->card, MOTU424_DRIVER_NAME, 0, 1, 0, &pcm);
	if (err < 0)
		return err;

	pcm->private_data = chip;
	strscpy(pcm->name, chip->model, sizeof(pcm->name));
	chip->pcm = pcm;

	snd_pcm_set_ops(pcm, SNDRV_PCM_STREAM_PLAYBACK, &motu424_pcm_ops);

	/* The host ring: coherent, 32-bit addressable, read by the DSP. */
	snd_pcm_set_managed_buffer_all(pcm, SNDRV_DMA_TYPE_DEV,
				       &chip->pci->dev,
				       MOTU424_RING_BYTES,
				       MOTU424_RING_BYTES);
	return 0;
}
