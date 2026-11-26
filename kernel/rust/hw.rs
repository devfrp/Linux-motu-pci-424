// SPDX-License-Identifier: GPL-2.0-or-later

//! The hardware layer: the only file that encodes real MOTU PCI-324/424 semantics.
//!
//! The model implemented here (docs/fpga-upload.md, docs/transport.md, docs/register-map.md) was
//! recovered from MOTUAW.sys and the card's DSP program and checked on a real PCI-424 (issue #2):
//!
//! - the card is a TMS320C6412 DSP in PCI boot mode plus an FPGA with no config PROM. At every
//!   start the host warm-resets the DSP, clocks the FPGA image out through the DSP's GPIO, writes
//!   the DSP program into internal RAM, releases the CPU and reads `audio_base` from a mailbox;
//! - audio is DSP bus-master DMA: the host hands the DSP a coherent ring, the DSP raises one
//!   interrupt per period (ack: RSTSRC INTRST), wraps after `RING_PERIODS` periods and reports its
//!   position as a host bus address at `audio_base + 0x2c`;
//! - the device configure (issue #2, 2026-10-20) is the vendor's 48 kHz parameter group at load,
//!   an optional mixer program handed over by the relock sequence, and the channel map at every
//!   stream start.
//!
//! Only the PCI-424 (137a:0004) load path is implemented. This is a port of the C driver's
//! `motu424_hw.c`, which has run on a card; the comments name the vendor routines each step
//! follows.

use core::ffi::c_void;

use kernel::{
    bindings,
    device::{Bound, Device},
    devres::Devres,
    dma::{Coherent, CoherentBox},
    firmware::Firmware,
    io::{poll::read_poll_timeout, Io as _},
    irq::{self, IrqReturn},
    pci,
    prelude::*,
    sync::{
        aref::ARef,
        atomic::{Acquire, Atomic, Relaxed, Release},
        new_spinlock, SpinLock,
    },
    time::{delay::fsleep, Delta},
};

use crate::{alsa, sha256::sha256};

// --- Windowed card-address space (vendor accessors 0x29110/0x29160) ---
//
// A 32-bit card address is a TMS320C6412 address, dispatched onto one of two MMIO windows:
// addresses tagged 0x018xxxxx go to window A (8 MB, DSP peripheral registers), everything else
// to window B (4 MB, the DSP memory page set by DSPP). With DSPP = 0, window B offsets
// 0..0x3ffff are the DSP's internal RAM; the rest of that page is reserved and never touched.

const WINA_TAG: u32 = 0x0180_0000;
const WINA_TAG_MASK: u32 = 0xff80_0000;
const WINA_MASK: u32 = 0x007f_ffff;
const WINB_MASK: u32 = 0x003f_ffff;
pub(crate) const WINA_LEN: u64 = 0x0080_0000;
pub(crate) const WINB_LEN: u64 = 0x0040_0000;
const RAM_LEN: u32 = 0x0004_0000;

// The only DSP peripheral registers the driver writes (SPRS219J / SPRU581C / SPRU234C). Every
// other window-A write is refused by `wr32()`, which keeps the DSP EEPROM controller (card
// 0x01c20000, the card's PCI identity) out of reach by construction.
const EMIF_CECTL0: u32 = 0x0180_0008;
const GPIO_GPEN: u32 = 0x01b0_0000;
const GPIO_GPDIR: u32 = 0x01b0_0004;
const GPIO_GPVAL: u32 = 0x01b0_0008;
const PCI_RSTSRC: u32 = 0x01c0_0000;

const CECTL0_INIT: u32 = 0x1091_4221; // 32-bit async CE0, vendor value
const RSTSRC_INTRST: u32 = 0x10; // bit 4: clear the DSP's INTREQ

// FPGA serial-configuration pins on the DSP GPIO (vendor fn 0x251e0).
const GP_DATA0: u32 = 1 << 5;
const GP_NCONFIG: u32 = 1 << 6;
const GP_DCLK: u32 = 1 << 7;

// I/O-port BAR: the C6412 Host Port Interface registers (SPRU581C).
const PORT_HSR: usize = 0x0;
const PORT_IRQ_PENDING: u32 = 1 << 1;
const PORT_HSR_INTAM: u32 = 1 << 2; // 1 = mask PINTA#, 0 = unmask
const PORT_HDCR: usize = 0x4;
const PORT_HDCR_WARMRESET: u32 = 1 << 0;
const PORT_HDCR_DSPINT: u32 = 1 << 1;
const PORT_DSPP: usize = 0x8;
const PORT_LEN: usize = 0xc;

// --- Host-loaded firmware, checked before any byte reaches the card ---
//
// Both images are MOTU's and are not shipped: the user extracts them from their own MOTUAW.sys
// with extract-firmware (tools/re/, built by `make tools`) and installs them under
// /lib/firmware/motu424/.

const FW_FPGA: &CStr = c"motu424/pci424-serial-container.bin";
const FW_FPGA_LEN: usize = 37177;
const FW_PROG: &CStr = c"motu424/pci424-program.bin";
const FW_PROG_LEN: usize = 28272;

