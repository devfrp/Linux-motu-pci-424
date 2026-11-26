// SPDX-License-Identifier: GPL-2.0-or-later

//! motu424 - Linux ALSA driver for the MOTU PCI-324 / PCI-424 audio card, in Rust.
//!
//! This file is the PCI driver: attach and detach, the BARs, DMA, the IRQ and the teardown
//! order. All register-level semantics live in `hw.rs`; the ALSA card is registered by
//! `motu424_alsa.c`, since the kernel has no Rust bindings for ALSA.
//!
//! The MOTU PCI-324/424 is undocumented hardware. The model in `hw.rs` comes from static RE of
//! the vendor Windows driver MOTUAW.sys and of the card's own DSP program, checked on a real
//! PCI-424 with the C driver in `kernel/` (issue #2). The board is a TI TMS320C6412 DSP (its PCI
//! port is the card's PCI interface) plus an FPGA with no config PROM, so the host loads both on
//! every start. The DSP is the PCI bus master: it moves audio to and from a host ring by itself.

use kernel::{
    bindings,
    device::Core,
    device::{Bound, Device},
    dma::{self, DmaMask},
    error::to_result,
    irq, pci,
    prelude::*,
    sync::Arc,
};

mod alsa;
mod hw;
mod sha256;

use hw::{Bars, Chip, Io};

const NAME: &CStr = c"motu424";

// PCI identity, from the vendor driver package (MOTUAW.inf, "MOTU Audio Installer 4.0.6").
// 0x137A is Mark of the Unicorn's PCI vendor ID; 0003..0005 are the PCI-324 / PCI-424 /
// PCIe-424 (HD Express) revisions, all handled by the vendor's "DriverInstall424" section.
const MOTU_VENDOR_ID: u32 = 0x137a;
const DEV_PCI324: u32 = 0x0003;
const DEV_PCI424: u32 = 0x0004;
const DEV_PCIE424: u32 = 0x0005;

/// `pci::DeviceId::from_id()` takes a `pci::Vendor`, which has no entry for MOTU and no public
/// constructor, so the IDs are built from the raw C structure, as `PCI_DEVICE()` does.
const fn motu_id(device: u32) -> pci::DeviceId {
    let raw = bindings::pci_device_id {
        vendor: MOTU_VENDOR_ID,
        device,
        subvendor: !0,
        subdevice: !0,
        class: 0,
        class_mask: 0,
        driver_data: 0,
        override_only: 0,
    };
    // SAFETY: `pci::DeviceId` is a `#[repr(transparent)]` wrapper of `struct pci_device_id` and
    // adds no invariants.
    unsafe { core::mem::transmute::<bindings::pci_device_id, pci::DeviceId>(raw) }
}

kernel::pci_device_table!(
    PCI_TABLE,
    MODULE_PCI_TABLE,
    <Motu424Driver as pci::Driver>::IdInfo,
    [
        (motu_id(DEV_PCI324), ()),
        (motu_id(DEV_PCI424), ()),
        (motu_id(DEV_PCIE424), ()),
    ]
);

fn raw_pci(pdev: &pci::Device<Core<'_>>) -> *mut bindings::pci_dev {
    // `pci::Device` is a `#[repr(transparent)]` wrapper of `Opaque<struct pci_dev>`, and its
    // `as_raw()` is private.
    core::ptr::from_ref(pdev).cast_mut().cast()
}

/// `pci_enable_device()`, undone on drop. The Rust PCI API only offers
/// `enable_device_mem()`, and the card's HPI registers are an I/O-port BAR.
struct Enabled(*mut bindings::pci_dev);

// SAFETY: `pci_disable_device()` may be called from any thread.
unsafe impl Send for Enabled {}

impl Enabled {
    fn new(pdev: &pci::Device<Core<'_>>) -> Result<Self> {
        let raw = raw_pci(pdev);
        // SAFETY: `raw` is the valid `struct pci_dev` being probed.
        to_result(unsafe { bindings::pci_enable_device(raw) })?;
        Ok(Self(raw))
    }
}

impl Drop for Enabled {
    fn drop(&mut self) {
        // SAFETY: the device was enabled by `new()` and is still bound: `Enabled` is dropped
        // with the driver data, before the device is released.
        unsafe { bindings::pci_disable_device(self.0) };
    }
}

