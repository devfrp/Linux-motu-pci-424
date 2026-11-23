// SPDX-License-Identifier: GPL-2.0-or-later
//! re-devext - device-extension type-recovery pass for MOTUAW.sys
//!
//! Scans .text + INIT for every field access on the device-extension object
//! (the `this` passed to WriteRegister/ReadRegister/ISR), to extend the partial
//! field map in docs/vendor-driver-map.md toward a complete struct layout.
//!
//! Strategy:
//!   1. Disassemble .text + INIT with capstone (linear sweep, resume past data).
//!   2. For every [reg+disp] memory operand where disp is in a plausible devext
//!      range (0x0..0x400), record (insn_addr, mnemonic, base_reg, disp, read/write).
//!   3. Also record the function each access lives in (nearest preceding prologue).
//!   4. Output a consolidated field-offset table with all accessor VAs.
//!
//! This is a superset of `xref mem`: it scans all code sections, classifies
//! read vs write, and groups by displacement across the whole image.
//!
//!     re-devext [--detail OFFSET]

use motu424_re::{parse_int, pe_sections, read_or_exit, x86_capstone, x86_sweep, Operand, X86Insn};
use std::collections::{BTreeMap, BTreeSet};

const PATH: &str = "vendor/MOTUAW.sys";

/// Capstone x86 register IDs of the stack registers. Accesses relative to
/// them are locals and arguments, not device-extension fields.
const STACK_REGS: [u16; 2] = [4, 5];

/// Crude read/write classification of an instruction's memory operand.
fn classify_rw(i: &X86Insn) -> &'static str {
    let m = i.mnemonic.to_ascii_lowercase();
    let dest_is_mem = matches!(i.operands.first(), Some(Operand::Mem { .. }));
    match m.as_str() {
        "mov" | "lea" if i.operands.len() >= 2 => {
            if dest_is_mem {
                "W"
            } else {
                "R"
            }
        }
        "add" | "sub" | "and" | "or" | "xor" | "inc" | "dec" | "shl" | "shr" | "sar" | "rol"
        | "ror" => {
            if i.operands.len() >= 2 && dest_is_mem {
                "RW"
            } else {
                "R"
            }
        }
        "pop" => "W",
        _ => "R",
    }
}

struct Access {
    addr: u64,
    func: Option<u64>,
    mnemonic: String,
    rw: &'static str,
    op_str: String,
}

fn main() {
    motu424_card::restore_sigpipe();
    let raw = read_or_exit(PATH);
    let (base, all) = pe_sections(&raw).unwrap_or_else(|e| {
        eprintln!("{PATH}: {e}");
        std::process::exit(1);
    });
    let sections: Vec<_> = all
        .into_iter()
        .filter(|s| s.raw_size > 0 && matches!(s.name.as_str(), ".text" | "INIT" | "init"))
        .collect();

    // Linear sweep across .text + INIT, resuming past undecodable bytes.
    let cs = x86_capstone();
    let mut insns = Vec::new();
    for s in &sections {
        let sz = s.raw_size.min(s.virtual_size);
        let buf = raw.get(s.file_offset..(s.file_offset + sz).min(raw.len())).unwrap_or(&[]);
        insns.extend(x86_sweep(&cs, buf, s.va));
    }
    let mut prologues: Vec<u64> = insns
        .iter()
        .filter(|i| i.mnemonic == "push" && i.op_str == "ebp")
        .map(|i| i.address)
        .collect();
    prologues.sort_unstable();
    let func_of = |addr: u64| -> Option<u64> {
        let n = prologues.partition_point(|&p| p <= addr);
        n.checked_sub(1).map(|k| prologues[k])
    };

    let mut fields: BTreeMap<i64, Vec<Access>> = BTreeMap::new();
    for i in &insns {
        for op in &i.operands {
            let Operand::Mem { base: b, index, disp } = *op else { continue };
            if b == 0 && index == 0 {
                continue; // absolute [disp]
            }
            if STACK_REGS.contains(&b) {
                continue;
            }
            if !(0..=0x400).contains(&disp) {
                continue;
            }
            fields.entry(disp).or_default().push(Access {
                addr: i.address,
                func: func_of(i.address),
                mnemonic: i.mnemonic.clone(),
                rw: classify_rw(i),
                op_str: i.op_str.clone(),
            });
        }
    }

    println!("=== Device-extension field access scan (base={base:#x}) ===");
    let names: Vec<&str> = sections.iter().map(|s| s.name.as_str()).collect();
    println!("Sections scanned: {}", names.join(", "));
    println!("Total instructions: {}", insns.len());
    println!("Functions found: {}", prologues.len());
    println!();
    println!("{:>6}  {:>4}  {:>3} {:>3} {:>3}  Functions (sample)", "Off", "#acc", "R", "W", "RW");
    println!("{}", "-".repeat(80));
    for (disp, acc) in &fields {
        let count = |k: &str| acc.iter().filter(|a| a.rw == k).count();
        let funcs: BTreeSet<u64> = acc.iter().filter_map(|a| a.func).collect();
        let mut shown: Vec<String> = funcs.iter().take(8).map(|f| format!("{f:#x}")).collect();
        if funcs.len() > 8 {
            shown.push(format!("(+{} more)", funcs.len() - 8));
        }
        println!(
            "{:>6}  {:4}  {:3} {:3} {:3}  {}",
            format!("{disp:#x}"),
            acc.len(),
            count("R"),
            count("W"),
            count("RW"),
            shown.join(" ")
        );
    }
    println!();

    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() > 1 && args[0] == "--detail" {
        let target = parse_int(&args[1]).unwrap_or(0) as i64;
        match fields.get(&target) {
            Some(acc) => {
                println!("=== Detailed accesses at offset {target:#x} ===");
                let mut acc: Vec<&Access> = acc.iter().collect();
                acc.sort_by_key(|a| a.addr);
                for a in acc {
                    println!(
                        "  {:#07x} (fn {:#x}): [{}] {:7} {}",
                        a.addr,
                        a.func.unwrap_or(0),
                        a.rw,
                        a.mnemonic,
                        a.op_str
                    );
                }
            }
            None => println!("No accesses at offset {target:#x}"),
        }
    }
}
