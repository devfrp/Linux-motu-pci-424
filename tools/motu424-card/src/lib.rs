// SPDX-License-Identifier: GPL-2.0-or-later
//! Shared userspace access to a MOTU PCI-324/424 for the motu424 tools.
//!
//! The card is found in `/sys/bus/pci/devices` by its vendor ID (0x137a).
//! Its BARs are reached through the sysfs `resourceN` files: the two MMIO
//! windows by `mmap`, the small I/O-port BAR (the DSP's HPI registers) by
//! `pread`/`pwrite`. Nothing here writes to the card on its own; the tools
//! decide what to write and keep their own allow-lists.
//!
//! The hardware model is in `docs/register-map.md`: window A (8 MB) is the
//! DSP's peripheral register space, window B (4 MB) the DSP memory page that
//! DSPP selects, with internal RAM at `0..0x40000` on page 0.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::fs::FileExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

/// Mark of the Unicorn's PCI vendor ID (MOTUAW.inf).
pub const MOTU_VENDOR: u32 = 0x137a;
/// Where the kernel lists PCI functions.
pub const SYS_PCI: &str = "/sys/bus/pci/devices";

/// Number of standard BARs.
pub const NUM_BARS: usize = 6;
/// IORESOURCE flag bits as the sysfs `resource` file shows them.
pub const RES_IO: u64 = 0x100;
pub const RES_MEM: u64 = 0x200;

/// Window sizes and masks (vendor accessors 0x29110/0x29160).
pub const WINA_LEN: u64 = 8 << 20;
pub const WINB_LEN: u64 = 4 << 20;
pub const WINB_MASK: u64 = 0x003f_ffff;
/// C6412 internal RAM in window B with DSPP = 0; the rest of page 0 is reserved.
pub const RAM_LEN: u32 = 0x40000;

/// HPI registers in the I/O-port BAR (TI SPRU581C).
pub const PORT_HSR: u64 = 0x0;
pub const PORT_HDCR: u64 = 0x4;
pub const PORT_DSPP: u64 = 0x8;

/// Human name of a MOTU device ID.
pub fn model_name(device: u32) -> &'static str {
    match device {
        0x0003 => "PCI-324",
        0x0004 => "PCI-424",
        0x0005 => "PCIe-424 (HD Express)",
        _ => "unknown/other",
    }
}

/// One BAR as listed in the sysfs `resource` file.
#[derive(Clone, Copy, Debug, Default)]
pub struct Bar {
    pub start: u64,
    pub end: u64,
    pub flags: u64,
    pub size: u64,
}

impl Bar {
    pub fn is_io(&self) -> bool {
        self.flags & RES_IO != 0
    }

    pub fn is_mem(&self) -> bool {
        self.flags & RES_MEM != 0
    }

    /// Role in the windowed model, from the BAR's type and size.
    pub fn classify(&self) -> &'static str {
        if self.is_io() {
            "I/O-port bridge (port BAR)"
        } else if self.is_mem() {
            if self.size >= 0x80_0000 {
                "window A (>=8 MB MMIO)"
            } else if self.size >= 0x20_0000 {
                "window B (~4 MB MMIO, audio aperture)"
            } else {
                "MMIO (small)"
            }
        } else {
            "unknown"
        }
    }

    /// Window B: an MMIO BAR of at least 2 MB and under 8 MB.
    pub fn is_window_b(&self) -> bool {
        self.is_mem() && self.size >= 0x20_0000 && self.size < 0x80_0000
    }
}

/// A PCI function under `/sys/bus/pci/devices`.
#[derive(Clone, Debug)]
pub struct Device {
    pub bdf: String,
    pub dir: PathBuf,
}

/// Parse a sysfs hex file such as `vendor` ("0x137a\n").
pub fn read_hex(path: &Path) -> Option<u64> {
    let s = fs::read_to_string(path).ok()?;
    let s = s.trim();
    u64::from_str_radix(s.trim_start_matches("0x"), 16).ok()
}

