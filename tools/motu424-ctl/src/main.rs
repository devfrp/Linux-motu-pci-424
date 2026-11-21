// SPDX-License-Identifier: GPL-2.0-or-later
//! motu424-ctl - the Linux "CueMix FX": a userspace management/control app for
//! the MOTU PCI-324/424 ALSA driver (motu424).
//!
//! MOTU's CueMix FX is a no-latency monitor mixer + clock/format control that
//! runs on the card's DSP. On Linux the driver exposes that control set as ALSA
//! kcontrols (see docs/cuemix-control-map.md); this tool manages them through
//! alsa-lib, the same way alsamixer/amixer manage any card. It auto-locates the
//! MOTU card (driver "motu424"), and can list/get/set controls or print a
//! CueMix-style status overview.
//!
//!   motu424-ctl                       # status overview of the MOTU card
//!   motu424-ctl list                  # every kcontrol (name / type / value)
//!   motu424-ctl get 'Clock Source'
//!   motu424-ctl set 'Clock Source' Internal
//!   motu424-ctl set 'Mix 00 Input 03 Volume' 92
//!   motu424-ctl -D hw:1 list          # target a specific control device
//!
//! The output of `list` is what cuemix-studio parses; keep its format.
//!
//! The driver's mixer is card-gated (Phase 5); with no MOTU card, or a card
//! that only exposes PCM so far, the tool says so and lists whatever is present.

mod alsa;

use alsa::{find_motu_card, ElemType, HCtl, HElem, Info};
use std::fmt::Write as _;
use std::process::ExitCode;

const DRIVER_MATCH: &str = "motu424";

fn type_name(t: ElemType) -> &'static str {
    match t {
        ElemType::Boolean => "bool",
        ElemType::Integer => "int",
        ElemType::Integer64 => "int64",
        ElemType::Enumerated => "enum",
        ElemType::Bytes => "bytes",
        ElemType::Other => "?",
    }
}

/// An element's current value(s), comma-separated.
fn value_text(elem: &HElem, info: &mut Info) -> String {
    let Ok(val) = elem.read() else {
        return "<read error>".to_string();
    };
    let ty = info.elem_type();
    let mut out = String::new();
    for i in 0..info.count() {
        if i > 0 {
            out.push(',');
        }
        match ty {
            ElemType::Boolean => out.push_str(if val.boolean(i) { "on" } else { "off" }),
            ElemType::Integer => {
                let _ = write!(out, "{}", val.integer(i));
            }
            ElemType::Integer64 => {
                let _ = write!(out, "{}", val.integer64(i));
            }
            ElemType::Enumerated => {
                let idx = val.enumerated(i);
                match elem.item_name(info, idx) {
                    Some(name) => out.push_str(&name),
                    None => {
                        let _ = write!(out, "{idx}");
                    }
                }
            }
            other => {
                let _ = write!(out, "<{}>", type_name(other));
            }
        }
    }
    out
}

/// The range or enum items an element accepts, in brackets.
fn range_text(elem: &HElem, info: &mut Info) -> String {
    match info.elem_type() {
        ElemType::Integer => format!(" [{}..{}]", info.min(), info.max()),
        ElemType::Enumerated => {
            let names: Vec<String> = (0..info.items())
                .filter_map(|i| elem.item_name(info, i))
                .collect();
            format!(" {{{}}}", names.join("|"))
        }
        _ => String::new(),
    }
}

fn cmd_list(hctl: &HCtl) -> i32 {
    let mut count = 0;
    for elem in hctl.elems() {
        let Ok(mut info) = elem.info() else { continue };
        println!(
            "{:<32} {:<6} = {}{}",
            elem.name(),
            type_name(info.elem_type()),
            value_text(&elem, &mut info),
            range_text(&elem, &mut info)
        );
        count += 1;
    }
    if count == 0 {
        println!("(card exposes no controls yet)");
    }
    0
}

fn cmd_get(hctl: &HCtl, name: &str) -> i32 {
    let Some(elem) = hctl.find(name) else {
        eprintln!("no such control: {name}");
        return 1;
    };
    let Ok(mut info) = elem.info() else { return 1 };
    println!(
        "{name} = {}{}",
        value_text(&elem, &mut info),
        range_text(&elem, &mut info)
    );
    0
}

/// An enum item name (case-insensitive) or numeric index, resolved to its index.
fn enum_index(elem: &HElem, info: &mut Info, s: &str) -> Option<u32> {
    let items = info.items();
    if let Ok(n) = s.parse::<u32>() {
        if n < items {
            return Some(n);
        }
    }
    (0..items).find(|&i| {
        elem.item_name(info, i)
            .is_some_and(|name| name.eq_ignore_ascii_case(s))
    })
}

fn parse_bool(s: &str) -> Option<bool> {
    match s.to_ascii_lowercase().as_str() {
        "on" | "1" | "true" | "yes" => Some(true),
        "off" | "0" | "false" | "no" => Some(false),
        _ => None,
    }
}

/// Parse an integer the way strtol(s, NULL, 0) does, 0 on garbage.
fn parse_long(s: &str) -> i64 {
    let (neg, t) = match s.trim().strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s.trim().trim_start_matches('+')),
    };
    let v = if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        i64::from_str_radix(h, 16).unwrap_or(0)
    } else if t.len() > 1 && t.starts_with('0') {
        i64::from_str_radix(&t[1..], 8).unwrap_or(0)
    } else {
        t.parse().unwrap_or(0)
    };
    if neg {
        -v
    } else {
        v
    }
}

