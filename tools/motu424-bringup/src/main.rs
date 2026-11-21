// SPDX-License-Identifier: GPL-2.0-or-later
//! motu424-bringup - replay the vendor driver's bring-up of a MOTU PCI-424
//! (137a:0004) from userspace, phase by phase, and report what the card's
//! firmware publishes.
//!
//! Hardware model (verified against TI SPRU581C / SPRS219J): the card's PCI
//! interface is the PCI port of a TMS320C6412 DSP in PCI boot mode.
//!   BAR2 (I/O, 16 B): +0 HSR, +4 HDCR, +8 DSPP
//!   BAR1 (8 MB):      DSP register space 0x01800000..  ("window A")
//!   BAR0 (4 MB):      DSP memory page selected by DSPP ("window B")
//!
//! Sequence (vendor fn 0x2c150 for DEV_0004, read from the 32-bit MOTUAW.sys):
//!   P1  HDCR <- 1                       WARMRESET
//!   P2  FPGA image out through the DSP's GPIO (GPEN/GPDIR/GPVAL at window-A
//!       0x300000/4/8): start, 37,177 bytes LSB-first, 1000 trailing clocks
//!   P3  DSPP <- 0; zero-fill DSP RAM 0..0x40000; write the 28,272-byte DSP
//!       program at 0 (+1 zero dword, as the vendor does); read it back
//!   P4  [vendor: HSR <- 0 (unmask PINTA#)]  skipped unless --unmask
//!       mailbox 0x3fffc <- 0; HDCR <- 2 (DSPINT releases the CPU);
//!       poll mailbox until non-zero  ->  audio_base
//!   P5  read-only: audio_base+0/4/8/0x14/0x18 and a dump of the block
//! The vendor's later steps (mixer zeroing, rate registers, +0x50, CECTL0)
//! are deliberately NOT performed.
//!
//! SAFETY
//!   - Default is --dry-run: prints every write it would do, performs none.
//!   - --execute asks for "yes" before each phase (unless --yes).
//!   - Every write goes through one function with a hard allow-list:
//!     window A: only 0x300000, 0x300004, 0x300008 (GPIO)
//!     window B: only 0x000000..0x03ffff while DSPP == 0 (DSP internal RAM)
//!     ports:    HSR <- 0 only, HDCR <- 1|2 only, DSPP <- 0 only
//!     The DSP EEPROM controller (window A 0x420000..) is unreachable by
//!     construction.
//!   - Refuses to run if a kernel driver is bound, if PCIBOOT/EEREAD do not
//!     read as expected, or if the firmware files do not match their sha256.
//!   - Window-B reads are confined to DSP RAM 0..0x3ffff (the rest of page 0
//!     is reserved on the C6412); port and GPIO reads are unrestricted.
//!
//! Run from the repository root:
//!        sudo motu424-bringup                 # dry run
//!        sudo motu424-bringup --execute [--until N] [--yes] [--unmask]

use motu424_card::{
    find_card, hexdump_words, map_whole, sha256_hex, Mapping, PortBar, PORT_DSPP, PORT_HDCR,
    PORT_HSR, RAM_LEN, WINA_LEN, WINB_LEN,
};
use std::io::{self, BufRead, Write};
use std::process;
use std::thread::sleep;
use std::time::{Duration, Instant};

const FW_FPGA: &str = "vendor/firmware/pci424-serial-container.bin";
const FW_FPGA_LEN: usize = 37177;
const FW_FPGA_SHA: &str = "34e62cf5a75f53a41004129ff1fe6dc435a87e14614d79edc6ce3916e18e04e2";
const FW_PROG: &str = "vendor/firmware/pci424-program.bin";
const FW_PROG_LEN: usize = 28272;
const FW_PROG_SHA: &str = "3a23bc97e49154e5eba8956681ee8dd0212cd7091e8d677c272ccb1a7faa6e4f";

const HSR_EEREAD: u32 = 1 << 4;
const HSR_CFGERR: u32 = 1 << 3;
const HSR_INTAM: u32 = 1 << 2;
const HSR_INTAVAL: u32 = 1 << 1;
const HDCR_WARMRESET: u32 = 1;
const HDCR_DSPINT: u32 = 2;
const HDCR_PCIBOOT: u32 = 4;