/// Parse a number the way C's `strtoul(s, NULL, 0)` does (0x.., 0.., decimal).
pub fn parse_num(s: &str) -> Option<u64> {
    let s = s.trim();
    if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u64::from_str_radix(h, 16).ok()
    } else if s.len() > 1 && s.starts_with('0') {
        u64::from_str_radix(&s[1..], 8).ok()
    } else {
        s.parse().ok()
    }
}

impl Device {
    pub fn new(bdf: &str) -> Self {
        Device {
            bdf: bdf.to_string(),
            dir: Path::new(SYS_PCI).join(bdf),
        }
    }

    pub fn exists(&self) -> bool {
        self.dir.exists()
    }

    pub fn vendor(&self) -> u32 {
        read_hex(&self.dir.join("vendor")).unwrap_or(0) as u32
    }

    pub fn device(&self) -> u32 {
        read_hex(&self.dir.join("device")).unwrap_or(0) as u32
    }

    /// A kernel driver is bound to the function.
    pub fn driver_bound(&self) -> bool {
        self.dir.join("driver").exists()
    }

    /// The BARs from the sysfs `resource` file (absent BARs have size 0).
    pub fn bars(&self) -> [Bar; NUM_BARS] {
        let mut bars = [Bar::default(); NUM_BARS];
        let Ok(text) = fs::read_to_string(self.dir.join("resource")) else {
            return bars;
        };
        for (i, line) in text.lines().take(NUM_BARS).enumerate() {
            let v: Vec<u64> = line
                .split_whitespace()
                .filter_map(|w| u64::from_str_radix(w.trim_start_matches("0x"), 16).ok())
                .collect();
            if v.len() < 3 {
                break;
            }
            let size = if v[0] != 0 { v[1] - v[0] + 1 } else { 0 };
            bars[i] = Bar {
                start: v[0],
                end: v[1],
                flags: v[2],
                size,
            };
        }
        bars
    }

    pub fn resource_path(&self, bar: usize) -> PathBuf {
        self.dir.join(format!("resource{bar}"))
    }

    /// Index of the first BAR matching `pred`.
    pub fn find_bar(&self, pred: impl Fn(&Bar) -> bool) -> Option<usize> {
        self.bars().iter().position(|b| b.size != 0 && pred(b))
    }
}

/// Every PCI function with the MOTU vendor ID, sorted by address.
pub fn find_cards() -> Vec<Device> {
    let mut out: Vec<Device> = match fs::read_dir(SYS_PCI) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .map(|e| Device::new(&e.file_name().to_string_lossy()))
            .filter(|d| d.vendor() == MOTU_VENDOR)
            .collect(),
        Err(_) => Vec::new(),
    };
    out.sort_by(|a, b| a.bdf.cmp(&b.bdf));
    out
}

/// The function named by `bdf`, or the first MOTU card.
pub fn find_card(bdf: Option<&str>) -> Result<Device, String> {
    match bdf {
        Some(b) => {
            let d = Device::new(b);
            if d.exists() {
                Ok(d)
            } else {
                Err(format!("no PCI device {b}"))
            }
        }
        None => find_cards()
            .into_iter()
            .next()
            .ok_or_else(|| format!("no MOTU ({MOTU_VENDOR:x}) device found")),
    }
}

/// A shared mapping of part of a BAR's `resourceN` file.
pub struct Mapping {
    ptr: *mut u8,
    len: usize,
}

// SAFETY: the mapping is plain device memory owned by this value; accesses go
// through volatile reads and writes.
unsafe impl Send for Mapping {}

