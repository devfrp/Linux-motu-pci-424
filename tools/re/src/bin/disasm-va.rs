// SPDX-License-Identifier: GPL-2.0-or-later
//! disasm-va - disassemble a function of MOTUAW.sys starting at an exact VA
//! (no prologue search), up to `count` instructions or the first `ret`.
//!
//!     disasm-va <va> [count]

use motu424_re::{parse_int, pe_sections, read_or_exit, x86_capstone};

const PATH: &str = "vendor/MOTUAW.sys";

fn main() {
    motu424_card::restore_sigpipe();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(va) = args.first().and_then(|a| parse_int(a)) else {
        eprintln!("usage: disasm-va <va> [count]");
        std::process::exit(1);
    };
    let count = args.get(1).and_then(|c| parse_int(c)).unwrap_or(200) as usize;

    let raw = read_or_exit(PATH);
    let (_, sections) = pe_sections(&raw).unwrap_or_else(|e| {
        eprintln!("{PATH}: {e}");
        std::process::exit(1);
    });
    let Some(s) = sections
        .iter()
        .find(|s| s.va <= va && va < s.va + s.virtual_size as u64)
    else {
        eprintln!("VA {va:#x} not in any section");
        std::process::exit(1);
    };
    let fo = s.file_offset + (va - s.va) as usize;
    let buf = raw.get(fo..(fo + count * 16).min(raw.len())).unwrap_or(&[]);

    let cs = x86_capstone();
    let Ok(insns) = cs.disasm_all(buf, va) else { return };
    for (n, i) in insns.iter().enumerate() {
        let mn = i.mnemonic().unwrap_or("");
        println!("  {:#07x}: {mn:7} {}", i.address(), i.op_str().unwrap_or(""));
        if n + 1 >= count || mn == "ret" {
            break;
        }
    }
}