const WA_GPEN: u32 = 0x30_0000; // card 0x01b00000
const WA_GPDIR: u32 = 0x30_0004;
const WA_GPVAL: u32 = 0x30_0008;
const WA_GP_MASK: u32 = 0xe0; // GP5 DATA0, GP6 nCONFIG, GP7 DCLK
const MAILBOX: u32 = 0x3fffc; // [table+0x30] & 0x3fffff, with DSPP = 0

fn die(msg: impl AsRef<str>) -> ! {
    eprintln!("{}", msg.as_ref());
    process::exit(1);
}

fn udelay(us: u64) {
    sleep(Duration::from_micros(us));
}

/// [off, off+len) lies inside DSP internal RAM and is dword-aligned.
fn in_ram(off: u32, len: u32) -> bool {
    off.is_multiple_of(4) && off < RAM_LEN && len <= RAM_LEN - off
}

struct Card {
    dry: bool,
    yes: bool,
    unmask: bool,
    port: PortBar,
    win_a: Mapping,
    win_b: Mapping,
    cur_dspp: u32,
    n_writes: u64,
    t0: Instant,
}

impl Card {
    // ---------- the only write paths, each with its allow-list ----------

    fn port_write(&mut self, off: u64, val: u32) {
        let ok = (off == PORT_HSR && val == 0)
            || (off == PORT_HDCR && (val == HDCR_WARMRESET || val == HDCR_DSPINT))
            || (off == PORT_DSPP && val == 0);
        if !ok {
            die(format!("REFUSED port write +0x{off:x} <- 0x{val:x} (not on allow-list)"));
        }
        self.n_writes += 1;
        if off == PORT_DSPP {
            self.cur_dspp = val; // tracked in dry run too
        }
        if self.dry {
            return;
        }
        if let Err(e) = self.port.write32(off, val) {
            die(format!("port write +0x{off:x} failed: {e}"));
        }
    }

    fn port_read(&self, off: u64) -> u32 {
        self.port
            .read32(off)
            .unwrap_or_else(|e| die(format!("port read +0x{off:x} failed: {e}")))
    }

    fn wina_write(&mut self, off: u32, val: u32) {
        if off != WA_GPEN && off != WA_GPDIR && off != WA_GPVAL {
            die(format!(
                "REFUSED window-A write 0x{off:06x} (only GPIO 0x300000/4/8 allowed)"
            ));
        }
        // only GP5..GP7 (DATA0, nCONFIG, DCLK) may be enabled or driven
        if (off == WA_GPVAL && (val & !WA_GP_MASK) != 0)
            || (off != WA_GPVAL && val != 0xe0 && val != 0xc0)
        {
            die(format!(
                "REFUSED window-A write 0x{off:06x} <- 0x{val:x} (value not on allow-list)"
            ));
        }
        self.n_writes += 1;
        if self.dry {
            return;
        }
        self.win_a.write32(off as usize, val);
    }

    fn winb_write(&mut self, off: u32, val: u32) {
        if self.cur_dspp != 0 || off >= RAM_LEN || !off.is_multiple_of(4) {
            die(format!(
                "REFUSED window-B write 0x{off:06x} with DSPP={} (only DSP RAM 0..0x3ffff at page 0)",
                self.cur_dspp
            ));
        }
        self.n_writes += 1;
        if self.dry {
            return;
        }
        self.win_b.write32(off as usize, val);
    }

    fn wina_read(&self, off: u32) -> u32 {
        self.win_a.read32(off as usize)
    }

    fn winb_read(&self, off: u32) -> u32 {
        // 0x40000..0x3fffff is reserved on the C6412: never read it over PCI
        if off >= RAM_LEN || !off.is_multiple_of(4) {
            die(format!("REFUSED window-B read 0x{off:06x} (only DSP RAM 0..0x3ffff)"));
        }
        self.win_b.read32(off as usize)
    }

    // ---------- helpers ----------

    fn hexdump_b(&self, off: u32, len: u32) {
        let words: Vec<u32> = (0..len / 4).map(|i| self.winb_read(off + 4 * i)).collect();
        hexdump_words(off as u64, &words);
    }

    fn confirm(&self, what: &str) -> bool {
        if self.dry {
            println!("  [dry run] {what}");
            return true;
        }
        if self.yes {
            return true;
        }
        print!("  >>> {what}. Type yes to proceed: ");
        let _ = io::stdout().flush();
        let mut line = String::new();
        if io::stdin().lock().read_line(&mut line).is_err() {
            return false;
        }
        line == "yes\n"
    }

