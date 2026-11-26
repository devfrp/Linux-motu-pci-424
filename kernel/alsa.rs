// SPDX-License-Identifier: GPL-2.0-or-later

//! The ALSA card and its PCM device (playback).
//!
//! The kernel has no Rust bindings for ALSA, so `Kbuild` generates them with bindgen from the
//! kernel's own `<sound/core.h>` and `<sound/pcm.h>`, with the flags the kernel uses for its own
//! bindings: every ALSA structure below is laid out from the real headers of the kernel being
//! built for. No register offset appears here; the PCM callbacks call into [`Chip`].
//!
//! The ALSA buffer is the host ring itself: a coherent buffer the DSP reads by bus-master DMA,
//! `RING_PERIODS` periods of `period_bytes` each. Capture is not exposed yet: where the DSP
//! writes recorded audio on the host side is still open (docs/transport.md).

use core::{
    ffi::{c_int, c_uint, c_void},
    fmt::Write,
};

use kernel::{
    bindings,
    device::{Bound, Device},
    error::to_result,
    prelude::*,
    sync::atomic::{Acquire, Atomic, Relaxed, Release},
};

use crate::{
    hw::{Chip, RING_BYTES, RING_PERIODS},
    params::{self, SNDRV_CARDS},
};

/// The bindgen output for `<sound/core.h>` and `<sound/pcm.h>` (see `Kbuild`).
#[allow(
    clippy::all,
    clippy::pedantic,
    clippy::restriction,
    clippy::nursery,
    dead_code,
    improper_ctypes,
    missing_docs,
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals,
    unreachable_pub,
    unnecessary_transmutes,
    unsafe_op_in_unsafe_fn
)]
pub(crate) mod sys {
    // Blocklisted in `Kbuild`, as the kernel does: mapped to `usize`/`isize` by hand.
    pub type __kernel_size_t = usize;
    pub type __kernel_ssize_t = isize;
    pub type __kernel_ptrdiff_t = isize;

    include!("alsa_generated.rs");
}

// The uapi values these macros stand for: the macros cast through `__force` types, which
// bindgen cannot evaluate. Both are part of the ALSA user-space ABI.
const FORMAT_S32_LE: sys::snd_pcm_format_t = 10;
const FMTBIT_S32_LE: u64 = 1 << FORMAT_S32_LE;
const STREAM_PLAYBACK: c_int = 0;
const RATE: c_uint = 48000;
const NAME: &CStr = c"motu424";

/// The next card slot of the `index`/`id`/`enable` arrays, advanced per probed card.
static NEXT_SLOT: Atomic<usize> = Atomic::new(0);

/// Checks the `enable` parameter for the next card: `ENODEV` past the last slot, `ENOENT` when
/// this card is disabled. Returns the slot.
pub(crate) fn slot() -> Result<usize> {
    let slot = NEXT_SLOT.load(Relaxed);
    if slot >= SNDRV_CARDS {
        return Err(ENODEV);
    }
    if !params::enable.get()[slot] {
        NEXT_SLOT.store(slot + 1, Relaxed);
        return Err(ENOENT);
    }
    Ok(slot)
}

/// Lives in `snd_card->private_data`.
struct CardData {
    chip: *const Chip,
    /// The open playback substream, null when closed.
    substream: Atomic<*mut sys::snd_pcm_substream>,
}

/// A registered ALSA card.
pub(crate) struct Card {
    card: *mut sys::snd_card,
}

// SAFETY: the card is only touched through ALSA, which does its own locking.
unsafe impl Send for Card {}
// SAFETY: as above; the only `&self` method is `disconnect()`.
unsafe impl Sync for Card {}

/// Copies `s` into a fixed C string field, truncated and NUL-terminated, like `strscpy()`.
fn set_str(field: &mut [core::ffi::c_char], s: &[u8]) {
    let n = s.len().min(field.len() - 1);
    for (d, &b) in field.iter_mut().zip(&s[..n]) {
        *d = b as core::ffi::c_char;
    }
    field[n] = 0;
}