const FPGA_SHA256: [u8; 32] = [
    0x34, 0xe6, 0x2c, 0xf5, 0xa7, 0x5f, 0x53, 0xa4, 0x10, 0x04, 0x12, 0x9f, 0xf1, 0xfe, 0x6d, 0xc4,
    0x35, 0xa8, 0x7e, 0x14, 0x61, 0x4d, 0x79, 0xed, 0xc6, 0xce, 0x39, 0x16, 0xe1, 0x8e, 0x04, 0xe2,
];

const PROG_SHA256: [u8; 32] = [
    0x3a, 0x23, 0xbc, 0x97, 0xe4, 0x91, 0x54, 0xe5, 0xeb, 0xa8, 0x95, 0x66, 0x81, 0xee, 0x8d, 0xd0,
    0x21, 0x2c, 0xd7, 0x09, 0x1e, 0x8d, 0x67, 0x7c, 0x27, 0x2c, 0xcb, 0x1a, 0x7f, 0xaa, 0x6e, 0x4f,
];

// Optional CueMix mixer program for the DSP (docs/transport.md). Generated routing data, so only
// its size and trailer are checked.
const FW_MIXER: &CStr = c"motu424/pci424-mixer.bin";
const MIXPROG_LEN: usize = 0x2800; // each DSP-side program buffer
const MIXPROG_TRAILER: [u32; 3] = [0x001c_00e2, 0x0798_03e2, 0x0000_8000];

/// The DSP program publishes `audio_base` in the last dword of internal RAM.
const MAILBOX: u32 = 0x0003_fffc;

// --- Audio block: the DSP program's global data, at audio_base (0x6fac with this image) ---

const AREG_MIXBASE: u32 = 0x00; // R: CueMix coefficient block (45 dwords)
const AREG_MIXPROG_BUF: u32 = 0x0c; // R: two DSP mixer-program buffers (+0x10)
const AREG_CHMAP: u32 = 0x14; // R: address of the 24-dword channel map
const AREG_DMAPOINT: u32 = 0x2c; // R: DSP's current host bus address
const AREG_FW_K1: u32 = 0x44; // R: firmware constant, parameter group
const AREG_FW_K2: u32 = 0x48; // R: firmware constant, parameter group
const AREG_ENABLE_INIT: u32 = 0x50; // W 1: vendor init, end of bring-up
const AREG_ENABLE: u32 = 0x54; // W: 1 = start, 0 = stop the stream
const AREG_CFG_C: u32 = 0x58; // W: configure arg c (0 on Windows)
const AREG_RING_BASE: u32 = 0x5c; // W: host ring bus address
const AREG_IRQ_FRAMES: u32 = 0x60; // W: frames per interrupt (host side)
const AREG_RING_PERIODS: u32 = 0x64; // W: periods per ring wrap
const AREG_XFER_A: u32 = 0x68; // W: fetch geometry, 0x30-byte units
const AREG_XFER_B: u32 = 0x6c; // W: fetch geometry
const AREG_RELOCK_W: u32 = 0x70; // W: relock word (q << 16 | -q)
const AREG_COPY_ARM: u32 = 0x74; // W: 1 while the host copy runs
const AREG_MIXPROG: u32 = 0x78; // W: mixer program the DSP runs
const AREG_RING_SIZE: u32 = 0x8c; // W: host ring size code (bytes)
const AREG_STOP_ACK: u32 = 0xf4; // W 0 before stop; DSP sets !0 when done
const AREG_COPY_SRC: u32 = 0xf8; // W: host bus address to copy from
const AREG_COPY_DST: u32 = 0xfc; // W: DSP address to copy to
const AREG_COPY_LEN: u32 = 0x100; // W: bytes; the DSP zeroes it when done
const AREG_MIXPROG_SWAP: u32 = 0x104; // W 1: switch to +0x78; DSP clears it
const AREG_MIXPROG_READY: u32 = 0x108; // R: DSP sets !0 when a swap is safe

const MIX_DWORDS: u32 = 45;
const CHMAP_DWORDS: usize = 24;

/// What the program's own init leaves in +0x44/+0x48 (bring-up run 1).
const FW_K1_VAL: u32 = 0x03a8_9000;
const FW_K2_VAL: u32 = 0x0009_c180;

/// +0x70 at 1x: q = 0x10000 / (1 * 0x1e0) = 0x88, (q << 16) | (u16)-q.
const RELOCK_W_1X: u32 = 0x0088_ff78;

// --- Host ring (docs/transport.md) ---
//
// One coherent host buffer of 512 KB, as on Windows. The DSP fetches one period per interrupt
// and wraps back to +0x5c after RING_PERIODS periods. Only the 1x / 48 kHz state the program
// boots into is used.

pub(crate) const RING_BYTES: u32 = 0x8_0000;
pub(crate) const RING_PERIODS: u32 = 8;
const IRQ_FRAMES_1X: u32 = 0x40;
const IRQ_FRAMES_MAX: u32 = 0x400;
const XFER_A: u32 = 0xe;
const XFER_B: u32 = 0x10;
const CFG_C: u32 = 0;

// The idle zero-channel stream the vendor starts at load (issue #2, 2026-11-13).
const IDLE_CFG_C: u32 = 1;
const IDLE_PERIODS: u32 = 4;
const IDLE_IRQ_FRAMES: u32 = 0x100;

/// One audio-block write of a parameter-group list.
type Wr = (u32, u32);

