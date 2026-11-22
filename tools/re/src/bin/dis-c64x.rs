// SPDX-License-Identifier: GPL-2.0-or-later
//! Disassemble a raw little-endian TMS320C64x image (loaded at address 0) with Capstone.
//!
//! Default output is one instruction per line in TI listing style: a leading "||"
//! marks an instruction that executes in parallel with the one above (p-bit of the
//! previous word set). Words that do not decode are printed as .word.
//!
//! --fields lists every data-page-relative access (*+B14[n]) instead, grouped by
//! field, translated to an offset from --ab (the published audio_base; the DSP's
//! data page is B14 and audio_base = B14 + 0x13c in the PCI-424 program), with the
//! start address of the enclosing routine. Routine boundaries are the heuristic
//! "the instruction after a `b b3` return plus its five delay slots".
//!
//! usage: dis-c64x IMAGE [--base ADDR] [--fields [--dp ADDR] [--ab ADDR]]

use capstone::prelude::*;
use capstone::{Arch, Endian, Mode};
use motu424_re::{le32, parse_int, read_or_exit};
use std::collections::BTreeMap;

const USAGE: &str = "usage: dis-c64x IMAGE [--base ADDR] [--fields [--dp ADDR] [--ab ADDR]]";

/// Memory-operand forms of the C64x load/store "mode" field (SPRU732).
fn mode_fmt(mode: u32, b: &str, o: u32, r: &str) -> String {
    match mode {
        0 => format!("*-{b}[{o}]"),
        1 => format!("*+{b}[{o}]"),
        4 => format!("*-{b}[{r}]"),
        5 => format!("*+{b}[{r}]"),
        8 => format!("*--{b}[{o}]"),
        9 => format!("*++{b}[{o}]"),
        10 => format!("*{b}--[{o}]"),
        11 => format!("*{b}++[{o}]"),
        12 => format!("*--{b}[{r}]"),
        13 => format!("*++{b}[{r}]"),
        14 => format!("*{b}--[{r}]"),
        15 => format!("*{b}++[{r}]"),
        _ => format!("*?{b}[{o}]"),
    }
}

/// `ops` holds a 64-bit register pair such as "a5:a4" (a word boundary before it).
fn has_pair(ops: &str) -> bool {
    let b = ops.as_bytes();
    let digits = |mut i: usize| {
        let s = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        (i > s).then_some(i)
    };
    (0..b.len()).any(|i| {
        let boundary = i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_');
        if !boundary || !matches!(b[i], b'a' | b'b') {
            return false;
        }
        let Some(j) = digits(i + 1) else { return false };
        if b.get(j) != Some(&b':') || !matches!(b.get(j + 1), Some(b'a' | b'b')) {
            return false;
        }
        digits(j + 2).is_some()
    })
}

/// Capstone 5's C64x module gets the A/B register-file side of load/store
/// operands wrong (it prints e.g. "stw b3, *a15--[2]" for the standard
/// "STW B3, *B15--[2]" prologue push). Re-derive the operands from the raw word
/// per the C64x CPU ISA (SPRU732) load/store formats: bit 3 = 1 selects the
/// 15-bit-offset form whose base is B14 (y=0) or B15 (y=1); otherwise base/offset
/// registers are on the .D1 (A, y=0) or .D2 (B, y=1) side; the data register side
/// is the s bit (bit 1) in both forms, and bit 2 is set in both.
fn ldst_operands(word: u32, mnemonic: &str, cap_ops: &str) -> String {
    let s = (word >> 1) & 1;
    let y = (word >> 7) & 1;
    let sd = (word >> 23) & 31;
    let side = if s != 0 { "b" } else { "a" };
    let data = if has_pair(cap_ops) {
        format!("{side}{}:{side}{sd}", sd + 1)
    } else {
        format!("{side}{sd}")
    };
    let mem = if (word >> 3) & 1 != 0 {
        format!(
            "*+{}[0x{:x}]",
            if y != 0 { "b15" } else { "b14" },
            (word >> 8) & 0x7fff
        )
    } else {
        let bs = if y != 0 { "b" } else { "a" };
        let (base, off, mode) = ((word >> 18) & 31, (word >> 13) & 31, (word >> 9) & 15);
        mode_fmt(mode, &format!("{bs}{base}"), off, &format!("{bs}{off}"))
    };
    if mnemonic.to_ascii_lowercase().starts_with("ld") {
        format!("{mem}, {data}")
    } else {
        format!("{data}, {mem}")
    }
}

