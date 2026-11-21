// SPDX-License-Identifier: GPL-2.0-or-later
//! motu424-ringwatch - sample the DSP's published ring position while the
//! motu424 driver streams. Reads only: it maps the window-B BAR read-only
//! through sysfs and reads /proc/interrupts. It can run with the driver bound,
//! except on kernels built with CONFIG_IO_STRICT_DEVMEM=y (Arch, for one):
//! there the mmap is refused while the driver holds the BARs, unless the
//! kernel was booted with iomem=relaxed.
//!
//!     sudo motu424-ringwatch [--bdf 0000:02:01.0] [--audio-base 0x6fac]
//!                            [--period-bytes 0x1800] [--interval 0.1]
//!                            [--duration 10] [--dump FILE]
//!
//! audio_base defaults to the value the driver logged ("DSP running:
//! audio_base=0x...") and falls back to 0x6fac (bring-up run 1, 2026-10-07).
//!
//! Columns: elapsed seconds, IRQ count on the motu424 line, +0x2c (position,
//! host bus address), +0x2c - +0x5c (byte offset into the ring), that offset
//! in periods, and the raw +0x54/+0x60/+0x64 and +0x128..+0x130 words so a
//! wrap or a stop is visible. The last lines give IRQs per second.

use motu424_card::{find_card, map_whole, parse_num, Mapping, RAM_LEN, WINB_LEN};
use std::process::{self, Command};
use std::thread::sleep;
use std::time::{Duration, Instant};

const USAGE: &str = "usage: motu424-ringwatch [--bdf BDF] [--audio-base N] [--period-bytes N] \
[--interval S] [--duration S] [--dump FILE]";

struct Opts {
    bdf: Option<String>,
    audio_base: Option<u32>,
    period_bytes: u32,
    interval: f64,
    duration: f64,
    dump: Option<String>,
}

fn die(msg: impl AsRef<str>) -> ! {
    eprintln!("{}", msg.as_ref());
    process::exit(1);
}

fn parse_opts() -> Opts {
    let mut o = Opts {
        bdf: None,
        audio_base: None,
        period_bytes: 0x1800,
        interval: 0.1,
        duration: 10.0,
        dump: None,
    };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let (key, inline) = match a.split_once('=') {
            Some((k, v)) => (k.to_string(), Some(v.to_string())),
            None => (a.clone(), None),
        };
        if key == "-h" || key == "--help" {
            println!("{USAGE}");
            process::exit(0);
        }
        let val = inline
            .or_else(|| args.next())
            .unwrap_or_else(|| die(format!("{key} needs a value\n{USAGE}")));
        let num = |v: &str| parse_num(v).unwrap_or_else(|| die(format!("bad number for {key}: {v}")));
        let secs = |v: &str| v.parse::<f64>().unwrap_or_else(|_| die(format!("bad seconds for {key}: {v}")));
        match key.as_str() {
            "--bdf" => o.bdf = Some(val),
            "--audio-base" => o.audio_base = Some(num(&val) as u32),
            "--period-bytes" => o.period_bytes = num(&val) as u32,
            "--interval" => o.interval = secs(&val),
            "--duration" => o.duration = secs(&val),
            "--dump" => o.dump = Some(val),
            _ => die(USAGE),
        }
    }
    o
}

/// The last audio_base the driver logged.
fn audio_base_from_dmesg() -> Option<u32> {
    let out = Command::new("dmesg").output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .filter(|l| l.contains("motu424"))
        .filter_map(|l| {
            let hex = l.split("audio_base=0x").nth(1)?;
            let hex: String = hex.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
            u32::from_str_radix(&hex, 16).ok()
        })
        .next_back()
}

/// Interrupts on the motu424 line, summed over CPUs.
fn irq_count() -> Option<u64> {
    let text = std::fs::read_to_string("/proc/interrupts").ok()?;
    let line = text.lines().find(|l| l.contains("motu424"))?;
    Some(
        line.split_whitespace()
            .skip(1)
            .filter(|w| w.chars().all(|c| c.is_ascii_digit()))
            .filter_map(|w| w.parse::<u64>().ok())
            .sum(),
    )
}