/// Formats into a fixed C string field, truncating like `snprintf()`.
struct FieldWriter<'a> {
    field: &'a mut [core::ffi::c_char],
    len: usize,
}

impl Write for FieldWriter<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for &b in s.as_bytes() {
            if self.len + 1 < self.field.len() {
                self.field[self.len] = b as core::ffi::c_char;
                self.len += 1;
            }
        }
        self.field[self.len] = 0;
        Ok(())
    }
}

fn raw_device(dev: &Device<Bound>) -> *mut sys::device {
    // `device::Device` is a `#[repr(transparent)]` wrapper of `Opaque<struct device>`, and its
    // `as_raw()` is crate-private.
    core::ptr::from_ref(dev).cast_mut().cast()
}

impl Card {
    /// Creates and registers the card for `chip`, in card slot `slot`, with `irq` as its
    /// sync IRQ. The card is devm-managed: freed with the device, after the driver data, which
    /// calls [`Card::disconnect`] first. `chip` must stay valid until then.
    pub(crate) fn new(chip: &Chip, model: &CStr, slot: usize, irq: c_int) -> Result<Self> {
        let dev = chip.device();
        let mut card: *mut sys::snd_card = core::ptr::null_mut();
        let mut pcm: *mut sys::snd_pcm = core::ptr::null_mut();

        // SAFETY: the device is bound; `index` and `id` are parameter arrays of SNDRV_CARDS
        // entries and `slot < SNDRV_CARDS`; `id` is NULL or a string the kernel allocated.
        to_result(unsafe {
            sys::snd_devm_card_new(
                raw_device(dev),
                params::index.get()[slot],
                params::id.get()[slot].0.cast(),
                crate::THIS_MODULE.as_ptr().cast(),
                core::mem::size_of::<CardData>(),
                &mut card,
            )
        })?;

        // SAFETY: `snd_devm_card_new()` returned a valid card whose `private_data` points to
        // `size_of::<CardData>()` bytes, suitably aligned, for this driver.
        let data = unsafe {
            let data = (*card).private_data.cast::<CardData>();
            data.write(CardData {
                chip: core::ptr::from_ref(chip),
                substream: Atomic::new(core::ptr::null_mut()),
            });
            if irq >= 0 {
                (*card).sync_irq = irq;
            }
            data
        };
        // Before registration, so the IRQ handler can signal ALSA as soon as a stream runs.
        chip.set_alsa(data.cast());

        // SAFETY: `card` is valid; `NAME` is NUL-terminated.
        to_result(unsafe { sys::snd_pcm_new(card, NAME.as_char_ptr().cast(), 0, 1, 0, &mut pcm) })?;

        // SAFETY: `pcm` was just created on `card`; `PCM_OPS` is a static; the device is bound.
        unsafe {
            (*pcm).private_data = data.cast();
            set_str(&mut (*pcm).name, model.to_bytes());
            sys::snd_pcm_set_ops(pcm, STREAM_PLAYBACK, &PCM_OPS);
            // The host ring: coherent, 32-bit addressable, read by the DSP.
            to_result(sys::snd_pcm_set_managed_buffer_all(
                pcm,
                sys::SNDRV_DMA_TYPE_DEV as c_int,
                raw_device(dev),
                RING_BYTES as usize,
                RING_BYTES as usize,
            ))?;

            let c = &mut *card;
            set_str(&mut c.driver, NAME.to_bytes());
            set_str(&mut c.shortname, model.to_bytes());
            let model = core::str::from_utf8(model.to_bytes()).unwrap_or("MOTU");
            let pci = core::str::from_utf8(dev.name().to_bytes()).unwrap_or("?");
            let _ = write!(
                FieldWriter {
                    field: &mut c.longname,
                    len: 0
                },
                "{model} at {pci}, irq {irq}"
            );

            to_result(sys::snd_card_register(card))?;
            dev_info!(dev, "{} registered as ALSA card {}\n", model, c.number);
        }

        NEXT_SLOT.store(slot + 1, Relaxed);
        Ok(Self { card })
    }

