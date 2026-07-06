// SPDX-License-Identifier: GPL-2.0-or-later
/*
 * motu424 - MOTU PCI-324/424 ALSA driver: PCI attach/detach, resource
 * management and the interrupt handler.
 *
 * This file contains the standard, hardware-agnostic driver framework. All
 * register-level semantics live in motu424_hw.c.
 */
#include <linux/module.h>
#include <linux/init.h>
#include <linux/pci.h>
#include <linux/interrupt.h>
#include <sound/core.h>
#include <sound/initval.h>

#include "motu424.h"

static int index[SNDRV_CARDS] = SNDRV_DEFAULT_IDX;
static char *id[SNDRV_CARDS] = SNDRV_DEFAULT_STR;
static bool enable[SNDRV_CARDS] = SNDRV_DEFAULT_ENABLE_PNP;

module_param_array(index, int, NULL, 0444);
MODULE_PARM_DESC(index, "Index value for the MOTU PCI-424 card.");
module_param_array(id, charp, NULL, 0444);
MODULE_PARM_DESC(id, "ID string for the MOTU PCI-424 card.");
module_param_array(enable, bool, NULL, 0444);
MODULE_PARM_DESC(enable, "Enable the MOTU PCI-424 card.");

static const struct pci_device_id motu424_ids[] = {
	{ PCI_DEVICE(MOTU424_VENDOR_ID, MOTU424_DEV_PCI424_A) },	/* PCI-324  */
	{ PCI_DEVICE(MOTU424_VENDOR_ID, MOTU424_DEV_PCI424_B) },	/* PCI-424  */
	{ 0, }
};
MODULE_DEVICE_TABLE(pci, motu424_ids);

/*
 * Interrupt handler. The card raises one IRQ per elapsed period, per active
 * DMA engine. motu424_hw_irq_ack() reads and clears the source register and
 * tells us which streams advanced; we then notify ALSA.
 */
static irqreturn_t motu424_interrupt(int irq, void *dev_id)
{
	struct motu424 *chip = dev_id;
	u32 pending;

	pending = motu424_hw_irq_ack(chip);
	if (!pending)
		return IRQ_NONE;

	if ((pending & MOTU424_IRQ_PLAY) && chip->playback.running &&
	    chip->playback.substream)
		snd_pcm_period_elapsed(chip->playback.substream);

	if ((pending & MOTU424_IRQ_REC) && chip->capture.running &&
	    chip->capture.substream)
		snd_pcm_period_elapsed(chip->capture.substream);

	return IRQ_HANDLED;
}

/* devres action: quiesce the card before its IRQ/DMA go away. */
static void motu424_hw_shutdown_action(void *data)
{
	motu424_hw_shutdown((struct motu424 *)data);
}

/* devres action: pci_alloc_irq_vectors() is not itself managed. */
static void motu424_free_irq_vectors_action(void *data)
{
	pci_free_irq_vectors((struct pci_dev *)data);
}

static int motu424_probe(struct pci_dev *pci, const struct pci_device_id *ent)
{
	static int dev;
	struct snd_card *card;
	struct motu424 *chip;
	int i, err;

	if (dev >= SNDRV_CARDS)
		return -ENODEV;
	if (!enable[dev]) {
		dev++;
		return -ENOENT;
	}

	/* Card is devm-managed: its teardown is tied to the pci device. */
	err = snd_devm_card_new(&pci->dev, index[dev], id[dev], THIS_MODULE,
				sizeof(*chip), &card);
	if (err < 0)
		return err;

	chip = card->private_data;
	chip->card = card;
	chip->pci = pci;
	spin_lock_init(&chip->lock);

	err = pcim_enable_device(pci);
	if (err < 0)
		return err;

	/*
	 * Map every BAR the card exposes, without interpreting them: the
	 * hardware layer decides which mapping is which window (the card is
	 * not a single flat register BAR). pcim_iomap() handles both MMIO
	 * and I/O-port BARs.
	 */
	for (i = 0; i < PCI_STD_NUM_BARS; i++) {
		if (!pci_resource_len(pci, i))
			continue;
		chip->bars[i].ptr = pcim_iomap(pci, i, 0);
		if (!chip->bars[i].ptr) {
			dev_err(&pci->dev, "cannot map BAR%d\n", i);
			return -ENOMEM;
		}
		chip->bars[i].len = pci_resource_len(pci, i);
		chip->bars[i].flags = pci_resource_flags(pci, i);
	}

	/* No bus mastering: the card is fed by PIO, it never DMAs host RAM. */

	snprintf(chip->model, sizeof(chip->model), "MOTU PCI-%s",
		 ent->device == MOTU424_DEV_PCI424_A ? "324" : "424");

	/* Bring the hardware to a known idle state before enabling IRQs. */
	err = motu424_hw_init(chip);
	if (err < 0)
		return err;

	err = pci_alloc_irq_vectors(pci, 1, 1, PCI_IRQ_ALL_TYPES);
	if (err < 0) {
		dev_err(&pci->dev, "cannot allocate an IRQ vector\n");
		return err;
	}
	err = devm_add_action_or_reset(&pci->dev,
				       motu424_free_irq_vectors_action, pci);
	if (err < 0)
		return err;
	chip->irq = pci_irq_vector(pci, 0);

	err = devm_request_irq(&pci->dev, chip->irq, motu424_interrupt,
			       IRQF_SHARED, MOTU424_DRIVER_NAME, chip);
	if (err < 0) {
		dev_err(&pci->dev, "cannot grab IRQ %d\n", chip->irq);
		return err;
	}
	card->sync_irq = chip->irq;

	/*
	 * Registered last so it runs first on teardown: quiesce DMA and mask
	 * interrupts before the IRQ handler and vectors are torn down (devres
	 * releases in reverse order). This keeps a shared IRQ line from
	 * "screaming" after free_irq().
	 */
	err = devm_add_action_or_reset(&pci->dev,
				       motu424_hw_shutdown_action, chip);
	if (err < 0)
		return err;

	err = motu424_pcm_create(chip);
	if (err < 0)
		return err;

	strscpy(card->driver, MOTU424_DRIVER_NAME, sizeof(card->driver));
	strscpy(card->shortname, chip->model, sizeof(card->shortname));
	snprintf(card->longname, sizeof(card->longname),
		 "%s at %s, irq %d", chip->model, pci_name(pci), chip->irq);

	err = snd_card_register(card);
	if (err < 0)
		return err;

	pci_set_drvdata(pci, card);
	dev_info(&pci->dev, "%s registered as ALSA card %d\n",
		 chip->model, card->number);
	dev++;
	return 0;
}

/*
 * No .remove callback is needed: all resources (card, IRQ, BAR mapping, the
 * hw_shutdown action and the PCI enable) are devm/pcim managed and released in
 * reverse order automatically when the device detaches.
 */
static struct pci_driver motu424_driver = {
	.name		= MOTU424_DRIVER_NAME,
	.id_table	= motu424_ids,
	.probe		= motu424_probe,
};

module_pci_driver(motu424_driver);

MODULE_AUTHOR("motu-pci-424 contributors");
MODULE_DESCRIPTION("MOTU PCI-324/424 audio card driver");
MODULE_LICENSE("GPL");