fn main() {
    motu424_card::restore_sigpipe();
    let o = parse_opts();
    let dev = find_card(o.bdf.as_deref()).unwrap_or_else(|e| die(e));
    let ab = o.audio_base.or_else(audio_base_from_dmesg).unwrap_or(0x6fac);
    let idx = dev
        .find_bar(|b| b.is_mem() && b.size == WINB_LEN)
        .unwrap_or_else(|| die("no 4 MB MMIO BAR (window B) on this card"));
    let res = dev.resource_path(idx);
    let mm: Mapping = map_whole(&res, WINB_LEN, false).unwrap_or_else(|e| {
        die(format!(
            "cannot map {e}; with the driver bound, a CONFIG_IO_STRICT_DEVMEM=y kernel needs iomem=relaxed"
        ))
    });
    if !ab.is_multiple_of(4) || ab + 0x140 > RAM_LEN {
        die(format!("audio_base 0x{ab:x} is not a dword address in DSP RAM"));
    }
    let rd = |off: u32| mm.read32((ab + off) as usize);

    if let Some(path) = &o.dump {
        let img = mm.read_bytes(0, RAM_LEN as usize);
        std::fs::write(path, img).unwrap_or_else(|e| die(format!("{path}: {e}")));
        println!("# wrote 256 KB of DSP RAM from {} to {path}", res.display());
        return;
    }

    println!(
        "# card {}, window B {}, audio_base 0x{ab:x}, period_bytes 0x{:x}",
        dev.bdf,
        res.display(),
        o.period_bytes
    );
    println!(
        "# +0x0c=0x{:x} +0x10=0x{:x} +0x58=0x{:x} +0x68=0x{:x} +0x6c=0x{:x} +0x8c=0x{:x} +0x80=0x{:x} +0x98=0x{:x}",
        rd(0x0c),
        rd(0x10),
        rd(0x58),
        rd(0x68),
        rd(0x6c),
        rd(0x8c),
        rd(0x80),
        rd(0x98)
    );
    println!("#     t   irqs      +0x2c      +0x5c    offset  periods  +0x54 +0x60 +0x64   +0x128   +0x12c   +0x130");

    let t0 = Instant::now();
    let irq0 = irq_count();
    let mut last = None;
    let mut prev_irqs = irq0;
    let mut first_tick: Option<(f64, u64)> = None; // (t, irqs) at the first/last IRQ change
    let mut last_tick: Option<(f64, u64)> = None;
    let mut t;
    loop {
        t = t0.elapsed().as_secs_f64();
        let irqs = irq_count();
        let (pos, base) = (rd(0x2c), rd(0x5c));
        let off = pos.wrapping_sub(base);
        let row = (
            irqs,
            pos,
            base,
            [rd(0x54), rd(0x60), rd(0x64), rd(0x128), rd(0x12c), rd(0x130)],
        );
        if let (Some(i), Some(p)) = (irqs, prev_irqs) {
            if i != p {
                first_tick.get_or_insert((t, p));
                last_tick = Some((t, i));
            }
        }
        prev_irqs = irqs;
        let done = t >= o.duration;
        if last != Some(row) || done {
            let per = if o.period_bytes != 0 {
                format!("{:7.3}", off as f64 / o.period_bytes as f64)
            } else {
                "      -".to_string()
            };
            let w = row.3;
            println!(
                "{t:7.3} {:6} 0x{pos:08x} 0x{base:08x} 0x{off:07x} {per}  {:5x} {:5x} {:5x} {:8x} {:8x} {:8x}",
                irqs.map_or(-1, |i| i as i64),
                w[0],
                w[1],
                w[2],
                w[3],
                w[4],
                w[5]
            );
            last = Some(row);
        }
        if done {
            break;
        }
        sleep(Duration::from_secs_f64(o.interval));
    }
    let irq1 = irq_count();
    if let (Some(a), Some(b)) = (irq0, irq1) {
        if t > 0.0 {
            println!("# whole run: {} IRQs in {t:.3} s = {:.2}/s", b - a, (b - a) as f64 / t);
        }
    }
    if let (Some((ta, ia)), Some((tb, ib))) = (first_tick, last_tick) {
        if tb > ta {
            let (n, dt) = (ib - ia, tb - ta);
            println!(
                "# while ticking: {n} IRQs in {dt:.3} s = {:.2}/s (0x40 frames/IRQ: 750/s at 48 kHz with 24 channels; \
                 the line may be shared, subtract the other device's count)",
                n as f64 / dt
            );
        }
    }
}