// The vendor's parameter group for 48 kHz at 1x (bring-up step 16, hw slot 0x30:
// 0x2a820/0x2a710/0x2a9e0/0x2aab0) with its constructor defaults: internal clock, latency 0,
// P = Q = 3000. K1/K2 are the firmware constants at +0x44/+0x48. Committed by +0x114, which DSP
// routine 0x45e0 consumes.
const CFG_HEAD: [Wr; 3] = [(0x88, 1), (0x8c, RING_BYTES), (0x90, 0)];

/// Written twice, before and after the +0x98 rate word.
const CFG_RATE: [Wr; 14] = [
    (0xc4, 0),          // latency: ms * rate / 1000
    (0xc8, 0),          //
    (0x9c, 0x1e),       // (Q + 50) / 100
    (0xa4, 0x4650),     // 6Q
    (0xa0, 0x708),      // 6Q / 10
    (0xb0, 0x1e),       // Q / 100
    (0xb4, 0xcccd),     // (8P * 2^20 / rate + 5) / 10
    (0xb8, 0x492b4000), // K1 * rate * 10 / 8P
    (0xbc, 0x640),      // rate * 100 / P
    (0xc0, 0),          //
    (0xe0, 0x9c180),    // K2 (halved at 2x/4x)
    (0xe4, 0xc31e00),   // (K2 * rate * 10 + 4P) / 8P
    (0xf0, 0x223),      // 0x40000 / (K2 * rate / 64e6)
    (0x114, 1),         // commit
];

const CFG_TAIL: [Wr; 8] = [
    (0x84, 0),      // internal clock
    (0xa8, 0),      //
    (0x114, 1),     // commit again
    (0x118, 1),     // the DSP clears it
    (0xcc, 0),      // clock-source words
    (0xd0, 0),      //
    (0xd4, 0),      //
    (0xdc, 0x7fff), // restored user setting
];

const RATE_WORD_48K: u32 = 0x0535_5555; // +0x98, as the firmware boots

/// Channel map at [+0x14], as a Windows-configured card with a 2408mk3 and a 24I/O held it: the
/// 14 (= +0x68) playback channels 24..37, one byte each as ch + ch / 24, at byte index i ^ 3.
const CHMAP: [u32; CHMAP_DWORDS] = {
    let mut m = [0u32; CHMAP_DWORDS];
    m[0] = 0x191a_1b1c;
    m[1] = 0x1d1e_1f20;
    m[2] = 0x2122_2324;
    m[3] = 0x2526_0000;
    m
};

fn is_wina(card: u32) -> bool {
    card & WINA_TAG_MASK == WINA_TAG
}

/// Window B is only DSP internal RAM here (DSPP = 0); the rest is reserved.
fn in_ram(card: u32, len: u32) -> bool {
    card & 3 == 0 && card < RAM_LEN && len <= RAM_LEN - card
}

/// Allow-list for writes: DSP internal RAM in window B and, in window A, only the five
/// peripheral registers the driver needs.
fn write_allowed(card: u32) -> bool {
    if !is_wina(card) {
        return in_ram(card, 4);
    }
    matches!(
        card,
        EMIF_CECTL0 | GPIO_GPEN | GPIO_GPDIR | GPIO_GPVAL | PCI_RSTSRC
    )
}

/// Module parameters, read once at probe from the C side (Rust parameters are integers only).
#[derive(Clone, Copy)]
pub(crate) struct Params {
    pub(crate) frame_bytes: u32,
    pub(crate) period_bytes: u32,
    pub(crate) irq_frames: u32,
    pub(crate) configure: bool,
    pub(crate) idle_stream: bool,
}

/// The three BARs, assigned to their hardware roles by type and size.
pub(crate) struct Bars {
    pub(crate) win_a: Devres<pci::Bar<'static>>,
    pub(crate) win_b: Devres<pci::Bar<'static>>,
    pub(crate) port: Devres<pci::Bar<'static>>,
}

/// Register access for one operation, borrowed from the [`Bars`] while the device is bound.
pub(crate) struct Io<'a> {
    dev: &'a Device<Bound>,
    win_a: &'a pci::Bar<'static>,
    win_b: &'a pci::Bar<'static>,
    port: &'a pci::Bar<'static>,
}

impl<'a> Io<'a> {
    pub(crate) fn new(bars: &'a Bars, dev: &'a Device<Bound>) -> Result<Self> {
        let port = bars.port.access(dev)?;
        if port.maxsize() < PORT_LEN {
            return Err(ENODEV);
        }
        Ok(Self {
            dev,
            win_a: bars.win_a.access(dev)?,
            win_b: bars.win_b.access(dev)?,
            port,
        })
    }

    fn rd32(&self, card: u32) -> u32 {
        let r = if is_wina(card) {
            self.win_a.try_read32((card & WINA_MASK) as usize)
        } else if in_ram(card, 4) {
            self.win_b.try_read32((card & WINB_MASK) as usize)
        } else {
            Err(EINVAL)
        };
        r.unwrap_or_else(|_| {
            kernel::do_once_lite! {
                dev_warn!(self.dev, "refusing read of reserved card address {:#010x}\n", card)
            };
            !0
        })
    }