    fn show_ports(&self, tag: &str) {
        let hsr = self.port_read(PORT_HSR);
        let hdcr = self.port_read(PORT_HDCR);
        let dspp = self.port_read(PORT_DSPP);
        let b = |v: u32, m: u32| u32::from(v & m != 0);
        println!(
            "  [{:8.3} s {tag}] HSR=0x{hsr:02x} (EEREAD={} CFGERR={} INTAM={} INTAVAL={}) HDCR=0x{hdcr:02x} (PCIBOOT={}) DSPP=0x{dspp:x}",
            self.t0.elapsed().as_secs_f64(),
            b(hsr, HSR_EEREAD),
            b(hsr, HSR_CFGERR),
            b(hsr, HSR_INTAM),
            b(hsr, HSR_INTAVAL),
            b(hdcr, HDCR_PCIBOOT)
        );
    }

    // ---------- phases ----------

    fn phase1_reset(&mut self) {
        println!("\nP1  WARMRESET: HDCR <- 1, then wait 1 ms (TI: 16 PCI clocks)");
        if !self.confirm("1 port write") {
            process::exit(2);
        }
        self.port_write(PORT_HDCR, HDCR_WARMRESET);
        if !self.dry {
            udelay(1000);
            self.show_ports("after reset");
        }
    }

    fn phase2_fpga(&mut self, img: &[u8]) {
        println!(
            "\nP2  FPGA image through GPIO: {FW_FPGA_LEN} bytes x 24 writes + start/finish = {} window-A writes",
            FW_FPGA_LEN * 24 + 4 + 2001
        );
        println!(
            "    GPEN<-0xe0 GPDIR<-0xe0 GPVAL<-0 (100us) GPVAL<-0x40 (5ms); per bit: v=0x40|(b?0x20:0): v, v|0x80, v;\n    then 1000 x (0xc0,0x40); GPDIR<-0xc0"
        );
        if !self.dry {
            println!(
                "  [before] GPEN=0x{:x} GPDIR=0x{:x} GPVAL=0x{:x}",
                self.wina_read(WA_GPEN),
                self.wina_read(WA_GPDIR),
                self.wina_read(WA_GPVAL)
            );
        }
        if !self.confirm("~894k GPIO writes (vendor fn 0x251e0/0x25170/0x25230)") {
            process::exit(2);
        }
        self.wina_write(WA_GPEN, 0xe0);
        self.wina_write(WA_GPDIR, 0xe0);
        self.wina_write(WA_GPVAL, 0x00);
        if !self.dry {
            udelay(100); // nCONFIG low; Altera minimum is a few us
        }
        self.wina_write(WA_GPVAL, 0x40);
        if !self.dry {
            udelay(5000); // nCONFIG high -> nSTATUS release before the first DCLK
        }
        for &b in &img[..FW_FPGA_LEN] {
            for bit in 0..8 {
                let v = 0x40 | if (b >> bit) & 1 != 0 { 0x20 } else { 0 };
                self.wina_write(WA_GPVAL, v);
                self.wina_write(WA_GPVAL, v | 0x80);
                self.wina_write(WA_GPVAL, v);
            }
        }
        for _ in 0..1000 {
            self.wina_write(WA_GPVAL, 0xc0);
            self.wina_write(WA_GPVAL, 0x40);
        }
        self.wina_write(WA_GPDIR, 0xc0);
        if !self.dry {
            self.show_ports("after FPGA");
            println!(
                "  [after]  GPEN=0x{:x} GPDIR=0x{:x} GPVAL=0x{:x} (GP5 now an input; its level is bit 5)",
                self.wina_read(WA_GPEN),
                self.wina_read(WA_GPDIR),
                self.wina_read(WA_GPVAL)
            );
        }
    }