/// One B14-relative access: address, LD/ST/other, mnemonic, operands, routine.
type Access<'a> = (u64, String, &'a str, &'a str, u64);

struct Line {
    addr: u64,
    word: u32,
    par: bool,
    mn: String,
    ops: String,
}

/// The access size of a load/store mnemonic: ldw -> 4, ldnw -> 4, ldhu -> 2.
fn access_size(mn: &str) -> u64 {
    let l = mn.to_ascii_lowercase();
    let sz = l.get(2..).unwrap_or("").trim_start_matches('n').trim_end_matches('u');
    match sz {
        "b" => 1,
        "h" => 2,
        "w" => 4,
        "dw" => 8,
        _ => 4,
    }
}

/// The offset n of a `*+b14[n]` / `*b14[n]` operand.
fn b14_offset(ops: &str) -> Option<u64> {
    let l = ops.to_ascii_lowercase();
    let mut from = 0;
    while let Some(p) = l[from..].find('*') {
        let mut rest = &l[from + p + 1..];
        rest = rest.strip_prefix('+').unwrap_or(rest);
        if let Some(r) = rest.strip_prefix("b14[") {
            if let Some(end) = r.find(']') {
                let n = &r[..end];
                let ok = if let Some(h) = n.strip_prefix("0x") {
                    !h.is_empty() && h.chars().all(|c| c.is_ascii_hexdigit())
                } else {
                    !n.is_empty() && n.chars().all(|c| c.is_ascii_digit())
                };
                if ok {
                    return parse_int(n);
                }
            }
        }
        from += p + 1;
    }
    None
}