    fn wr32(&self, card: u32, val: u32) {
        let r = if !write_allowed(card) {
            Err(EINVAL)
        } else if is_wina(card) {
            self.win_a.try_write32(val, (card & WINA_MASK) as usize)
        } else {
            self.win_b.try_write32(val, (card & WINB_MASK) as usize)
        };
        if r.is_err() {
            kernel::do_once_lite! {
                dev_warn!(
                    self.dev,
                    "refusing write to card address {:#010x} (not on the allow-list)\n",
                    card
                )
            };
        }
    }

    // Card addresses wrap like the C ones; `rd32()`/`wr32()` refuse anything outside RAM.

    fn awr(&self, audio_base: u32, off: u32, val: u32) {
        self.wr32(audio_base.wrapping_add(off), val);
    }

    fn ard(&self, audio_base: u32, off: u32) -> u32 {
        self.rd32(audio_base.wrapping_add(off))
    }

    fn awr_list(&self, audio_base: u32, list: &[Wr]) {
        for &(off, val) in list {
            self.awr(audio_base, off, val);
        }
    }

    // The HPI registers sit in an I/O-port BAR. `pci::Bar` maps it with `pci_iomap()`, but its
    // accessors are MMIO ones (`readl`/`writel`), which cannot reach a port cookie, so these use
    // `ioread32()`/`iowrite32()` on the mapping's address. `PORT_LEN` was checked in `new()`.

    fn port_wr(&self, off: usize, val: u32) {
        // SAFETY: `off + 4 <= PORT_LEN <= maxsize()`, so the address is inside the BAR that
        // `pci_iomap()` mapped, which stays mapped while `self.port` is borrowed.
        unsafe { bindings::iowrite32(val, (self.port.addr() + off) as *mut c_void) }
    }

    fn port_rd(&self, off: usize) -> u32 {
        // SAFETY: as in `port_wr()`.
        unsafe { bindings::ioread32((self.port.addr() + off) as *const c_void) }
    }
}

fn cond_resched() {
    // SAFETY: `__cond_resched()` may be called from any sleepable context, which the load
    // sequence is (it runs from probe).
    unsafe { bindings::__cond_resched() };
}

fn fw_get(dev: &Device<Bound>, name: &CStr, len: usize, sha: &[u8; 32]) -> Result<Firmware> {
    let fw = Firmware::request(name, dev).inspect_err(|e| {
        dev_err!(
            dev,
            "cannot load {} ({}): extract it from your own MOTUAW.sys with extract-firmware (make tools) and install it under /lib/firmware/motu424/\n",
            name,
            e.to_errno()
        )
    })?;
    if fw.size() != len {
        dev_err!(dev, "{}: {} bytes, expected {}\n", name, fw.size(), len);
        return Err(EINVAL);
    }
    if sha256(fw.data()) != *sha {
        dev_err!(dev, "{}: SHA-256 mismatch, not the expected image\n", name);
        return Err(EINVAL);
    }
    Ok(fw)
}