    fn phase3_program(&mut self, prog: &[u8]) {
        // vendor: (len>>2)+1 dwords from a zeroed bounce buffer; prog has 4 zero bytes appended
        let ndw = (FW_PROG_LEN / 4 + 1) as u32;
        let word = |i: u32| {
            let o = 4 * i as usize;
            u32::from_le_bytes([prog[o], prog[o + 1], prog[o + 2], prog[o + 3]])
        };
        println!(
            "\nP3  DSPP <- 0; zero-fill DSP RAM 0..0x{RAM_LEN:x} ({} writes); write program at 0 ({ndw} dwords); verify by read-back",
            RAM_LEN / 4
        );
        if !self.dry {
            println!("  [before] window B head:");
            self.hexdump_b(0, 32);
        }
        if !self.confirm("1 port write + 65536 + 7069 window-B writes") {
            process::exit(2);
        }
        self.port_write(PORT_DSPP, 0);
        for off in (0..RAM_LEN).step_by(4) {
            self.winb_write(off, 0);
        }
        for i in 0..ndw {
            self.winb_write(4 * i, word(i));
        }
        if self.dry {
            return;
        }
        let bad = (0..ndw).filter(|&i| self.winb_read(4 * i) != word(i)).count();
        let z = ((FW_PROG_LEN as u32 + 4)..RAM_LEN)
            .step_by(4)
            .filter(|&off| self.winb_read(off) != 0)
            .count();
        println!(
            "  read-back: {bad}/{ndw} program dwords differ, {z} non-zero dwords in the cleared tail, mailbox=0x{:08x}",
            self.winb_read(MAILBOX)
        );
        if bad != 0 || z != 0 {
            println!("  !! DSP RAM did not hold what was written. Window B is not decoding; STOP here.");
            process::exit(3);
        }
        println!("  window B now decodes addresses: writes stick. Head:");
        self.hexdump_b(0, 32);
        self.show_ports("after program load");
    }

    fn phase4_release(&mut self) -> u32 {
        println!(
            "\nP4  {}; mailbox 0x{MAILBOX:x} <- 0; HDCR <- 2 (DSPINT: release CPU); poll mailbox up to 3 s",
            if self.unmask {
                "HSR <- 0 (unmask PINTA#, as the vendor does)"
            } else {
                "(HSR unmask SKIPPED: no driver to service IRQs; use --unmask to follow the vendor exactly)"
            }
        );
        let what = if self.unmask {
            "2 port writes + 1 window-B write"
        } else {
            "1 port write + 1 window-B write"
        };
        if !self.confirm(what) {
            process::exit(2);
        }
        if self.unmask {
            self.port_write(PORT_HSR, 0);
        }
        self.winb_write(MAILBOX, 0);
        // read back: flushes the posted write through the bridge before DSPINT
        if !self.dry && self.winb_read(MAILBOX) != 0 {
            die(format!(
                "mailbox did not clear (0x{:08x}); not releasing the CPU",
                self.winb_read(MAILBOX)
            ));
        }
        self.port_write(PORT_HDCR, HDCR_DSPINT);
        if self.dry {
            return 0;
        }
        let t0 = Instant::now();
        let mut v;
        loop {
            v = self.winb_read(MAILBOX);
            if v != 0 || t0.elapsed() > Duration::from_secs(3) {
                break;
            }
            udelay(5); // the deadline is wall-clock
        }
        let waited = t0.elapsed().as_secs_f64();
        self.show_ports("after DSPINT");
        if v == 0 {
            println!("  mailbox stayed 0 after 3 s. DSP did not publish. RAM head now:");
            self.hexdump_b(0, 32);
            println!(
                "  (the vendor would now WARMRESET and give up; do a full power cycle before retrying;\n   if the program itself changed the RAM head above, the CPU did run: try --unmask next)"
            );
            return 0;
        }
        println!("  *** audio_base = 0x{v:08x} (published {waited:.3} s after DSPINT)");
        v
    }

    fn phase5_inspect(&self, ab: u32) {
        println!("\nP5  read-only inspection of the published block");
        if self.dry {
            println!("  [dry run] would read audio_base+0/4/8/0x14/0x18 and dump 0x140 bytes");
            return;
        }
        if !in_ram(ab, 0x140) {
            println!(
                "  audio_base 0x{ab:08x} (+0x140) is outside DSP RAM 0..0x3ffff (or unaligned); not reading it blind."
            );
            return;
        }
        println!("  mix_base = 0x{:08x}   (+0)", self.winb_read(ab));
        for o in [4u32, 8, 0x14, 0x18] {
            println!("  +0x{o:02x}    = 0x{:08x}", self.winb_read(ab + o));
        }
        println!("  block dump:");
        self.hexdump_b(ab, 0x140);
        let mb = self.winb_read(ab);
        if in_ram(mb, 0xb4) {
            println!("  mix block (45 dwords):");
            self.hexdump_b(mb, 0xb4);
        } else {
            println!("  mix_base 0x{mb:08x} (+0xb4) is outside DSP RAM (or unaligned); not dumped.");
        }
        println!("  mailbox again: 0x{:08x}; RAM head:", self.winb_read(MAILBOX));
        self.hexdump_b(0, 32);
    }
}

