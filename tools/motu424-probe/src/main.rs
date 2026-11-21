// SPDX-License-Identifier: GPL-2.0-or-later
//! motu424-probe - userspace helper for reverse engineering the MOTU
//! PCI-324/424 register map.
//!
//! It scans /sys/bus/pci for Mark of the Unicorn devices (vendor 0x137A),
//! prints their identity, enumerates every BAR and classifies it against the
//! windowed hardware model recovered from the vendor driver (see
//! docs/register-map.md):
//!
//!   - an ~8 MB MMIO BAR  -> window A  (card addr & 0x7fffff)
//!   - a  ~4 MB MMIO BAR  -> window B  (card addr & 0x3fffff, DSP memory)
//!   - a small I/O-port BAR -> the DSP's HPI registers (HSR, HDCR, DSPP)
//!
//! It dumps the head of each BAR and the old window-B bank control words, and
//! takes an optional targeted window-B dump so idle-vs-streaming diffs can be
//! taken. Read-only. The kernel driver must not be bound at the same time.
//!
//!   sudo motu424-probe                          # scan + classify + dump all cards
//!   sudo motu424-probe 0000:01:00.0             # one card
//!   sudo motu424-probe 0000:01:00.0 0x6fac 0x200   # window-B dump @off,len

use motu424_card::{
    find_cards, hexdump_words, model_name, parse_num, Bar, Device, Mapping, NUM_BARS, WINB_MASK,
};
use std::process::ExitCode;

/// How much of each BAR head to dump.
const HEAD_BYTES: u64 = 0x100;
/// The window-B bank control words (PCI-324 FPGA-load path).
const BANK0_CTRL: u64 = 0x000c_0024;
const BANK1_CTRL: u64 = 0x0010_0024;
const PAGE: u64 = 0x1000;

/// Map `want` bytes at page-aligned `off` of BAR `idx`, clamped to the BAR.
fn map_bar(dev: &Device, idx: usize, bar: &Bar, off: u64, want: u64) -> Option<Mapping> {
    let want = if off + want > bar.size {
        bar.size.saturating_sub(off)
    } else {
        want
    };
    if want == 0 {
        return None;
    }
    match Mapping::new(&dev.resource_path(idx), off, want as usize, false) {
        Ok(m) => Some(m),
        Err(e) => {
            eprintln!("mmap resource{idx} (need root, and driver unbound): {e}");
            None
        }
    }
}

fn words(m: &Mapping, from: usize, len: usize) -> Vec<u32> {
    (0..len / 4).map(|i| m.read32(from + 4 * i)).collect()
}

/// Dump the head of a BAR plus, for window B, the bank control words.
fn dump_bar(dev: &Device, idx: usize, bar: &Bar) {
    if let Some(m) = map_bar(dev, idx, bar, 0, HEAD_BYTES) {
        println!("    head:");
        hexdump_words(0, &words(&m, 0, m.len()));
    }
    if bar.is_window_b() {
        for reg in [BANK0_CTRL & WINB_MASK, BANK1_CTRL & WINB_MASK] {
            if let Some(m) = map_bar(dev, idx, bar, reg & !(PAGE - 1), PAGE) {
                println!(
                    "    bank ctrl @ card 0x{reg:08x} (win-B): {:08x}",
                    m.read32((reg & (PAGE - 1)) as usize)
                );
            }
        }
    }
}

/// The optional user-requested window-B dump.
fn dump_target(dev: &Device, bars: &[Bar; NUM_BARS], card_off: u64, len: u64) {
    let Some(idx) = (0..NUM_BARS).rev().find(|&i| bars[i].is_window_b()) else {
        eprintln!("  no window-B BAR to target");
        return;
    };
    let off = card_off & WINB_MASK;
    let page = off & !(PAGE - 1);
    // map whole pages covering [off, off+len), not just len rounded up
    let span = ((off - page) + len + PAGE - 1) & !(PAGE - 1);
    let Some(m) = map_bar(dev, idx, &bars[idx], page, span) else {
        return;
    };
    // map_bar() clamps the mapping to the BAR; clamp the dump the same way
    let len = len.min(m.len() as u64 - (off - page));
    println!("  window-B target @ card 0x{card_off:08x}, {len} bytes (BAR{idx}):");
    hexdump_words(card_off, &words(&m, (off - page) as usize, len as usize));
}

fn handle(dev: &Device, target: Option<(u64, u64)>) {
    let device = dev.device();
    println!(
        "MOTU PCI device {}: device={device:04x} ({})",
        dev.bdf,
        model_name(device)
    );
    let bars = dev.bars();
    for (i, b) in bars.iter().enumerate() {
        if b.size == 0 {
            continue;
        }
        println!(
            "  BAR{i}: 0x{:08x}..0x{:08x} ({} KB) flags=0x{:x} -> {}",
            b.start,
            b.end,
            b.size >> 10,
            b.flags,
            b.classify()
        );
        dump_bar(dev, i, b);
    }
    if let Some((off, len)) = target {
        if len != 0 {
            dump_target(dev, &bars, off, len);
        }
    }
}

fn main() -> ExitCode {
    motu424_card::restore_sigpipe();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let want = args.first().map(String::as_str);
    let target = args.get(1).map(|o| {
        (
            parse_num(o).unwrap_or(0),
            args.get(2).and_then(|l| parse_num(l)).unwrap_or(0),
        )
    });

    let devices: Vec<Device> = match want {
        Some(bdf) => {
            let d = Device::new(bdf);
            if d.exists() {
                vec![d]
            } else {
                vec![]
            }
        }
        None => find_cards(),
    };
    for d in &devices {
        handle(d, target);
    }
    if devices.is_empty() {
        eprintln!(
            "No MOTU (vendor 137a) PCI device found.\n\
             If the card is present, check `lspci -nn | grep 137a`."
        );
        return ExitCode::from(2);
    }
    ExitCode::SUCCESS
}