fn le32(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

/// Step 2: clock the FPGA image out through the DSP GPIO (vendor fn 0x251e0, 0x25170, 0x25230):
/// nCONFIG low then high, then per byte LSB first one DCLK pulse per bit with DATA0 stable, then
/// 1000 trailing clocks. The vendor reads no nSTATUS/CONF_DONE; whether the FPGA took the image
/// shows in step 3, since window B only decodes once it is configured.
fn fpga_load(io: &Io<'_>, img: &[u8]) {
    let pins = GP_DATA0 | GP_NCONFIG | GP_DCLK;

    io.wr32(GPIO_GPEN, pins);
    io.wr32(GPIO_GPDIR, pins);
    io.wr32(GPIO_GPVAL, 0);
    io.rd32(GPIO_GPVAL); // flush posted writes
    fsleep(Delta::from_micros(100));
    io.wr32(GPIO_GPVAL, GP_NCONFIG);
    io.rd32(GPIO_GPVAL);
    fsleep(Delta::from_micros(5000));

    for &byte in img {
        for bit in 0..8 {
            let v = GP_NCONFIG | if (byte >> bit) & 1 != 0 { GP_DATA0 } else { 0 };
            io.wr32(GPIO_GPVAL, v);
            io.wr32(GPIO_GPVAL, v | GP_DCLK);
            io.wr32(GPIO_GPVAL, v);
        }
        cond_resched();
    }
    for _ in 0..1000 {
        io.wr32(GPIO_GPVAL, GP_NCONFIG | GP_DCLK);
        io.wr32(GPIO_GPVAL, GP_NCONFIG);
    }
    io.wr32(GPIO_GPDIR, GP_NCONFIG | GP_DCLK);
}

/// Step 3: zero DSP internal RAM and write the program at 0, then read it back. The vendor
/// writes (len >> 2) + 1 dwords from a zeroed buffer; the extra dword is already zero after the
/// fill.
fn prog_load(io: &Io<'_>, prog: &[u8]) -> Result {
    io.port_wr(PORT_DSPP, 0);
    for off in (0..RAM_LEN).step_by(4) {
        io.wr32(off, 0);
        if off & 0xfff == 0 {
            cond_resched();
        }
    }
    let words = prog.len() / 4;
    for i in 0..words {
        io.wr32(4 * i as u32, le32(prog, 4 * i));
    }

    let bad = (0..words)
        .filter(|&i| io.rd32(4 * i as u32) != le32(prog, 4 * i))
        .count();
    if bad != 0 {
        dev_err!(
            io.dev,
            "DSP program read-back: {} of {} dwords differ (FPGA not configured?)\n",
            bad,
            words
        );
        return Err(EIO);
    }
    Ok(())
}

/// What the DSP program published once it runs.
#[derive(Clone, Copy)]
pub(crate) struct Published {
    audio_base: u32,
    mix_base: u32,
    chmap_base: u32,
}

/// Step 4: clear the mailbox, release the CPU with DSPINT and wait for the program to publish
/// `audio_base` there (the card did so on the first poll).
fn dsp_start(io: &Io<'_>) -> Result<Published> {
    let dev = io.dev;

    io.wr32(MAILBOX, 0);
    if io.rd32(MAILBOX) != 0 {
        dev_err!(dev, "mailbox did not clear\n");
        return Err(EIO);
    }
    io.port_wr(PORT_HDCR, PORT_HDCR_DSPINT);

    let ab = read_poll_timeout(
        || Ok(io.rd32(MAILBOX)),
        |v| *v != 0,
        Delta::from_micros(10),
        Delta::from_secs(3),
    )
    .inspect_err(|_| dev_err!(dev, "DSP did not publish audio_base\n"))?;
    if !in_ram(ab, 0x140) {
        dev_err!(dev, "DSP published an invalid audio_base {:#010x}\n", ab);
        return Err(EIO);
    }

    let mix_base = io.ard(ab, AREG_MIXBASE);
    let chmap_base = io.ard(ab, AREG_CHMAP);
    if !in_ram(mix_base, MIX_DWORDS * 4) || !in_ram(chmap_base, CHMAP_DWORDS as u32 * 4) {
        dev_err!(
            dev,
            "DSP published an invalid mix_base {:#010x} or channel map {:#010x}\n",
            mix_base,
            chmap_base
        );
        return Err(EIO);
    }
    Ok(Published {
        audio_base: ab,
        mix_base,
        chmap_base,
    })
}

/// The vendor's parameter group for 48 kHz (bring-up step 16), in its order. Half the values
/// derive from +0x44/+0x48, so nothing is written unless the program left the constants they were
/// computed from.
fn dsp_configure(io: &Io<'_>, ab: u32) {
    let k1 = io.ard(ab, AREG_FW_K1);
    let k2 = io.ard(ab, AREG_FW_K2);

    if k1 != FW_K1_VAL || k2 != FW_K2_VAL {
        dev_warn!(
            io.dev,
            "+0x44/+0x48 = {:#x}/{:#x}, expected {:#x}/{:#x}: parameter group not written\n",
            k1,
            k2,
            FW_K1_VAL,
            FW_K2_VAL
        );
        return;
    }
    io.awr_list(ab, &CFG_HEAD);
    io.awr_list(ab, &CFG_RATE);
    io.awr(ab, 0x98, RATE_WORD_48K);
    io.awr_list(ab, &CFG_RATE);
    io.awr_list(ab, &CFG_TAIL);
    dev_info!(io.dev, "48 kHz parameter group written\n");
}

/// Step 5, the rest of the vendor init (fn 0x2c150): zero the mixer block, the parameter group,
/// audio_base+0x50 <- 1, then the EMIF CE0 timing the DSP needs to reach the FPGA (the value a
/// Windows-configured card reads back in CECTL0/CECTL1).
fn dsp_init(io: &Io<'_>, p: &Published, configure: bool) {
    for i in 0..MIX_DWORDS {
        io.wr32(p.mix_base + 4 * i, 0);
    }
    if configure {
        dsp_configure(io, p.audio_base);
    }
    io.awr(p.audio_base, AREG_ENABLE_INIT, 1);
    io.wr32(EMIF_CECTL0, CECTL0_INIT);
}

/// The optional mixer program: a multiple of 4 bytes that fits a DSP program buffer and ends with
/// the vendor's trailer. It goes into a coherent buffer now, while the DSP cannot master the bus
/// yet; [`Chip::relock`] hands it over.
fn mixer_get(dev: &Device<Bound>) -> Option<(Coherent<[u8]>, usize)> {
    let Ok(fw) = Firmware::request_nowarn(FW_MIXER, dev) else {
        dev_info!(
            dev,
            "no {}: the DSP runs without a mixer program\n",
            FW_MIXER
        );
        return None;
    };
    let data = fw.data();
    let n = data.len();
    let tlen = MIXPROG_TRAILER.len() * 4;
    let valid = n.is_multiple_of(4)
        && n >= tlen
        && n <= MIXPROG_LEN
        && MIXPROG_TRAILER
            .iter()
            .enumerate()
            .all(|(i, &w)| le32(data, n - tlen + 4 * i) == w);
    if !valid {
        dev_warn!(
            dev,
            "{}: {} bytes, not a mixer program (multiple of 4, at most {:#x}, ending with the CueMix trailer); ignored\n",
            FW_MIXER,
            n,
            MIXPROG_LEN
        );
        return None;
    }

    let mut buf = CoherentBox::<[u8]>::zeroed_slice(dev, MIXPROG_LEN, GFP_KERNEL).ok()?;
    buf[..n].copy_from_slice(data);
    Some((buf.into(), n))
}

/// Load and start the card: the vendor DEV_0004 sequence (docs/fpga-upload.md), as run on
/// hardware by tools/motu424-bringup. Sleeps; called from probe before bus mastering and the
/// IRQ, with PINTA# masked throughout.
pub(crate) fn boot(io: &Io<'_>, params: Params) -> Result<Boot> {
    let dev = io.dev;

    let fpga = fw_get(dev, FW_FPGA, FW_FPGA_LEN, &FPGA_SHA256)?;
    let prog = fw_get(dev, FW_PROG, FW_PROG_LEN, &PROG_SHA256)?;

    io.port_wr(PORT_HSR, PORT_HSR_INTAM);
    io.port_wr(PORT_HDCR, PORT_HDCR_WARMRESET);
    fsleep(Delta::from_micros(1000));

    fpga_load(io, fpga.data());
    let published = prog_load(io, prog.data())
        .and_then(|()| dsp_start(io))
        .inspect_err(|_| io.port_wr(PORT_HDCR, PORT_HDCR_WARMRESET))?;
    dsp_init(io, &published, params.configure);
    let mixer = mixer_get(dev);

    // The idle stream's ring. Zeroed: with no channels the DSP still fetches 0x600-byte periods
    // from it (measured on a card), and finds silence.
    let idle_ring = if params.idle_stream {
        let ring = Coherent::<u8>::zeroed_slice(dev, RING_BYTES as usize, GFP_KERNEL);
        if ring.is_err() {
            dev_warn!(
                dev,
                "no memory for the idle-stream ring; idle stream disabled\n"
            );
        }
        ring.ok()
    } else {
        None
    };

    let mut p = params;
    if p.irq_frames == 0 || p.irq_frames > IRQ_FRAMES_MAX {
        dev_warn!(
            dev,
            "irq_frames={:#x} out of range (1..{:#x}); using {:#x}\n",
            p.irq_frames,
            IRQ_FRAMES_MAX,
            IRQ_FRAMES_1X
        );
        p.irq_frames = IRQ_FRAMES_1X;
    }
    let max_period = RING_BYTES / RING_PERIODS;
    if p.frame_bytes != 0
        && (!p.frame_bytes.is_multiple_of(4)
            || p.period_bytes == 0
            || !p.period_bytes.is_multiple_of(p.frame_bytes)
            || p.period_bytes > max_period)
    {
        dev_warn!(
            dev,
            "frame_bytes={} must be a multiple of 4 dividing period_bytes={:#x} (at most {:#x}); streaming disabled\n",
            p.frame_bytes,
            p.period_bytes,
            max_period
        );
        p.frame_bytes = 0;
    }
    let note = if p.frame_bytes != 0 {
        ""
    } else {
        "; streaming disabled until frame_bytes is set"
    };
    dev_info!(
        dev,
        "DSP running: audio_base={:#x} mix_base={:#x}{}\n",
        published.audio_base,
        published.mix_base,
        note
    );

    Ok(Boot {
        published,
        params: p,
        mixer,
        idle_ring,
    })
}

/// Everything [`boot`] leaves for the running driver.
pub(crate) struct Boot {
    published: Published,
    params: Params,
    mixer: Option<(Coherent<[u8]>, usize)>,
    idle_ring: Option<Coherent<[u8]>>,
}

/// Stream state guarded by [`Chip::lock`].
struct State {
    idle_running: bool,
    mixer_idx: u32,
}

/// The running card, shared by the driver data, the IRQ handler and the ALSA callbacks.
///
/// Every reference to a `Chip` is owned by the driver's bound data (directly or through its IRQ
/// registration), and the ALSA card is disconnected before that data is dropped, so a `Chip`
/// only ever exists while the PCI device is bound to this driver.
#[pin_data]
pub(crate) struct Chip {
    dev: ARef<Device>,
    bars: Bars,
    published: Published,
    params: Params,
    /// `audio_base`, cleared by [`Chip::shutdown`]: 0 = DSP not running.
    audio_base: Atomic<u32>,
    mixer: Option<(Coherent<[u8]>, usize)>,
    idle_ring: Option<Coherent<[u8]>>,
    /// Guards the register sequences of start, stop, idle stream and shutdown. Never taken in
    /// the IRQ handler, which only acks and reads `playing`.
    #[pin]
    lock: SpinLock<State>,
    playing: Atomic<bool>,
    /// The ALSA side, for `snd_pcm_period_elapsed()`; null until the card exists.
    alsa: Atomic<*mut c_void>,
}

impl Chip {
    pub(crate) fn new(dev: &Device<Bound>, bars: Bars, boot: Boot) -> impl PinInit<Self> {
        let dev: ARef<Device> = dev.into();
        pin_init!(Self {
            dev,
            bars,
            published: boot.published,
            params: boot.params,
            audio_base: Atomic::new(boot.published.audio_base),
            mixer: boot.mixer,
            idle_ring: boot.idle_ring,
            lock <- new_spinlock!(State {
                idle_running: false,
                mixer_idx: 0
            }),
            playing: Atomic::new(false),
            alsa: Atomic::new(core::ptr::null_mut()),
        })
    }

    /// The bound device.
    pub(crate) fn device(&self) -> &Device<Bound> {
        // SAFETY: by the type invariant, a `Chip` only exists while the device is bound.
        unsafe { self.dev.as_bound() }
    }

    fn io(&self) -> Result<Io<'_>> {
        Io::new(&self.bars, self.device())
    }

    fn ab(&self) -> u32 {
        self.audio_base.load(Relaxed)
    }

    pub(crate) fn set_alsa(&self, alsa: *mut c_void) {
        self.alsa.store(alsa, Release);
    }

    pub(crate) fn frame_bytes(&self) -> u32 {
        self.params.frame_bytes
    }

    pub(crate) fn period_bytes(&self) -> u32 {
        self.params.period_bytes
    }

    pub(crate) fn running(&self) -> bool {
        self.ab() != 0
    }

    /// Hand the DSP the mixer program, if one was loaded (vendor relock 0x2b7f0, once at load
    /// here). The DSP copies it by bus-master DMA from the host buffer into the next of its two
    /// program buffers (+0x0c/+0x10, used in turn), reading host memory only, then switches to
    /// it at a safe point. Called after bus mastering and the IRQ. A failure is reported and
    /// leaves the DSP on no program.
    pub(crate) fn relock(&self) {
        let Some((buf, len)) = &self.mixer else {
            return;
        };
        let ab = self.ab();
        let Ok(io) = self.io() else { return };
        if ab == 0 {
            return;
        }
        let dev = self.device();

        let idx = {
            let mut st = self.lock.lock();
            st.mixer_idx ^= 1;
            st.mixer_idx
        };
        let dst = io.ard(ab, AREG_MIXPROG_BUF + 4 * idx);
        if !in_ram(dst, MIXPROG_LEN as u32) {
            dev_warn!(dev, "invalid DSP mixer-program buffer {:#010x}\n", dst);
            return;
        }

        io.awr(ab, AREG_COPY_SRC, buf.dma_handle() as u32);
        io.awr(ab, AREG_COPY_DST, dst);
        io.awr(ab, AREG_COPY_LEN, *len as u32);
        io.awr(ab, AREG_COPY_ARM, 1);
        // The vendor polls every 20 us, at most 10000 times.
        let copied = read_poll_timeout(
            || Ok(io.ard(ab, AREG_COPY_LEN)),
            |v| *v == 0,
            Delta::from_micros(20),
            Delta::from_millis(200),
        );
        io.awr(ab, AREG_COPY_ARM, 0);
        if copied.is_err() {
            dev_warn!(
                dev,
                "DSP did not finish the mixer-program copy (+0x100 = {:#x})\n",
                io.ard(ab, AREG_COPY_LEN)
            );
            return;
        }
        let ready = read_poll_timeout(
            || Ok(io.ard(ab, AREG_MIXPROG_READY)),
            |v| *v != 0,
            Delta::from_micros(5),
            Delta::from_millis(100),
        );
        if ready.is_err() {
            dev_warn!(
                dev,
                "DSP never signalled a safe mixer-program swap (+0x108)\n"
            );
            return;
        }
        io.awr(ab, AREG_MIXPROG_READY, 0);
        io.awr(ab, AREG_MIXPROG, dst);
        io.awr(ab, AREG_RELOCK_W, RELOCK_W_1X);
        io.awr(ab, AREG_MIXPROG_SWAP, 1);
        dev_info!(
            dev,
            "mixer program: {:#x} bytes handed to the DSP at {:#x}\n",
            len,
            dst
        );
    }

    /// The idle zero-channel stream Windows runs from driver start (issue #2): +0x58 = 1 makes the
    /// DSP's per-frame routine treat the stream as disabled, +0x68 = +0x6c = 0 fetches no
    /// channels, and the tick runs over the idle ring with 4 periods per wrap. PINTA# is unmasked
    /// as on Windows; the IRQ handler acks these interrupts without signalling ALSA. Probe starts
    /// it after bus mastering and the IRQ; PCM close restarts it.
    pub(crate) fn idle_start(&self) {
        let Some(ring) = &self.idle_ring else { return };
        let Ok(io) = self.io() else { return };

        {
            let mut st = self.lock.lock();
            let ab = self.ab();
            if ab == 0 || st.idle_running {
                return;
            }
            io.awr(ab, AREG_CFG_C, IDLE_CFG_C);
            io.awr(ab, AREG_XFER_A, 0);
            io.awr(ab, AREG_XFER_B, 0);
            io.awr(ab, AREG_IRQ_FRAMES, IDLE_IRQ_FRAMES);
            io.awr(ab, AREG_RING_PERIODS, IDLE_PERIODS);
            io.awr(ab, AREG_RING_BASE, ring.dma_handle() as u32);
            io.awr(ab, AREG_RING_SIZE, RING_BYTES);
            io.awr(ab, AREG_ENABLE, 1);
            io.wr32(PCI_RSTSRC, RSTSRC_INTRST);
            io.port_wr(PORT_HSR, 0);
            st.idle_running = true;
        }
        dev_info!(self.device(), "idle stream running\n");
    }

    /// Stop the idle stream and wait for the DSP's acknowledge; sleeps.
    pub(crate) fn idle_stop(&self) {
        let Ok(io) = self.io() else { return };
        {
            let mut st = self.lock.lock();
            if !st.idle_running {
                return;
            }
            st.idle_running = false;
            let ab = self.ab();
            if ab != 0 {
                io.awr(ab, AREG_STOP_ACK, 0);
                io.awr(ab, AREG_ENABLE, 0);
            }
            io.port_wr(PORT_HSR, PORT_HSR_INTAM);
        }
        self.sync_stop();
    }

    /// Quiesce the card before its IRQ and DMA buffers go away: stop the stream, mask PINTA# and
    /// warm-reset the DSP, which also ends its bus-master accesses to host memory. Idempotent.
    pub(crate) fn shutdown(&self) {
        let Ok(io) = self.io() else { return };
        let mut st = self.lock.lock();
        let ab = self.ab();
        if ab != 0 {
            io.awr(ab, AREG_STOP_ACK, 0);
            io.awr(ab, AREG_ENABLE, 0);
        }
        io.port_wr(PORT_HSR, PORT_HSR_INTAM);
        io.port_wr(PORT_HDCR, PORT_HDCR_WARMRESET);
        self.audio_base.store(0, Relaxed);
        self.playing.store(false, Release);
        st.idle_running = false;
    }

    /// Start playback from the host ring at bus address `ring`: what the vendor's configure path
    /// (0x26760) writes at every stream start, the fetch geometry and the channel map, then ring
    /// base, size code, frames per interrupt and periods per wrap, then enable; clear any stale
    /// interrupt request and unmask PINTA#. Called from the atomic trigger.
    pub(crate) fn stream_start(&self, ring: u64) {
        let Ok(io) = self.io() else { return };
        let _st = self.lock.lock();
        let ab = self.ab();
        if ab == 0 {
            return;
        }
        io.awr(ab, AREG_CFG_C, CFG_C);
        io.awr(ab, AREG_XFER_A, XFER_A);
        io.awr(ab, AREG_XFER_B, XFER_B);
        if self.params.configure {
            for (i, &w) in CHMAP.iter().enumerate() {
                io.wr32(self.published.chmap_base + 4 * i as u32, w);
            }
        }
        io.awr(ab, AREG_RING_BASE, ring as u32);
        io.awr(ab, AREG_RING_SIZE, RING_BYTES);
        io.awr(ab, AREG_IRQ_FRAMES, self.params.irq_frames);
        io.awr(ab, AREG_RING_PERIODS, RING_PERIODS);
        io.awr(ab, AREG_ENABLE, 1);
        io.wr32(PCI_RSTSRC, RSTSRC_INTRST);
        io.port_wr(PORT_HSR, 0);
        self.playing.store(true, Release);
    }

    /// Stop: clear the stop acknowledge, disable, mask PINTA#. The DSP sets +0xf4 again once it
    /// has really stopped; [`Chip::sync_stop`] waits for that outside the atomic trigger.
    pub(crate) fn stream_stop(&self) {
        let Ok(io) = self.io() else { return };
        let _st = self.lock.lock();
        self.playing.store(false, Release);
        let ab = self.ab();
        if ab != 0 {
            io.awr(ab, AREG_STOP_ACK, 0);
            io.awr(ab, AREG_ENABLE, 0);
        }
        io.port_wr(PORT_HSR, PORT_HSR_INTAM);
    }

    /// Wait for the DSP to acknowledge a stop; sleeps.
    pub(crate) fn sync_stop(&self) {
        let ab = self.ab();
        let Ok(io) = self.io() else { return };
        if ab == 0 {
            return;
        }
        let acked = read_poll_timeout(
            || Ok(io.ard(ab, AREG_STOP_ACK)),
            |v| *v != 0,
            Delta::from_micros(100),
            Delta::from_millis(100),
        );
        if acked.is_err() {
            dev_warn!(self.device(), "DSP did not acknowledge the stop\n");
        }
    }

    /// Position, in frames: the DSP stores the host bus address it has reached at
    /// audio_base+0x2c (the vendor computes (value - ring) >> 2, in dwords) and resets it to the
    /// ring base every RING_PERIODS periods.
    pub(crate) fn pointer(&self, ring: u64) -> usize {
        let ab = self.ab();
        let Ok(io) = self.io() else { return 0 };
        if ab == 0 || self.params.frame_bytes == 0 {
            return 0;
        }
        let off = io.ard(ab, AREG_DMAPOINT).wrapping_sub(ring as u32);
        if off >= RING_PERIODS.saturating_mul(self.params.period_bytes) {
            return 0;
        }
        off.checked_div(self.params.frame_bytes).unwrap_or(0) as usize
    }
}

/// Interrupt (vendor ISR 0x2bae0): pending = HSR bit 1; ack by writing INTRST to RSTSRC, which
/// clears the request the DSP raised with INTREQ.
impl irq::Handler for Chip {
    fn handle(&self, dev: &Device<Bound>) -> IrqReturn {
        let Ok(io) = Io::new(&self.bars, dev) else {
            return IrqReturn::None;
        };
        if io.port_rd(PORT_HSR) & PORT_IRQ_PENDING == 0 {
            return IrqReturn::None; // not ours (shared line)
        }
        io.wr32(PCI_RSTSRC, RSTSRC_INTRST);

        if self.playing.load(Acquire) {
            let alsa = self.alsa.load(Acquire);
            if !alsa.is_null() {
                // SAFETY: `alsa` was returned by `motu424_alsa_create()`, and the card it names
                // is freed only after the driver data, which owns this IRQ registration.
                unsafe { alsa::motu424_alsa_period_elapsed(alsa) };
            }
        }
        IrqReturn::Handled
    }
}
