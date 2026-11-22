// SPDX-License-Identifier: GPL-2.0-or-later
//! ram-diff A.bin B.bin [--start 0x6e70] [--end 0x10000] [--ab 0x6fac] [--max-run 12]
//!
//! Compare two 256 KB PCI-424 DSP RAM images dword by dword over [start, end)
//! and print each differing run with both values, labelled with the
//! audio_base offset where it falls inside the published block.

use motu424_re::{le32, parse_int, read_or_exit};

const USAGE: &str =
    "usage: ram-diff A.bin B.bin [--start 0x6e70] [--end 0x10000] [--ab 0x6fac] [--max-run 12]";

fn die(msg: &str) -> ! {
    eprintln!("{msg}");
    std::process::exit(2);
}

struct Opts {
    start: usize,
    end: usize,
    ab: usize,
    max_run: usize,
}

fn label(x: usize, ab: usize) -> String {
    match x.checked_sub(ab) {
        Some(off) if off < 0x200 => format!("ab+0x{off:03x}"),
        _ => format!("0x{x:05x}"),
    }
}

fn flush(a: &[u8], b: &[u8], o: &Opts, run: usize, end: usize) {
    let n = (end - run) / 4;
    let shown = (run..end.min(run + 4 * o.max_run)).step_by(4);
    let more = if n > o.max_run { " ..." } else { "" };
    let row = |d: &[u8]| {
        shown
            .clone()
            .map(|x| format!("{:08x}", le32(d, x).unwrap_or(0)))
            .collect::<Vec<_>>()
            .join(" ")
    };
    println!("{} .. {} ({n} dw)", label(run, o.ab), label(end - 4, o.ab));
    println!("   A: {}{more}", row(a));
    println!("   B: {}{more}", row(b));
}

fn main() {
    motu424_card::restore_sigpipe();
    let mut files = Vec::new();
    let mut o = Opts {
        start: 0x6e70,
        end: 0x10000,
        ab: 0x6fac,
        max_run: 12,
    };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "-h" || a == "--help" {
            println!("{USAGE}");
            return;
        }
        if let Some(key) = a.strip_prefix("--") {
            let v = args.next().unwrap_or_else(|| die(USAGE));
            let n = parse_int(&v).unwrap_or_else(|| die(&format!("bad number: {v}"))) as usize;
            match key {
                "start" => o.start = n,
                "end" => o.end = n,
                "ab" => o.ab = n,
                "max-run" => o.max_run = n,
                _ => die(USAGE),
            }
        } else {
            files.push(a);
        }
    }
    if files.len() != 2 {
        die(USAGE);
    }
    let a = read_or_exit(&files[0]);
    let b = read_or_exit(&files[1]);
    let end = o.end.min(a.len()).min(b.len()) & !3;

    let mut run: Option<usize> = None;
    for x in (o.start..end).step_by(4) {
        if le32(&a, x) != le32(&b, x) {
            run.get_or_insert(x);
        } else if let Some(r) = run.take() {
            flush(&a, &b, &o, r, x);
        }
    }
    if let Some(r) = run {
        flush(&a, &b, &o, r, end);
    }
}