fn cmd_set(hctl: &HCtl, name: &str, args: &[String]) -> i32 {
    let Some(elem) = hctl.find(name) else {
        eprintln!("no such control: {name}");
        return 1;
    };
    let Ok(mut info) = elem.info() else { return 1 };
    let ty = info.elem_type();
    let Ok(mut val) = elem.read() else { return 1 };

    // one value applies to all members; N values set them individually
    for i in 0..info.count() {
        let s = if args.len() == 1 {
            Some(&args[0])
        } else {
            args.get(i as usize)
        };
        let Some(s) = s else { break };
        match ty {
            ElemType::Boolean => match parse_bool(s) {
                Some(b) => val.set_boolean(i, b),
                None => {
                    eprintln!("expected on/off: {s}");
                    return 1;
                }
            },
            ElemType::Integer => val.set_integer(i, parse_long(s)),
            ElemType::Enumerated => match enum_index(&elem, &mut info, s) {
                Some(idx) => val.set_enumerated(i, idx),
                None => {
                    eprintln!(
                        "invalid value '{s}' for {name}{}",
                        range_text(&elem, &mut info)
                    );
                    return 1;
                }
            },
            other => {
                eprintln!("cannot set control of type {}", type_name(other));
                return 1;
            }
        }
    }
    if elem.write(&val).is_err() {
        eprintln!("write failed for {name}");
        return 1;
    }
    cmd_get(hctl, name)
}

/// CueMix-style overview: clock/format up top, then a count of the matrix.
fn cmd_status(hctl: &HCtl, ctlname: &str) -> i32 {
    println!("MOTU card on {ctlname}");
    let show = |name: &str| {
        hctl.find(name)
            .and_then(|e| e.info().ok().map(|mut i| value_text(&e, &mut i)))
    };
    if let Some(v) = show("Clock Source") {
        println!("  Clock Source : {v}");
    }
    if let Some(v) = show("Clock Rate") {
        println!("  Clock Rate   : {v} Hz");
    }

    let (mut inputs, mut mixes, mut sends, mut outputs, mut total) = (0, 0, 0, 0, 0);
    for elem in hctl.elems() {
        let nm = elem.name();
        total += 1;
        if nm.starts_with("Input ") && nm.contains("Trim") {
            inputs += 1;
        } else if nm.starts_with("Mix ") && nm.contains("Master") {
            mixes += 1;
        } else if nm.starts_with("Mix ") && nm.contains("Volume") {
            sends += 1;
        } else if nm.starts_with("Output ") && nm.contains("Volume") {
            outputs += 1;
        }
    }
    // attached AudioWire interfaces, one RO enum per populated slot
    let slots: Vec<String> = ('A'..='D')
        .filter_map(|s| show(&format!("Slot {s} Interface")).map(|v| format!("{s}={v}")))
        .collect();
    if !slots.is_empty() {
        println!("  Interfaces   : {}", slots.join(", "));
    }
    println!("  Mixer        : {inputs} input(s), {mixes} mix bus(es), {sends} matrix send(s)");
    if outputs > 0 {
        match show("Patchbay Switch") {
            Some(v) => println!("  Outputs      : {outputs} output(s), patchbay {v}"),
            None => println!("  Outputs      : {outputs} output(s)"),
        }
    }
    println!("  Controls     : {total} total   (use 'list' to see them all)");
    if total == 0 {
        println!("  note: driver exposes no mixer controls yet (Phase 5, needs a card).");
    }
    0
}

fn usage() {
    eprintln!(
        "usage: motu424-ctl [-D <ctldev>] [command]\n  \
         (no command)            CueMix-style status overview\n  \
         list                    list every kcontrol\n  \
         get  <name>             read one control\n  \
         set  <name> <val...>    write one control\n  \
         -D hw:N                 target a specific control device"
    );
}

fn main() -> ExitCode {
    motu424_card::restore_sigpipe();
    let args: Vec<String> = std::env::args().collect();
    let (ctlname, argi) = if args.len() > 2 && args[1] == "-D" {
        (args[2].clone(), 3)
    } else {
        match find_motu_card(DRIVER_MATCH, "MOTU") {
            Some(c) => (c, 1),
            None => {
                eprintln!(
                    "no MOTU card found (driver '{DRIVER_MATCH}'). Load the module, or pass -D hw:N."
                );
                return ExitCode::from(1);
            }
        }
    };

    let hctl = match HCtl::open(&ctlname) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("open {ctlname}: {e}");
            return ExitCode::from(1);
        }
    };
    if let Err(e) = hctl.load() {
        eprintln!("load {ctlname}: {e}");
        return ExitCode::from(1);
    }

    let cmd = args.get(argi).map_or("status", String::as_str);
    let rc = match cmd {
        "status" => cmd_status(&hctl, &ctlname),
        "list" => cmd_list(&hctl),
        "get" if argi + 1 < args.len() => cmd_get(&hctl, &args[argi + 1]),
        "set" if argi + 2 < args.len() => cmd_set(&hctl, &args[argi + 1], &args[argi + 2..]),
        _ => {
            usage();
            2
        }
    };
    ExitCode::from(rc as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integers_parse_like_strtol() {
        assert_eq!(parse_long("92"), 92);
        assert_eq!(parse_long("0x10"), 16);
        assert_eq!(parse_long("-3"), -3);
        assert_eq!(parse_long("010"), 8);
        assert_eq!(parse_long("junk"), 0);
    }

    #[test]
    fn booleans_accept_the_usual_words() {
        assert_eq!(parse_bool("ON"), Some(true));
        assert_eq!(parse_bool("no"), Some(false));
        assert_eq!(parse_bool("maybe"), None);
    }
}
