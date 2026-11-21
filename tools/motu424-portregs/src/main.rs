// SPDX-License-Identifier: GPL-2.0-or-later
//! motu424-portregs - READ the PCI-424's three I/O-port registers and decode
//! them per TI SPRU581C (the card's PCI interface is a TMS320C6412 DSP).
//!
//!     sudo motu424-portregs [0000:02:01.0]
//!
//! Read-only: it opens the sysfs I/O-port resource read-only and uses pread.
//! Per SPRU581C none of these reads has side effects (HSR.INTSRC is cleared
//! only by writing 1 to it; HDCR's write-only bits read as 0).

use motu424_card::{find_card, PortBar, PORT_DSPP, PORT_HDCR, PORT_HSR};
use std::process::ExitCode;

fn run() -> Result<(), String> {
    let bdf = std::env::args().nth(1);
    let dev = find_card(bdf.as_deref())?;
    let idx = dev
        .find_bar(|b| b.is_io() && b.start != 0)
        .ok_or("no I/O-port BAR on this device")?;
    let path = dev.resource_path(idx);
    let port = PortBar::open(&path, false).map_err(|e| format!("{}: {e}", path.display()))?;
    let rd = |off| port.read32(off).map_err(|e| format!("read +0x{off:x}: {e}"));
    let (hsr, hdcr, dspp) = (rd(PORT_HSR)?, rd(PORT_HDCR)?, rd(PORT_DSPP)?);
    let bit = |v: u32, n: u32| (v >> n) & 1;

    println!("{} I/O BAR {}", dev.bdf, path.display());
    println!("  +0x0 HSR  = 0x{hsr:08x}");
    println!("        bit4 EEREAD  = {}   (1 = PCI config registers were loaded from the EEPROM)", bit(hsr, 4));
    println!("        bit3 CFGERR  = {}   (1 = EEPROM autoinit error)", bit(hsr, 3));
    println!("        bit2 INTAM   = {}   (1 = PINTA# masked)", bit(hsr, 2));
    println!("        bit1 INTAVAL = {}   (1 = PINTA# currently asserted)", bit(hsr, 1));
    println!("        bit0 INTSRC  = {}   (1 = DSP raised an interrupt since last clear)", bit(hsr, 0));
    println!("  +0x4 HDCR = 0x{hdcr:08x}");
    println!("        bit2 PCIBOOT = {}   (1 = DSP boots from PCI, CPU stalled until DSPINT)", bit(hdcr, 2));
    println!(
        "  +0x8 DSPP = 0x{dspp:08x}   (DSP page: window B maps DSP address 0x{:08x})",
        (dspp & 0x3ff) << 22
    );
    println!();
    if bit(hdcr, 2) == 1 && bit(hsr, 4) == 1 && bit(hsr, 3) == 0 {
        println!("VERDICT: matches the model: PCI boot mode, identity from EEPROM, no config error.");
    } else {
        println!("VERDICT: does NOT fully match the expected PCIBOOT=1 / EEREAD=1 / CFGERR=0; stop and review before any write.");
    }
    Ok(())
}

fn main() -> ExitCode {
    motu424_card::restore_sigpipe();
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