impl Mapping {
    /// Map `len` bytes at `offset` (page-aligned) of a `resourceN` file.
    pub fn new(path: &Path, offset: u64, len: usize, writable: bool) -> io::Result<Self> {
        let file = OpenOptions::new().read(true).write(writable).open(path)?;
        let prot = if writable {
            libc::PROT_READ | libc::PROT_WRITE
        } else {
            libc::PROT_READ
        };
        // SAFETY: a fresh shared mapping of an open file; the kernel checks the
        // range against the BAR and the result is checked below.
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                prot,
                libc::MAP_SHARED,
                file.as_raw_fd(),
                offset as libc::off_t,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        Ok(Mapping {
            ptr: ptr.cast(),
            len,
        })
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Read the little-endian dword at byte offset `off`.
    pub fn read32(&self, off: usize) -> u32 {
        assert!(off.is_multiple_of(4) && off + 4 <= self.len, "read32 0x{off:x} out of the mapping");
        // SAFETY: in bounds and aligned (checked above), inside a live mapping.
        u32::from_le(unsafe { std::ptr::read_volatile(self.ptr.add(off).cast::<u32>()) })
    }

    /// Write the little-endian dword at byte offset `off`.
    pub fn write32(&self, off: usize, val: u32) {
        assert!(off.is_multiple_of(4) && off + 4 <= self.len, "write32 0x{off:x} out of the mapping");
        // SAFETY: in bounds and aligned (checked above), inside a live mapping
        // created writable by the caller.
        unsafe { std::ptr::write_volatile(self.ptr.add(off).cast::<u32>(), val.to_le()) }
    }

    /// Copy `len` bytes from offset `off` out of the mapping, dword by dword.
    pub fn read_bytes(&self, off: usize, len: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(len);
        let mut o = off;
        while o < off + len {
            out.extend_from_slice(&self.read32(o).to_le_bytes());
            o += 4;
        }
        out.truncate(len);
        out
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: unmaps exactly the range mapped in `new`.
        unsafe { libc::munmap(self.ptr.cast(), self.len) };
    }
}

/// Map a whole BAR, checking its size first.
pub fn map_whole(path: &Path, want_len: u64, writable: bool) -> Result<Mapping, String> {
    let len = fs::metadata(path)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .len();
    if len != want_len {
        return Err(format!(
            "{} is {} bytes, expected {}",
            path.display(),
            len,
            want_len
        ));
    }
    Mapping::new(path, 0, len as usize, writable).map_err(|e| format!("mmap {}: {e}", path.display()))
}

/// The I/O-port BAR, reached with `pread`/`pwrite` on its `resourceN` file.
pub struct PortBar {
    file: File,
    pub path: PathBuf,
}

impl PortBar {
    pub fn open(path: &Path, writable: bool) -> io::Result<Self> {
        let file = OpenOptions::new().read(true).write(writable).open(path)?;
        Ok(PortBar {
            file,
            path: path.to_path_buf(),
        })
    }

    pub fn read32(&self, off: u64) -> io::Result<u32> {
        let mut b = [0u8; 4];
        self.file.read_exact_at(&mut b, off)?;
        Ok(u32::from_le_bytes(b))
    }

    pub fn write32(&self, off: u64, val: u32) -> io::Result<()> {
        self.file.write_all_at(&val.to_le_bytes(), off)
    }
}

/// SHA-256 of `data` as lowercase hex.
pub fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Print `words` as rows of four dwords, labelled from `addr`.
pub fn hexdump_words(addr: u64, words: &[u32]) {
    for (row, chunk) in words.chunks(4).enumerate() {
        print!("    {:08x}:", addr + 16 * row as u64);
        for w in chunk {
            print!(" {w:08x}");
        }
        println!();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_parse_like_strtoul() {
        assert_eq!(parse_num("0x6fac"), Some(0x6fac));
        assert_eq!(parse_num("010"), Some(8));
        assert_eq!(parse_num("96"), Some(96));
        assert_eq!(parse_num("0"), Some(0));
    }

    #[test]
    fn sha256_matches_a_known_vector() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn bars_classify_by_size() {
        let b = Bar { start: 1, end: 0x40_0000, flags: RES_MEM, size: 0x40_0000 };
        assert!(b.is_window_b());
        let a = Bar { size: 0x80_0000, ..b };
        assert_eq!(a.classify(), "window A (>=8 MB MMIO)");
    }
}
