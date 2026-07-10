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
by static reverse engineering of the vendor Windows driver for
interoperability purposes; see ``CLEANROOM.md`` and ``docs/register-map.md``,
``docs/transport.md``, ``docs/vendor-driver-map.md`` in the driver's source
tree for the full evidence trail behind every register access in
``motu424_hw.c``.

Architecture
============

The driver keeps every hardware-specific fact confined to ``motu424.h`` and
``motu424_hw.c``; the PCI attach path (``motu424_main.c``) and the ALSA PCM
ops (``motu424_pcm.c``) are hardware-agnostic and never touch a register
offset directly.

Access model
------------

The card exposes a windowed ~24-bit card-address space over two MMIO BARs
(window A, 8 MiB; window B, 4 MiB) plus a small I/O-port bridge BAR, rather
than a flat register file. ``motu424_assign_windows()`` classifies the
probed BARs by type and size at load time; a single-MMIO-BAR card routes
everything through window B.

Transport
---------

Audio is **not** host bus-mastered. The host pushes each period into a
card-side PIO aperture in window B (``memcpy_toio()``) using plain
``vmalloc`` host buffers (``SNDRV_DMA_TYPE_VMALLOC``); there is no DMA mask
and no scatter-gather setup. This matches the vendor driver, whose PnP
resource-list parser never processes a ``CmResourceTypeDma`` descriptor on
this path.

Interrupts
----------

IRQ pending is port-BAR offset ``0x0`` bit 1; acknowledgement is a write of
``0x10`` to a card-reported ack address. Period-elapsed accounting is a
per-IRQ sample accumulator compared against the configured period size.

Firmware
--------

The classic PCI-324/424 self-configures its Altera FPGA from onboard flash
at power-on; the driver uploads no firmware and declares none via
``MODULE_FIRMWARE()``. The PCIe HD Express variant (``DEV_0005``) uses a
different ARM+Xilinx image and host-side upload for that variant is not yet
implemented (out of scope until that card revision is targeted).

Bring-up and module parameters
===============================

The card-reported runtime addresses for the audio register block, IRQ ack
register, CueMix coefficient region and the two streaming apertures are read
by the vendor driver from the card at init time and cannot be predicted
statically. Until they are supplied, this driver maps BARs and services
interrupts but refuses to start a stream (``-ENXIO``). Obtain them with
``tools/motu424-probe`` (run with the driver unbound from the device) and
pass them as module parameters:

.. code-block:: text

   audio_base=<hex>      Card address of the audio register block
   ack_addr=<hex>        Card address of the IRQ ack register
   mix_base=<hex>        Card address of the CueMix coefficient region
   play_aperture=<hex>   Card address of the playback aperture
   cap_aperture=<hex>    Card address of the capture aperture

Current status and limitations
===============================

- Playback/capture at the base 1x rates (44.1/48 kHz) is implemented against
  the recovered register model but has not been exercised against real
  hardware (no card on the development machine); everything above is
  therefore unverified in practice, not just in the "TODO: verify" sense
  used for a handful of still-uncertain bit-level details in the header.
- No mixer (CueMix) kcontrols are registered yet; a companion userspace tool
  (``tools/motu424-ctl``, ``tools/motu424-gui``) implements the control
  surface against the planned kcontrol names in
  ``docs/cuemix-control-map.md``, ready to light up once the driver side is
  wired.
- AudioWire breakout interface enumeration (channel/bus counts per attached
  interface) is not implemented; the driver currently assumes the
  fixed-frame channel counts documented in ``motu424.h``.
- No suspend/resume firmware re-upload path (not needed for the classic
  card's self-configuring FPGA; unimplemented for HD Express).

This note will be extended (and relocated if submitted upstream) once the
driver has been validated against real hardware.