/// Maps every BAR the card exposes and assigns them their roles from their types and sizes: the
/// I/O BAR is the HPI register set, the 8 MB MMIO BAR is window A (DSP peripherals), the 4 MB
/// one window B (DSP memory). All three are required.
fn map_bars(pdev: &pci::Device<Core<'_>>) -> Result<Bars> {
    let (mut port, mut win_a, mut win_b) = (None, None, None);

    for i in 0..bindings::PCI_STD_NUM_BARS {
        let len = pdev.resource_len(i)?;
        if len == 0 {
            continue;
        }
        // SAFETY: `i` is a standard BAR index of the valid device being probed.
        let flags = unsafe { (*raw_pci(pdev)).resource[i as usize].flags };
        let is_io = flags & bindings::IORESOURCE_IO as usize != 0;
        let is_mem = flags & bindings::IORESOURCE_MEM as usize != 0;

        let slot = if is_io && port.is_none() {
            &mut port
        } else if is_mem && len >= hw::WINA_LEN && win_a.is_none() {
            &mut win_a
        } else if is_mem && len >= hw::WINB_LEN && win_b.is_none() {
            &mut win_b
        } else {
            continue;
        };
        let bar = pdev
            .iomap_region(i, NAME)
            .inspect_err(|e| dev_err!(pdev, "cannot request/map BAR{}: {}\n", i, e.to_errno()))?;
        *slot = Some(bar.into_devres()?);
    }

    match (port, win_a, win_b) {
        (Some(port), Some(win_a), Some(win_b)) => Ok(Bars { win_a, win_b, port }),
        _ => {
            dev_err!(pdev, "expected an I/O BAR, an 8 MB and a 4 MB MMIO BAR\n");
            Err(ENODEV)
        }
    }
}

/// The bound driver data.
///
/// Teardown order matters and is fixed by `PinnedDrop` and then the field order: disconnect the
/// ALSA card (no more PCM callbacks), quiesce and reset the DSP (no more DMA or interrupts), free
/// the IRQ, drop the chip with its DMA buffers, disable the device. The BAR mappings and the ALSA
/// card are devres resources, released after this.
#[pin_data(PinnedDrop)]
struct Motu424 {
    card: alsa::Card,
    #[pin]
    irq: irq::Registration<Arc<Chip>>,
    chip: Arc<Chip>,
    _enabled: Enabled,
}

#[pinned_drop]
impl PinnedDrop for Motu424 {
    fn drop(self: Pin<&mut Self>) {
        self.card.disconnect();
        self.chip.shutdown();
    }
}

/// Mixer handover and idle stream, which make the DSP read host memory, then the ALSA card.
/// Runs once the IRQ is registered; on failure the DSP is quiesced before the IRQ goes away.
fn start(chip: &Chip, model: &CStr) -> Result<alsa::Card> {
    chip.relock();
    chip.idle_start();
    alsa::Card::new(chip, model).inspect_err(|_| chip.shutdown())
}

struct Motu424Driver;

impl pci::Driver for Motu424Driver {
    type IdInfo = ();
    type Data<'bound> = Motu424;

    const ID_TABLE: pci::IdTable<Self::IdInfo> = &PCI_TABLE;

    fn probe<'bound>(
        pdev: &'bound pci::Device<Core<'_>>,
        _info: &'bound Self::IdInfo,
    ) -> impl PinInit<Self::Data<'bound>, Error> + 'bound {
        pin_init::pin_init_scope(move || {
            alsa::slot()?;

            // Only the PCI-424 (0004) load sequence is implemented and tested. The PCI-324
            // (0003) loads its FPGA through different registers, and the PCIe-424 (0005) has
            // not been checked on a card yet (docs/fpga-upload.md).
            let device = u32::from(pdev.device_id());
            if device != DEV_PCI424 {
                dev_warn!(
                    pdev,
                    "device {:04x} is not supported yet: only the PCI-424 (0004) firmware load is implemented\n",
                    device
                );
                return Err(ENODEV);
            }
            let model = c"MOTU PCI-424";

            let enabled = Enabled::new(pdev)?;
            let bars = map_bars(pdev)?;

            // SAFETY: no DMA allocation or mapping exists for this device yet.
            unsafe { dma::Device::dma_set_mask_and_coherent(pdev, DmaMask::new::<32>())? };

            // Load the FPGA and DSP program; PINTA# stays masked until a stream.
            let dev: &Device<Bound> = pdev.as_ref();
            let boot = hw::boot(&Io::new(&bars, dev)?, alsa::params())?;
            let chip = Arc::pin_init(Chip::new(dev, bars, boot), GFP_KERNEL)?;

            // The DSP is the PCI bus master: it fetches audio from the host ring. Enabled only
            // now that it has been reset and reloaded, so a DSP left running by a warm reboot
            // cannot reach stale host addresses.
            pdev.set_master();

            let vector = *pdev
                .alloc_irq_vectors(1, 1, pci::IrqTypes::all())
                .inspect_err(|_| dev_err!(pdev, "cannot allocate an IRQ vector\n"))?
                .start();

            Ok(try_pin_init!(Motu424 {
                irq <- pdev.request_irq(vector, irq::Flags::SHARED, NAME, Ok::<_, Error>(chip.clone())),
                card: start(&chip, model)?,
                chip,
                _enabled: enabled,
            }? Error))
        })
    }
}

kernel::module_pci_driver! {
    type: Motu424Driver,
    name: "motu424",
    authors: ["motu-pci-424 contributors"],
    description: "MOTU PCI-324/424 audio card driver",
    license: "GPL",
}