    /// Disconnects the card and waits until every file on it is closed, so no PCM callback
    /// runs afterwards. The card itself is freed by devres with the device.
    pub(crate) fn disconnect(&self) {
        // SAFETY: `self.card` is a registered card, freed only by devres after this.
        unsafe { sys::snd_card_disconnect_sync(self.card) };
    }
}

/// From the IRQ handler while playback runs: signal ALSA that a period has elapsed.
///
/// # Safety
///
/// `alsa` must be the pointer [`Card::new`] gave [`Chip::set_alsa`], whose card is still
/// allocated.
pub(crate) unsafe fn period_elapsed(alsa: *mut c_void) {
    // SAFETY: by the function's safety requirements.
    let data = unsafe { &*alsa.cast::<CardData>() };
    let substream = data.substream.load(Acquire);
    if !substream.is_null() {
        // SAFETY: the substream stays valid while open; close clears it first.
        unsafe { sys::snd_pcm_period_elapsed(substream) };
    }
}

/// The frame layout on the ring is not confirmed (docs/transport.md), so the format is raw
/// 32-bit words, `frame_bytes / 4` per frame; the channel count and the period size are pinned
/// in open().
const HW: sys::snd_pcm_hardware = sys::snd_pcm_hardware {
    info: sys::SNDRV_PCM_INFO_MMAP
        | sys::SNDRV_PCM_INFO_MMAP_VALID
        | sys::SNDRV_PCM_INFO_INTERLEAVED
        | sys::SNDRV_PCM_INFO_BLOCK_TRANSFER,
    formats: FMTBIT_S32_LE,
    rates: sys::SNDRV_PCM_RATE_48000,
    rate_min: RATE,
    rate_max: RATE,
    buffer_bytes_max: RING_BYTES as usize,
    period_bytes_min: 256,
    period_bytes_max: (RING_BYTES / RING_PERIODS) as usize,
    periods_min: RING_PERIODS,
    periods_max: RING_PERIODS,
    // SAFETY: `snd_pcm_hardware` is plain data; zero is valid for the remaining fields.
    ..unsafe { core::mem::zeroed() }
};

static PCM_OPS: sys::snd_pcm_ops = sys::snd_pcm_ops {
    open: Some(pcm_open),
    close: Some(pcm_close),
    prepare: Some(pcm_prepare),
    trigger: Some(pcm_trigger),
    sync_stop: Some(pcm_sync_stop),
    pointer: Some(pcm_pointer),
    // SAFETY: `snd_pcm_ops` is a table of optional function pointers; zero means "none".
    ..unsafe { core::mem::zeroed() }
};

/// # Safety
///
/// `ss` must be a substream of this driver's PCM, which ALSA passes to the callbacks below.
unsafe fn card_data<'a>(ss: *mut sys::snd_pcm_substream) -> &'a CardData {
    // SAFETY: ALSA copies `pcm->private_data`, the `CardData` set in `Card::new()`, into each
    // substream; the card is disconnected before the chip it points to goes away.
    unsafe { &*(*ss).private_data.cast::<CardData>() }
}

/// # Safety
///
/// As [`card_data`].
unsafe fn chip<'a>(ss: *mut sys::snd_pcm_substream) -> &'a Chip {
    // SAFETY: by the function's safety requirements, and `CardData::chip` outlives the card's
    // callbacks.
    unsafe { &*card_data(ss).chip }
}