/// Read a firmware image, check its length and SHA-256, append 4 zero bytes.
fn load_fw(path: &str, want: usize, sha: &str) -> Vec<u8> {
    let mut buf = std::fs::read(path).unwrap_or_else(|_| {
        die(format!(
            "cannot open {path} (run from the repository root, after: tools/target/release/extract-firmware)"
        ))
    });
    if buf.len() != want {
        die(format!("{path}: expected {want} bytes, got {}", buf.len()));
    }
    let got = sha256_hex(&buf);
    if got != sha {
        die(format!("{path}: sha256 mismatch ({got})"));
    }
    buf.extend_from_slice(&[0; 4]);
    println!("  {path}: {want} bytes, sha256 ok");
    buf
}

fn main() {
    motu424_card::restore_sigpipe();
    let mut dry = true;
    let mut yes = false;
    let mut unmask = false;
    let mut until = 5;
    let mut bdf = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--execute" => dry = false,
            "--dry-run" => dry = true,
            "--yes" => yes = true,
            "--unmask" => unmask = true,
            "--until" => {
                until = args
                    .next()
                    .and_then(|n| n.parse().ok())
                    .unwrap_or_else(|| die("--until needs a phase number"))
            }
            s if !s.starts_with('-') => bdf = Some(s.to_string()),
            _ => die("usage: motu424-bringup [BDF] [--execute] [--until N] [--yes] [--unmask]"),
        }
    }

    let dev = find_card(bdf.as_deref()).unwrap_or_else(|e| die(e));
    println!(
        "device {}  mode: {}{}",
        dev.dir.display(),
        if dry { "DRY RUN (no writes)" } else { "EXECUTE" },
        if unmask { " --unmask" } else { "" }
    );
    if dev.driver_bound() {
        die("a kernel driver is bound to the device; rmmod it first");
    }
    if dev.device() != 0x0004 {
        die(format!(
            "device id 0x{:04x} is not 0x0004; this tool knows only the PCI-424 table",
            dev.device()
        ));
    }

    println!("firmware:");
    let fpga = load_fw(FW_FPGA, FW_FPGA_LEN, FW_FPGA_SHA);
    let prog = load_fw(FW_PROG, FW_PROG_LEN, FW_PROG_SHA);

    let writable = !dry;
    let port = PortBar::open(&dev.resource_path(2), writable).unwrap_or_else(|e| {
        die(format!("open {}: {e} (need root)", dev.resource_path(2).display()))
    });
    let win_a = map_whole(&dev.resource_path(1), WINA_LEN, writable)
        .unwrap_or_else(|e| die(format!("{e}: not the expected window A")));
    let win_b = map_whole(&dev.resource_path(0), WINB_LEN, writable)
        .unwrap_or_else(|e| die(format!("{e}: not the expected window B")));

    let mut card = Card {
        dry,
        yes,
        unmask,
        port,
        win_a,
        win_b,
        cur_dspp: 0,
        n_writes: 0,
        t0: Instant::now(),
    };

    println!("pre-flight (reads):");
    card.show_ports("now");
    let hsr = card.port_read(PORT_HSR);
    let hdcr = card.port_read(PORT_HDCR);
    card.cur_dspp = card.port_read(PORT_DSPP) & 0x3ff;
    if hdcr & HDCR_PCIBOOT == 0 || hsr & HSR_EEREAD == 0 || hsr & HSR_CFGERR != 0 {
        die("pre-flight failed: expected PCIBOOT=1 EEREAD=1 CFGERR=0");
    }
    println!(
        "  GPEN=0x{:x} GPDIR=0x{:x} GPVAL=0x{:x}  CECTL0=0x{:08x}",
        card.wina_read(WA_GPEN),
        card.wina_read(WA_GPDIR),
        card.wina_read(WA_GPVAL),
        card.wina_read(8)
    );

    let mut ab = 0;
    if until >= 1 {
        card.phase1_reset();
    }
    if until >= 2 {
        card.phase2_fpga(&fpga);
    }
    if until >= 3 {
        card.phase3_program(&prog);
    }
    if until >= 4 {
        ab = card.phase4_release();
    }
    if until >= 5 && (ab != 0 || dry) {
        card.phase5_inspect(ab);
    }

    println!(
        "\ndone: {} writes {} through phase {until}.",
        card.n_writes,
        if dry { "planned" } else { "performed" }
    );
    if !dry {
        println!("Full power-off before booting Windows; the DSP state is volatile.");
    }
}
