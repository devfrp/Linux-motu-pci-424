===================================================================
motu424 - MOTU PCI-324/PCI-424/PCIe-424 (HD Express) ALSA PCI driver
===================================================================

Supported hardware
===================

Mark of the Unicorn (vendor ID ``0x137A``) PCI/PCIe AudioWire host cards:

======  ==============================================
DEV ID  Card
======  ==============================================
0x0003  PCI-324
0x0004  PCI-424
0x0005  PCIe-424 ("HD Express")
======  ==============================================

These cards have no host-visible audio I/O of their own; they are the PCI
bridge for a MOTU AudioWire breakout interface (828, 2408, 24I/O, 896HD,
HD192, ...) attached over the AudioWire cable. This driver talks to the PCI
card only. Per-interface channel/mixer enumeration is not yet implemented.

Provenance
==========

No vendor programming documentation exists for this hardware. The register
map, transport protocol and IRQ semantics implemented here were recovered
by static reverse engineering of the vendor Windows driver and of the
card's DSP program, and checked on a real card, for interoperability
purposes; see ``CLEANROOM.md`` and ``docs/register-map.md``,
``docs/transport.md``, ``docs/fpga-upload.md`` and
``docs/vendor-driver-map.md`` in the driver's source tree for the full
evidence trail behind every register access in ``motu424_hw.c``.

Architecture
============

The driver keeps every hardware-specific fact confined to ``motu424.h`` and
``motu424_hw.c``; the PCI attach path (``motu424_main.c``) and the ALSA PCM
ops (``motu424_pcm.c``) are hardware-agnostic and never touch a register
offset directly.

Access model
------------

The card's PCI interface is the PCI port of a TI TMS320C6412 DSP in PCI
boot mode. The I/O BAR holds the DSP's Host Port Interface registers (HSR,
HDCR, DSPP); the 8 MiB MMIO BAR (window A) is the DSP peripheral register
space and the 4 MiB one (window B) the DSP memory page selected by DSPP.
``motu424_assign_windows()`` classifies the BARs by type and size. Every
write goes through an allow-list: DSP internal RAM in window B, and only
EMIF CECTL0, the GPIO registers and RSTSRC in window A, so the DSP EEPROM
controller that holds the card's PCI identity cannot be written.

Firmware
--------

The board's FPGA has no configuration PROM, so the host loads it on every
start, as the vendor driver does. ``motu424_hw_init()`` warm-resets the
DSP, clocks the FPGA image out through the DSP's GPIO, writes the DSP
program into internal RAM and checks it, releases the CPU (DSPINT) and
reads the audio block address the program publishes in its mailbox.

Both images belong to MOTU and are not distributed with the driver. Users
extract them from their own copy of the vendor driver with
``tools/re/extract-firmware.py`` and install them as::

   /lib/firmware/motu424/pci424-serial-container.bin
   /lib/firmware/motu424/pci424-program.bin

Their lengths and SHA-256 are checked before anything reaches the card;
probe fails if either is missing or different.

Transport
---------

The DSP is the PCI bus master. The ALSA buffer is a 512 KiB coherent host
ring (``SNDRV_DMA_TYPE_DEV``, 32-bit DMA mask) whose bus address the driver
hands to the DSP; the DSP fetches audio from it and reports the bus address
it has reached, which ``.pointer`` turns into a frame offset.

Interrupts
----------

The DSP raises PINTA# every 64 frames (at 1x). IRQ pending is HSR bit 1;
the driver acknowledges it by writing INTRST (``0x10``) to the DSP's RSTSRC
register and calls ``snd_pcm_period_elapsed()``.

Module parameters
=================

.. code-block:: text

   frame_bytes=<n>   Bytes per frame on the host ring. Not confirmed yet;
                     0 (the default) keeps streaming disabled.

Current status and limitations
===============================

- The firmware load matches the sequence run on a real PCI-424 with
  ``tools/motu424-bringup``. The stream path (start, stop, interrupt,
  pointer) follows the vendor driver and the DSP program but has not run on
  a card yet.
- Only the PCI-424 (``0x0004``) is supported. The PCI-324 loads its FPGA
  through different registers, and the PCIe-424 has not been checked on a
  card.
- Playback only, 48 kHz only, and the frame layout on the ring is unknown:
  samples are exposed as ``frame_bytes / 4`` raw ``S32_LE`` words. Capture
  and rate changes (the vendor relock sequence) are not implemented.
- No mixer (CueMix) kcontrols are registered yet; a companion userspace tool
  (``tools/motu424-ctl``, ``tools/motu424-gui``) implements the control
  surface against the planned kcontrol names in
  ``docs/cuemix-control-map.md``.
- AudioWire breakout interface enumeration is not implemented.
- No suspend/resume: the FPGA loses its configuration with power, so resume
  will need to repeat the full load.

This note will be extended (and relocated if submitted upstream) once the
driver has been validated against real hardware.