fn main() {
    motu424_card::restore_sigpipe();
    let mut image = None;
    let (mut base, mut fields, mut dp, mut ab) = (0u64, false, 0x6e70u64, 0x6facu64);
    let mut args = std::env::args().skip(1);
    let num = |v: Option<String>| -> u64 {
        v.as_deref().and_then(parse_int).unwrap_or_else(|| {
            eprintln!("{USAGE}");
            std::process::exit(2)
        })
    };
    while let Some(a) = args.next() {
        match a.as_str() {
            "--base" => base = num(args.next()),
            "--fields" => fields = true,
            "--dp" => dp = num(args.next()),
            "--ab" => ab = num(args.next()),
            "-h" | "--help" => {
                println!("{USAGE}");
                return;
            }
            s if !s.starts_with('-') && image.is_none() => image = Some(s.to_string()),
            _ => {
                eprintln!("{USAGE}");
                std::process::exit(2);
            }
        }
    }
    let Some(image) = image else {
        eprintln!("{USAGE}");
        std::process::exit(2);
    };
    let data = read_or_exit(&image);

    let mut cs = Capstone::new_raw(Arch::TMS320C64X, Mode::Default, std::iter::empty(), Some(Endian::Little))
        .expect("capstone: TMS320C64x support missing");
    cs.set_skipdata(true).expect("capstone: skipdata");
    let insns = cs.disasm_all(&data, base).expect("capstone: disassembly failed");

    let mut lines = Vec::new();
    let mut prev_p = 0;
    for i in insns.iter() {
        let word = le32(&data, (i.address() - base) as usize).unwrap_or(0);
        let mn = i.mnemonic().unwrap_or("");
        if mn == ".byte" {
            lines.push(Line {
                addr: i.address(),
                word,
                par: false,
                mn: ".word".into(),
                ops: format!("0x{word:08x}"),
            });
            prev_p = 0;
            continue;
        }
        let mut ops = i.op_str().unwrap_or("").to_string();
        let lmn = mn.to_ascii_lowercase();
        if (lmn.starts_with("ld") || lmn.starts_with("st")) && word & 4 != 0 {
            ops = ldst_operands(word, mn, &ops);
        }
        // Capstone omits the predicate: creg (bits 31-29) 1..6 = B0 B1 B2 A1 A2 A0, z (bit 28) = negate.
        let (creg, z) = (word >> 29, (word >> 28) & 1);
        if creg > 0 && creg < 7 && mn != "nop" && mn != "NOP" {
            let reg = ["B0", "B1", "B2", "A1", "A2", "A0"][creg as usize - 1];
            ops = format!("[{}{reg}] {ops}", if z != 0 { "!" } else { "" });
        }
        lines.push(Line {
            addr: i.address(),
            word,
            par: prev_p != 0,
            mn: mn.to_string(),
            ops,
        });
        prev_p = word & 1;
    }

    if !fields {
        for l in &lines {
            let (pred, rest) = match l.ops.strip_prefix('[').and_then(|_| l.ops.split_once("] ")) {
                Some((p, r)) => (format!("{p}]"), r),
                None => (String::new(), l.ops.as_str()),
            };
            println!(
                "{:06x}  {:08x}  {} {:6}{:8} {rest}",
                l.addr,
                l.word,
                if l.par { "||" } else { "  " },
                pred,
                l.mn
            );
        }
        return;
    }

    // routine starts: after each "b b3" return and its 5 delay slots (ignore parallel bars)
    let mut starts = vec![base];
    for (n, l) in lines.iter().enumerate() {
        if l.mn.eq_ignore_ascii_case("b") && l.ops.trim().eq_ignore_ascii_case("b3") {
            // skip to the first instruction of the execute packet after the 5 delay-slot packets
            let (mut k, mut packets) = (n, 0);
            while packets < 6 && k + 1 < lines.len() {
                k += 1;
                if !lines[k].par {
                    packets += 1;
                }
            }
            starts.push(lines[k].addr);
        }
    }
    starts.sort_unstable();
    starts.dedup();
    let routine_of = |addr: u64| {
        let i = starts.partition_point(|&s| s <= addr);
        starts[i.saturating_sub(1)]
    };

    // field byte offset -> (addr, kind, mnemonic, operands, routine)
    let mut found: BTreeMap<u64, Vec<Access>> = BTreeMap::new();
    for l in &lines {
        let Some(n) = b14_offset(&l.ops) else { continue };
        // the offset is scaled by the access size: B/BU x1, H/HU x2, W x4, DW x8
        let byte = n * access_size(&l.mn);
        let lmn = l.mn.to_ascii_lowercase();
        let kind = if lmn.starts_with("st") {
            "ST".to_string()
        } else if lmn.starts_with("ld") {
            "LD".to_string()
        } else {
            l.mn.clone()
        };
        found
            .entry(byte)
            .or_default()
            .push((l.addr, kind, &l.mn, &l.ops, routine_of(l.addr)));
    }

    let delta = ab as i64 - dp as i64;
    let delta = if delta < 0 {
        format!("-{:x}", -delta)
    } else {
        format!("{delta:x}")
    };
    println!("# data page B14 = 0x{dp:x}, audio_base = 0x{ab:x} (= B14 + 0x{delta})");
    println!("# {} routines by the b-b3 heuristic", starts.len());
    for (byte, uses) in &found {
        let rel = *byte as i64 - (ab as i64 - dp as i64);
        let tag = if rel >= 0 {
            format!("audio_base+0x{rel:x}")
        } else {
            format!("audio_base-0x{:x}", -rel)
        };
        let nld = uses.iter().filter(|u| u.1 == "LD").count();
        let nst = uses.iter().filter(|u| u.1 == "ST").count();
        println!(
            "\n== B14+0x{byte:x} = 0x{:x} = {tag}   ({nld} LD, {nst} ST, {} other)",
            dp + byte,
            uses.len() - nld - nst
        );
        for (addr, kind, mn, ops, rt) in uses {
            println!("   {addr:06x}  routine {rt:06x}  {kind:2}  {mn:8} {ops}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairs_need_a_word_boundary() {
        assert!(has_pair("a5:a4, *+b14[0x8]"));
        assert!(!has_pair("xa5:a4"));
        assert!(!has_pair("a5, a4"));
    }

    #[test]
    fn b14_offsets_are_found() {
        assert_eq!(b14_offset("*+b14[0x2c], a3"), Some(0x2c));
        assert_eq!(b14_offset("b3, *+B14[12]"), Some(12));
        assert_eq!(b14_offset("*+b15[0x2c], a3"), None);
    }

    #[test]
    fn access_sizes_follow_the_mnemonic() {
        assert_eq!(access_size("ldw"), 4);
        assert_eq!(access_size("ldnw"), 4);
        assert_eq!(access_size("ldhu"), 2);
        assert_eq!(access_size("stb"), 1);
        assert_eq!(access_size("lddw"), 8);
    }
}
