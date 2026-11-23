// SPDX-License-Identifier: GPL-2.0-or-later
//! Shared helpers for the reverse-engineering tools in tools/re/.
//!
//! They read the user's own copy of the vendor driver (`vendor/MOTUAW.sys`,
//! git-ignored) or the user-extracted DSP program. Nothing here is ever
//! redistributed; the tools print facts (addresses, offsets, hashes).

/// Read a whole file or exit with a message.
pub fn read_or_exit(path: &str) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|e| {
        eprintln!("{path}: {e}");
        std::process::exit(1);
    })
}

/// Parse a number like Python's int(s, 0): 0x.., 0o.., 0b.. or decimal.
pub fn parse_int(s: &str) -> Option<u64> {
    let s = s.trim();
    let lower = s.to_ascii_lowercase();
    if let Some(h) = lower.strip_prefix("0x") {
        u64::from_str_radix(h, 16).ok()
    } else if let Some(o) = lower.strip_prefix("0o") {
        u64::from_str_radix(o, 8).ok()
    } else if let Some(b) = lower.strip_prefix("0b") {
        u64::from_str_radix(b, 2).ok()
    } else {
        s.parse().ok()
    }
}

/// Little-endian dword at byte offset `off`.
pub fn le32(buf: &[u8], off: usize) -> Option<u32> {
    buf.get(off..off + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// One section of a PE image, as the section table gives it.
#[derive(Clone, Debug)]
pub struct Section {
    pub name: String,
    /// Absolute virtual address (image base + RVA).
    pub va: u64,
    pub file_offset: usize,
    pub raw_size: usize,
    pub virtual_size: usize,
}

/// The image base and section table of a PE32/PE32+ file.
pub fn pe_sections(raw: &[u8]) -> Result<(u64, Vec<Section>), String> {
    let u16_at = |o: usize| raw.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    let bad = || "not a PE image".to_string();
    if raw.get(0..2) != Some(b"MZ") {
        return Err(bad());
    }
    let pe = le32(raw, 0x3c).ok_or_else(bad)? as usize;
    if raw.get(pe..pe + 4) != Some(b"PE\0\0") {
        return Err(bad());
    }
    let coff = pe + 4;
    let nsec = u16_at(coff + 2).ok_or_else(bad)? as usize;
    let opt_size = u16_at(coff + 16).ok_or_else(bad)? as usize;
    let opt = coff + 20;
    let base = match u16_at(opt).ok_or_else(bad)? {
        0x10b => le32(raw, opt + 28).ok_or_else(bad)? as u64,
        0x20b => {
            let lo = le32(raw, opt + 24).ok_or_else(bad)? as u64;
            let hi = le32(raw, opt + 28).ok_or_else(bad)? as u64;
            lo | hi << 32
        }
        _ => return Err(bad()),
    };
    let table = opt + opt_size;
    let mut out = Vec::with_capacity(nsec);
    for k in 0..nsec {
        let o = table + 40 * k;
        let name = raw.get(o..o + 8).ok_or_else(bad)?;
        let name = String::from_utf8_lossy(name).trim_end_matches('\0').to_string();
        let field = |n: usize| le32(raw, o + 8 + 4 * n).ok_or_else(bad);
        out.push(Section {
            name,
            virtual_size: field(0)? as usize,
            va: base + field(1)? as u64,
            raw_size: field(2)? as usize,
            file_offset: field(3)? as usize,
        });
    }
    Ok((base, out))
}

/// A decoded x86 operand, reduced to what the tools look at.
#[derive(Clone, Copy, Debug)]
pub enum Operand {
    Reg,
    Imm(i64),
    /// Memory operand; register IDs are Capstone's (0 = none).
    Mem { base: u16, index: u16, disp: i64 },
}

/// A decoded x86 instruction, detached from Capstone's buffers.
#[derive(Clone, Debug)]
pub struct X86Insn {
    pub address: u64,
    pub size: usize,
    pub mnemonic: String,
    pub op_str: String,
    pub operands: Vec<Operand>,
}

/// A 32-bit x86 Capstone handle with operand details.
pub fn x86_capstone() -> capstone::Capstone {
    use capstone::prelude::*;
    Capstone::new()
        .x86()
        .mode(arch::x86::ArchMode::Mode32)
        .detail(true)
        .build()
        .expect("capstone: x86 support missing")
}

/// Linear sweep of `buf` loaded at `va`, resuming one byte past anything
/// Capstone cannot decode (jump tables and data embedded in code).
pub fn x86_sweep(cs: &capstone::Capstone, buf: &[u8], va: u64) -> Vec<X86Insn> {
    use capstone::arch::x86::X86OperandType;
    use capstone::arch::ArchOperand;
    let mut out = Vec::new();
    let mut off = 0usize;
    while off < buf.len() {
        let insns = match cs.disasm_all(&buf[off..], va + off as u64) {
            Ok(i) => i,
            Err(_) => {
                off += 1;
                continue;
            }
        };
        if insns.is_empty() {
            off += 1;
            continue;
        }
        for i in insns.iter() {
            let operands = cs
                .insn_detail(i)
                .map(|d| {
                    d.arch_detail()
                        .operands()
                        .into_iter()
                        .map(|op| match op {
                            ArchOperand::X86Operand(x) => match x.op_type {
                                X86OperandType::Imm(v) => Operand::Imm(v),
                                X86OperandType::Mem(m) => Operand::Mem {
                                    base: m.base().0,
                                    index: m.index().0,
                                    disp: m.disp(),
                                },
                                _ => Operand::Reg,
                            },
                            _ => Operand::Reg,
                        })
                        .collect()
                })
                .unwrap_or_default();
            out.push(X86Insn {
                address: i.address(),
                size: i.len(),
                mnemonic: i.mnemonic().unwrap_or("").to_string(),
                op_str: i.op_str().unwrap_or("").to_string(),
                operands,
            });
            off = (i.address() - va) as usize + i.len();
        }
    }
    out
}

/// Python-style `hex()`: 0x-prefixed, minus sign in front.
pub fn py_hex(v: i64) -> String {
    if v < 0 {
        format!("-{:#x}", -(v as i128))
    } else {
        format!("{v:#x}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ints_parse_like_python() {
        assert_eq!(parse_int("0x6fac"), Some(0x6fac));
        assert_eq!(parse_int("0b101"), Some(5));
        assert_eq!(parse_int("42"), Some(42));
        assert_eq!(parse_int("zz"), None);
    }
}