unsafe extern "C" fn pcm_open(ss: *mut sys::snd_pcm_substream) -> c_int {
    // SAFETY: ALSA calls this with one of our substreams.
    let (data, c) = unsafe { (card_data(ss), chip(ss)) };
    if !c.running() || c.frame_bytes() == 0 {
        kernel::do_once_lite! {
            dev_warn!(
                c.device(),
                "streaming disabled: the host-ring frame size is not known yet (motu424.frame_bytes=)\n"
            )
        };
        return ENXIO.to_errno();
    }

    // SAFETY: an open substream has a runtime.
    let rt = unsafe { &mut *(*ss).runtime };
    rt.hw = HW;
    rt.hw.channels_min = c.frame_bytes() / 4;
    rt.hw.channels_max = c.frame_bytes() / 4;

    // The DSP fetches period_bytes per interrupt and wraps after RING_PERIODS periods: the
    // buffer must be exactly that.
    let pb = c.period_bytes();
    rt.hw.period_bytes_min = pb as usize;
    rt.hw.period_bytes_max = pb as usize;
    // SAFETY: `rt` is the substream's runtime, during open.
    let err = unsafe {
        sys::snd_pcm_hw_constraint_minmax(
            rt,
            sys::SNDRV_PCM_HW_PARAM_PERIOD_BYTES as sys::snd_pcm_hw_param_t,
            pb,
            pb,
        )
    };
    if err < 0 {
        return err;
    }

    data.substream.store(ss, Release);
    0
}

unsafe extern "C" fn pcm_close(ss: *mut sys::snd_pcm_substream) -> c_int {
    // SAFETY: ALSA calls this with one of our substreams.
    let (data, c) = unsafe { (card_data(ss), chip(ss)) };
    data.substream.store(core::ptr::null_mut(), Release);
    // ALSA has run sync_stop by now: back to the idle stream, if enabled.
    c.idle_start();
    0
}

unsafe extern "C" fn pcm_prepare(ss: *mut sys::snd_pcm_substream) -> c_int {
    // SAFETY: ALSA calls this with one of our substreams.
    let c = unsafe { chip(ss) };
    // The trigger is atomic: leave the idle stream here, where we can sleep.
    c.idle_stop();

    // Start the DSP on silence; ALSA fills the buffer before START.
    // SAFETY: a prepared substream has a runtime with its buffer allocated.
    unsafe {
        let rt = &*(*ss).runtime;
        sys::snd_pcm_format_set_silence(
            rt.format,
            rt.dma_area.cast(),
            (rt.buffer_size as c_uint).wrapping_mul(rt.channels),
        )
    }
}

unsafe extern "C" fn pcm_trigger(ss: *mut sys::snd_pcm_substream, cmd: c_int) -> c_int {
    // SAFETY: ALSA calls this with one of our substreams, which has a runtime.
    let (c, rt) = unsafe { (chip(ss), &*(*ss).runtime) };
    match cmd as u32 {
        sys::SNDRV_PCM_TRIGGER_START => {
            c.stream_start(rt.dma_addr);
            0
        }
        sys::SNDRV_PCM_TRIGGER_STOP => {
            c.stream_stop();
            0
        }
        _ => EINVAL.to_errno(),
    }
}

unsafe extern "C" fn pcm_sync_stop(ss: *mut sys::snd_pcm_substream) -> c_int {
    // SAFETY: ALSA calls this with one of our substreams.
    unsafe { chip(ss) }.sync_stop();
    0
}

unsafe extern "C" fn pcm_pointer(ss: *mut sys::snd_pcm_substream) -> sys::snd_pcm_uframes_t {
    // SAFETY: ALSA calls this with one of our substreams, which has a runtime.
    let (c, rt) = unsafe { (chip(ss), &*(*ss).runtime) };
    c.pointer(rt.dma_addr) as sys::snd_pcm_uframes_t
}

/// The sync IRQ number for the card: the IRQ of the device's first vector.
pub(crate) fn irq_of(pdev: *mut bindings::pci_dev) -> c_int {
    // SAFETY: `pdev` is the bound device, whose IRQ vector was allocated.
    unsafe { bindings::pci_irq_vector(pdev, 0) }
}
