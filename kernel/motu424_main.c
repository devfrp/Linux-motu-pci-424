// SPDX-License-Identifier: GPL-2.0-or-later
/*
 * motu424 - MOTU PCI-324/424 ALSA driver: PCI attach/detach, resource
 * management and the interrupt handler.
 *
 * This file contains the standard, hardware-agnostic driver framework. All
 * register-level semantics live in motu424_hw.c.
 */
#include <linux/err.h>
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
	{ PCI_DEVICE(MOTU424_VENDOR_ID, MOTU424_DEV_PCI424_C) },	/* PCIe-424 */
	{ 0, }
};
MODULE_DEVICE_TABLE(pci, motu424_ids);

/*
 * Interrupt handler. The DSP raises one interrupt every IRQ_FRAMES frames;
 * motu424_hw_irq_ack() acks it, and ALSA reads the real position through
 * the pointer callback.
 */
static irqreturn_t motu424_interrupt(int irq, void *dev_id)
{
	struct motu424 *chip = dev_id;
	u32 pending;

	pending = motu424_hw_irq_ack(chip);
	if (!pending)
		return IRQ_NONE;

	if ((pending & MOTU424_IRQ_PLAY) && chip->playback.substream)
		snd_pcm_period_elapsed(chip->playback.substream);

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

	/*
	 * Only the PCI-424 (0004) load sequence is implemented and tested.
	 * The PCI-324 (0003) loads its FPGA through different registers, and
	 * the PCIe-424 (0005) has not been checked on a card yet
	 * (docs/fpga-upload.md).
	 */
	if (ent->device != MOTU424_DEV_PCI424_B) {
		dev_warn(&pci->dev,
			 "device %04x is not supported yet: only the PCI-424 (0004) firmware load is implemented\n",
			 ent->device);
		return -ENODEV;
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
	 * not a single flat register BAR). pcim_iomap_region() both reserves
	 * (pci_request_region()) and maps each BAR in one managed call, so a
	 * conflicting claim on the same resource is caught here instead of
	 * being silently skipped (plain pcim_iomap() only maps and never
	 * requests the region).
	 */
	for (i = 0; i < PCI_STD_NUM_BARS; i++) {
		void __iomem *p;

		if (!pci_resource_len(pci, i))
			continue;
		p = pcim_iomap_region(pci, i, MOTU424_DRIVER_NAME);
		if (IS_ERR(p)) {
			dev_err(&pci->dev, "cannot request/map BAR%d: %pe\n",
				i, p);
			return PTR_ERR(p);
		}
		chip->bars[i].ptr = p;
		chip->bars[i].len = pci_resource_len(pci, i);
		chip->bars[i].flags = pci_resource_flags(pci, i);
	}

	err = dma_set_mask_and_coherent(&pci->dev, DMA_BIT_MASK(32));
	if (err < 0)
		return err;

	snprintf(chip->model, sizeof(chip->model), "MOTU PCI-%s",
		 ent->device == MOTU424_DEV_PCI424_A ? "324" : "424");

	/* Load the FPGA and DSP program; PINTA# stays masked until a stream. */
	err = motu424_hw_init(chip);
	if (err < 0)
		return err;

	/*
	 * The DSP is the PCI bus master: it fetches audio from the host ring.
	 * Enabled only now that it has been reset and reloaded, so a DSP left
	 * running by a warm reboot cannot reach stale host addresses.
	 */
	pci_set_master(pci);

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
	 * Registered after the IRQ so it runs before it on teardown: stop and
	 * reset the DSP, ending its DMA, and mask interrupts before the IRQ
	 * handler and vectors are torn down (devres releases in reverse
	 * order). The host ring is freed later with the card.
	 */
	err = devm_add_action_or_reset(&pci->dev,
				       motu424_hw_shutdown_action, chip);
	if (err < 0)
		return err;

	/*
	 * The mixer-program handover makes the DSP read host memory, so it
	 * needs bus mastering and must come after the shutdown action, which
	 * resets the DSP before that buffer is freed.
	 */
	motu424_hw_relock(chip);

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
