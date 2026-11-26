// SPDX-License-Identifier: GPL-2.0-or-later

//! The boundary with `motu424_alsa.c`, which owns the ALSA card and the module parameters.
//!
//! The kernel has no Rust bindings for ALSA, so the sound card and its PCM device are registered
//! in C. The C side calls back into the `motu424_rs_pcm_*` functions below, passing the
//! [`Chip`] pointer it was given at creation; every register access stays in Rust.

use core::ffi::c_void;

use kernel::{
    device::{Bound, Device},
    error::to_result,
    ffi::{c_char, c_int, c_uint, c_ulong},
    prelude::*,
};

use crate::hw::{Chip, Params};

/// Mirrors `struct motu424_params` in `motu424_alsa.c`.
#[repr(C)]
struct RawParams {
    frame_bytes: c_uint,
    period_bytes: c_uint,
    irq_frames: c_uint,
    configure: bool,
    idle_stream: bool,
}

extern "C" {
    fn motu424_alsa_params(p: *mut RawParams);
    fn motu424_alsa_slot() -> c_int;
    fn motu424_alsa_create(
        dev: *mut c_void,
        chip: *const c_void,
        model: *const c_char,
        out: *mut *mut c_void,
    ) -> c_int;
    fn motu424_alsa_disconnect(alsa: *mut c_void);
    pub(crate) fn motu424_alsa_period_elapsed(alsa: *mut c_void);
}

/// The module parameters, as `motu424_alsa.c` declares them.
pub(crate) fn params() -> Params {
    let mut p = RawParams {
        frame_bytes: 0,
        period_bytes: 0,
        irq_frames: 0,
        configure: false,
        idle_stream: false,
    };
    // SAFETY: `p` is a valid, writable `struct motu424_params`.
    unsafe { motu424_alsa_params(&mut p) };
    Params {
        frame_bytes: p.frame_bytes,
        period_bytes: p.period_bytes,
        irq_frames: p.irq_frames,
        configure: p.configure,
        idle_stream: p.idle_stream,
    }
}

/// Checks the `enable` parameter for the next card: `ENODEV` past the last slot, `ENOENT` when
/// this card is disabled.
pub(crate) fn slot() -> Result {
    // SAFETY: no arguments; the C side only reads its parameter arrays.
    to_result(unsafe { motu424_alsa_slot() })
}

fn raw_device(dev: &Device<Bound>) -> *mut c_void {
    // `device::Device` is a `#[repr(transparent)]` wrapper of `Opaque<struct device>`, and its
    // `as_raw()` is crate-private. Passed as `void *`: `struct device` is not FFI-safe in Rust's
    // eyes (it has zero-sized members).
    core::ptr::from_ref(dev).cast_mut().cast()
}

/// The registered ALSA card of one PCI-424.
pub(crate) struct Card(*mut c_void);

// SAFETY: the C side only ever touches the card through ALSA, which does its own locking.
unsafe impl Send for Card {}
// SAFETY: as above; `Card` has no `&self` methods besides `disconnect()`, which is called once.
unsafe impl Sync for Card {}

impl Card {
    /// Creates and registers the card. `chip` must outlive the card's callbacks: the driver
    /// calls [`Card::disconnect`] before dropping it.
    pub(crate) fn new(chip: &Chip, model: &CStr) -> Result<Self> {
        let mut out = core::ptr::null_mut();
        // SAFETY: the device is bound, `model` is NUL-terminated and copied by the callee, and
        // `chip` stays valid until `disconnect()`, by the driver's drop order.
        to_result(unsafe {
            motu424_alsa_create(
                raw_device(chip.device()),
                core::ptr::from_ref(chip).cast(),
                model.as_char_ptr(),
                &mut out,
            )
        })?;
        chip.set_alsa(out);
        Ok(Self(out))
    }

    /// Disconnects the card and waits until every file on it is closed, so no PCM callback runs
    /// afterwards. The card itself is freed by devres with the device.
    pub(crate) fn disconnect(&self) {
        // SAFETY: `self.0` came from `motu424_alsa_create()` and the card is still allocated.
        unsafe { motu424_alsa_disconnect(self.0) };
    }
}

/// # Safety
///
/// `chip` must be the pointer given to `motu424_alsa_create()`, still valid, as it is until the
/// card has been disconnected.
unsafe fn chip<'a>(chip: *const c_void) -> &'a Chip {
    // SAFETY: by the function's safety requirements.
    unsafe { &*chip.cast::<Chip>() }
}

/// PCM open: the channel count and period size the PCM must be pinned to, or `-ENXIO` while
/// streaming is disabled.
///
/// # Safety
///
/// See [`chip`]; `channels` and `period_bytes` must be writable.
#[no_mangle]
unsafe extern "C" fn motu424_rs_pcm_open(
    c: *const c_void,
    channels: *mut c_uint,
    period_bytes: *mut c_uint,
) -> c_int {
    // SAFETY: by the function's safety requirements.
    let c = unsafe { chip(c) };
    if !c.running() || c.frame_bytes() == 0 {
        kernel::do_once_lite! {
            dev_warn!(
                c.device(),
                "streaming disabled: the host-ring frame size is not known yet (motu424.frame_bytes=)\n"
            )
        };
        return ENXIO.to_errno();
    }
    // SAFETY: by the function's safety requirements.
    unsafe {
        *channels = c.frame_bytes() / 4;
        *period_bytes = c.period_bytes();
    }
    0
}

/// PCM close: ALSA has run sync_stop by now; back to the idle stream, if enabled.
///
/// # Safety
///
/// See [`chip`].
#[no_mangle]
unsafe extern "C" fn motu424_rs_pcm_close(c: *const c_void) {
    // SAFETY: by the function's safety requirements.
    unsafe { chip(c) }.idle_start();
}

/// PCM prepare: leave the idle stream here, where we can sleep (the trigger is atomic).
///
/// # Safety
///
/// See [`chip`].
#[no_mangle]
unsafe extern "C" fn motu424_rs_pcm_prepare(c: *const c_void) {
    // SAFETY: by the function's safety requirements.
    unsafe { chip(c) }.idle_stop();
}

/// Trigger START, from atomic context.
///
/// # Safety
///
/// See [`chip`].
#[no_mangle]
unsafe extern "C" fn motu424_rs_pcm_start(c: *const c_void, ring: u64) {
    // SAFETY: by the function's safety requirements.
    unsafe { chip(c) }.stream_start(ring);
}

/// Trigger STOP, from atomic context.
///
/// # Safety
///
/// See [`chip`].
#[no_mangle]
unsafe extern "C" fn motu424_rs_pcm_stop(c: *const c_void) {
    // SAFETY: by the function's safety requirements.
    unsafe { chip(c) }.stream_stop();
}

/// PCM sync_stop: wait for the DSP to acknowledge the stop; sleeps.
///
/// # Safety
///
/// See [`chip`].
#[no_mangle]
unsafe extern "C" fn motu424_rs_pcm_sync_stop(c: *const c_void) {
    // SAFETY: by the function's safety requirements.
    unsafe { chip(c) }.sync_stop();
}

/// PCM pointer, in frames, from atomic context.
///
/// # Safety
///
/// See [`chip`].
#[no_mangle]
unsafe extern "C" fn motu424_rs_pcm_pointer(c: *const c_void, ring: u64) -> c_ulong {
    // SAFETY: by the function's safety requirements.
    unsafe { chip(c) }.pointer(ring) as c_ulong
}
