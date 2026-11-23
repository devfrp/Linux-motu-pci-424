// SPDX-License-Identifier: GPL-2.0-or-later
//! xref - capstone-based cross-reference / disassembly helper for the MOTU
//! vendor driver `MOTUAW.sys` (PE32 i386, image base 0x10000).
//!
//! objdump gives no xrefs; vtable-scan resolves vtable slots but not call sites.
//! This closes the gap: it linearly disassembles .text with capstone, then
//! answers "who calls VA X", "what does function at VA Y do", and "where is
//! immediate/disp Z used", including indirect `call [reg+disp]` virtual
//! dispatch sites.
//!
//!     xref fn 0x11320            # disassemble the function starting at VA
//!     xref callers 0x11320       # direct callers of VA
//!     xref vcalls                # every indirect `call [reg+disp]`, by disp
//!     xref imm 0x30cc0           # instructions referencing an immediate/disp
//!     xref mem 0x64              # instructions touching [reg+0x64] (disp match)
//!
//! Section table is for the shipped MOTUAW.sys; adjust from `objdump -h` if needed.

use motu424_re::{parse_int, py_hex, read_or_exit, x86_capstone, x86_sweep, Operand, X86Insn};
use std::collections::BTreeMap;

const PATH: &str = "vendor/MOTUAW.sys";
// .text: virtual_addr, file_offset, size, from `objdump -h MOTUAW.sys`
const TEXT_VA: u64 = 0x11000;
const TEXT_FO: usize = 0x400;
const TEXT_SZ: usize = 0x1e4ba;

const USAGE: &str = "usage: xref fn|callers|imm|mem <va-or-value> | xref vcalls";

/// Instructions from `start` up to and including the first ret/retN.
fn fn_body(insns: &[X86Insn], start: u64) -> &[X86Insn] {
    let Some(s) = insns.iter().position(|i| i.address == start) else {
        return &[];
    };
    let end = insns[s..]
        .iter()
        .position(|i| i.mnemonic.starts_with("ret"))
        .map_or(insns.len(), |e| s + e + 1);
    &insns[s..end]
}

/// Nearest `push ebp` prologue at or before `va`.
fn fn_start(insns: &[X86Insn], va: u64) -> Option<u64> {
    insns
        .iter()
        .take_while(|i| i.address <= va)
        .filter(|i| i.mnemonic == "push" && i.op_str == "ebp")
        .map(|i| i.address)
        .last()
}

fn main() {
    motu424_card::restore_sigpipe();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first().map(String::as_str) else {
        eprintln!("{USAGE}");
        std::process::exit(1);
    };
    let arg = || -> u64 {
        args.get(1).and_then(|a| parse_int(a)).unwrap_or_else(|| {
            eprintln!("{USAGE}");
            std::process::exit(1)
        })
    };
    let raw = read_or_exit(PATH);
    let buf = raw.get(TEXT_FO..(TEXT_FO + TEXT_SZ).min(raw.len())).unwrap_or(&[]);
    let ins = x86_sweep(&x86_capstone(), buf, TEXT_VA);
    let print = |i: &X86Insn| println!("  {:#07x}: {} {}", i.address, i.mnemonic, i.op_str);

    match cmd {
        "fn" => {
            let start = arg();
            let s = fn_start(&ins, start).unwrap_or(start);
            for i in fn_body(&ins, s) {
                println!("  {:#07x}: {:7} {}", i.address, i.mnemonic, i.op_str);
            }
        }
        "callers" => {
            let tgt = arg();
            let want = format!("{tgt:#x}");
            for i in ins.iter().filter(|i| i.mnemonic == "call" && i.op_str == want) {
                let fs = fn_start(&ins, i.address).map_or("?".into(), |f| format!("{f:#x}"));
                println!("  {:#07x} (in fn {fs}): call {tgt:#x}", i.address);
            }
        }
        "vcalls" => {
            // indirect calls through a register+displacement (virtual dispatch)
            let mut by: BTreeMap<i64, Vec<u64>> = BTreeMap::new();
            for i in ins.iter().filter(|i| i.mnemonic == "call") {
                for op in &i.operands {
                    if let Operand::Mem { base, disp, .. } = *op {
                        if base != 0 {
                            by.entry(disp).or_default().push(i.address);
                        }
                    }
                }
            }
            for (disp, locs) in &by {
                let shown: Vec<String> = locs.iter().take(20).map(|a| format!("{a:#x}")).collect();
                println!("  disp {} ({}x): {}", py_hex(*disp), locs.len(), shown.join(" "));
            }
        }
        "imm" => {
            let val = arg() as i64;
            for i in &ins {
                for op in &i.operands {
                    match *op {
                        Operand::Imm(v) if v == val => print(i),
                        Operand::Mem { disp, .. } if disp == val => print(i),
                        _ => {}
                    }
                }
            }
        }
        "mem" => {
            let disp = arg() as i64;
            for i in &ins {
                for op in &i.operands {
                    if let Operand::Mem { base, disp: d, .. } = *op {
                        if d == disp && base != 0 {
                            print(i);
                        }
                    }
                }
            }
        }
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(1);
        }
    }
}
