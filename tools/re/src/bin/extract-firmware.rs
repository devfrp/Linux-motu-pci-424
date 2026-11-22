// SPDX-License-Identifier: GPL-2.0-or-later
//! extract-firmware - pull the card firmware images out of YOUR OWN copy of
//! the vendor driver MOTUAW.sys (MOTU Audio Installer 4.0.6.6814).
//!
//! The vendor driver carries the FPGA serial image and the card program inside
//! its .data section and uploads them at every device start. They are MOTU's
//! copyrighted firmware, so this repository ships only the facts needed to find
//! them (offsets, lengths, hashes), never the bytes. Output goes to
//! vendor/firmware/, which is git-ignored.
//!
//!     extract-firmware [path/to/MOTUAW.sys ...]
//!
//! With no argument it tries vendor/x86/MOTUAW.sys and vendor/MOTUAW.sys. Both
//! the 32-bit and the 64-bit build are recognised by file hash; every image is
//! verified by sha256 before it is written.

use motu424_card::sha256_hex;
use std::path::Path;
use std::process::ExitCode;

const BUILDS: [(&str, Build); 2] = [
    (
        "37d68dedde71ff9d9a436f257b5da1eac8b3e031d992c0be5cab17b2d8d8ad0e",
        Build::X86,
    ),
    (
        "fbbb78cc706caef1d310ec12c503854f307ca4ce6f9c4b8054936d8181607a45",
        Build::X64,
    ),
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Build {
    X86,
    X64,
}

impl Build {
    fn name(self) -> &'static str {
        match self {
            Build::X86 => "x86",
            Build::X64 => "x64",
        }
    }
}

struct Image {
    name: &'static str,
    off_x86: usize,
    off_x64: Option<usize>,
    len: usize,
    sha: &'static str,
}

const IMAGES: [Image; 6] = [
    Image {
        name: "pci424-serial-container.bin",
        off_x86: 0x257d0,
        off_x64: Some(0x2d8c0),
        len: 37177,
        sha: "34e62cf5a75f53a41004129ff1fe6dc435a87e14614d79edc6ce3916e18e04e2",
    },
    // data fork of the AppleSingle container
    Image {
        name: "pci424-bitstream.rbf",
        off_x86: 0x2580e,
        off_x64: Some(0x2d8fe),
        len: 36617,
        sha: "e20543f55ea8381e3b3980f396c49c8a3d74f4a26aecaf2226f3d61617cdef9b",
    },
    // written to card address 0
    Image {
        name: "pci424-program.bin",
        off_x86: 0x2e910,
        off_x64: Some(0x36a00),
        len: 28272,
        sha: "3a23bc97e49154e5eba8956681ee8dd0212cd7091e8d677c272ccb1a7faa6e4f",
    },
    Image {
        name: "pci324-ep1k30.rbf",
        off_x86: 0x35840,
        off_x64: Some(0x3d970),
        len: 59215,
        sha: "267b343dfc537d114625f8fb8fdea00fc6e1aeb51ea2e6d03fd8b99ee05a5256",
    },
    // written to card address 0
    Image {
        name: "pci324-program.bin",
        off_x86: 0x47610,
        off_x64: Some(0x4f740),
        len: 37696,
        sha: "1eae8bad11249f940960be08d1d9d4cb3f5abcda05d614969c137df4892e9c4b",
    },
    // written to card address 0x80000000
    Image {
        name: "pci324-data.bin",
        off_x86: 0x43f90,
        off_x64: None,
        len: 13952,
        sha: "02b75d8d65e0a19205550270411da0aa90b3828b351daa4bfba6213bee25f264",
    },
];

const OUT_DIR: &str = "vendor/firmware";

fn be16(b: &[u8], o: usize) -> u16 {
    u16::from_be_bytes([b[o], b[o + 1]])
}

fn be32(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

fn describe_applesingle(blob: &[u8]) {
    if blob.len() < 26 || be32(blob, 0) != 0x0005_1600 {
        return;
    }
    let version = be32(blob, 4);
    let count = be16(blob, 24) as usize;
    println!("    AppleSingle v{version:#x}, {count} entries:");
    for k in 0..count {
        let o = 26 + 12 * k;
        if o + 12 > blob.len() {
            break;
        }
        let (eid, off, ln) = (be32(blob, o), be32(blob, o + 4), be32(blob, o + 8));
        let what = match eid {
            1 => "data fork",
            2 => "resource fork",
            9 => "Finder info",
            _ => "?",
        };
        println!("      id {eid} ({what}): offset {off:#x}, length {ln}");
    }
}

fn extract(path: &str) -> bool {
    let raw = match std::fs::read(path) {
        Ok(r) => r,
        Err(e) => {
            println!("{path}: {e}, skipped");
            return false;
        }
    };
    let hash = sha256_hex(&raw);
    let Some(&(_, build)) = BUILDS.iter().find(|(h, _)| *h == hash) else {
        println!("{path}: not a recognised 4.0.6.6814 MOTUAW.sys build, skipped");
        return false;
    };
    println!("{path}: {} build recognised", build.name());
    if let Err(e) = std::fs::create_dir_all(OUT_DIR) {
        println!("{OUT_DIR}: {e}");
        return false;
    }
    let mut ok = true;
    for img in &IMAGES {
        let off = match build {
            Build::X86 => Some(img.off_x86),
            Build::X64 => img.off_x64,
        };
        let Some(off) = off else {
            println!("  {}: no offset recorded for the {} build, skipped", img.name, build.name());
            continue;
        };
        let Some(blob) = raw.get(off..off + img.len) else {
            println!("  {}: file too short for {off:#x}+{}", img.name, img.len);
            ok = false;
            continue;
        };
        let got = sha256_hex(blob);
        if got != img.sha {
            println!("  {}: HASH MISMATCH at {off:#x} (got {}...)", img.name, &got[..16]);
            ok = false;
            continue;
        }
        let out = Path::new(OUT_DIR).join(img.name);
        if let Err(e) = std::fs::write(&out, blob) {
            println!("  {}: {e}", out.display());
            ok = false;
            continue;
        }
        println!("  {}: {} bytes from {off:#x}, sha256 ok", img.name, img.len);
        if img.name.ends_with("container.bin") {
            describe_applesingle(blob);
        }
    }
    ok
}

fn main() -> ExitCode {
    motu424_card::restore_sigpipe();
    let mut paths: Vec<String> = std::env::args().skip(1).collect();
    if paths.is_empty() {
        paths = ["vendor/x86/MOTUAW.sys", "vendor/MOTUAW.sys"]
            .iter()
            .filter(|p| Path::new(p).exists())
            .map(|p| p.to_string())
            .collect();
    }
    if paths.is_empty() {
        eprintln!(
            "usage: extract-firmware [path/to/MOTUAW.sys ...]\n\
             (no argument: tries vendor/x86/MOTUAW.sys and vendor/MOTUAW.sys)"
        );
        return ExitCode::FAILURE;
    }
    let results: Vec<bool> = paths.iter().map(|p| extract(p)).collect();
    println!("output directory: {OUT_DIR} (git-ignored)");
    if results.iter().all(|&r| r) {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
