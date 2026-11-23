// SPDX-License-Identifier: GPL-2.0-or-later
//! vtable-scan - static C++ vtable / xref helper for reverse-engineering the
//! MOTU vendor driver `MOTUAW.sys` (PE32 i386, image base 0x10000).
//!
//! objdump gives no cross-references, so this reconstructs them: it scans the
//! .rdata/.data sections for runs of dwords that all point into .text (i.e.
//! vtables), and lets you resolve a given vtable slot to a function VA. This is
//! the tool that cracked the IRQ path (hardware-class vtable @ 0x30cc0, ISR =
//! slot 0x28 = 0x2bae0). See docs/register-map.md and docs/vendor-driver-map.md.
//!
//!     vtable-scan [path-to-MOTUAW.sys]
//!     vtable-scan MOTUAW.sys --slot 0x30cc0 0x28
//!
//! The binary lives in the git-ignored vendor/ dir and is never redistributed.
//! Section offsets below are for the shipped MOTUAW.sys; adjust from
//! `objdump -h` if you re-run against a different build.

use motu424_re::{le32, parse_int, read_or_exit};

/// name -> (virtual_addr, file_offset, size)  from `objdump -h MOTUAW.sys`
const SEC: [(&str, u64, usize, u64); 3] = [
    (".text", 0x11000, 0x00400, 0x1e4ba),
    (".rdata", 0x30000, 0x1ea00, 0x00f34),
    (".data", 0x31000, 0x1fa00, 0x31198),
];
const TEXT_LO: u32 = 0x11000;
const TEXT_HI: u32 = 0x11000 + 0x1e4ba;

fn va2fo(va: u64) -> Option<usize> {
    SEC.iter()
        .find(|(_, v, _, sz)| *v <= va && va < v + sz)
        .map(|(_, v, fo, _)| fo + (va - v) as usize)
}

fn rd32(buf: &[u8], va: u64) -> Option<u32> {
    le32(buf, va2fo(va)?)
}

fn is_code(x: Option<u32>) -> bool {
    matches!(x, Some(v) if (TEXT_LO..TEXT_HI).contains(&v))
}

/// (vtable_va, slot targets) for runs of at least `min_slots` code pointers.
fn scan_vtables(buf: &[u8], sec: &str, min_slots: usize) -> Vec<(u64, Vec<u32>)> {
    let &(_, va, _, sz) = SEC.iter().find(|s| s.0 == sec).expect("known section");
    let mut out = Vec::new();
    let mut i = 0u64;
    while i + 4 < sz {
        let a = va + i;
        let mut vals = Vec::new();
        let mut j = a;
        while is_code(rd32(buf, j)) {
            vals.push(rd32(buf, j).unwrap_or(0));
            j += 4;
        }
        if vals.len() >= min_slots {
            out.push((a, vals));
            i = j - va;
        } else {
            i += 4;
        }
    }
    out
}

fn main() {
    motu424_card::restore_sigpipe();
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut path = "vendor/MOTUAW.sys".to_string();
    if args.first().is_some_and(|a| !a.starts_with("--")) {
        path = args.remove(0);
    }
    let buf = read_or_exit(&path);

    if args.len() >= 3 && args[0] == "--slot" {
        let vt = parse_int(&args[1]).unwrap_or(0);
        let off = parse_int(&args[2]).unwrap_or(0);
        let tgt = rd32(&buf, vt + off);
        let shown = match tgt {
            Some(t) if t != 0 => format!("0x{t:x}"),
            _ => "(none)".to_string(),
        };
        println!("vtable 0x{vt:x} slot@0x{off:x} -> {shown}");
        return;
    }

    for sec in [".rdata", ".data"] {
        println!("==== vtable candidates in {sec} ====");
        for (a, vals) in scan_vtables(&buf, sec, 3) {
            let head: Vec<String> = vals.iter().take(12).map(|v| format!("0x{v:x}")).collect();
            println!(
                "  vtab @0x{a:x} ({} slots): {}{}",
                vals.len(),
                head.join(" "),
                if vals.len() > 12 { " ..." } else { "" }
            );
        }
    }
}
