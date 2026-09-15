#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-or-later
"""
motu424-gui - a CueMix FX-style mixing console for the MOTU PCI-324/424.

A GTK4 front-end over `motu424-ctl` (the tested CLI is the single source of truth
for talking to the driver's ALSA kcontrols). It reads `motu424-ctl list`, rebuilds
the CueMix model from the kcontrol names (see docs/cuemix-control-map.md), and
renders it like the real console:

  * one tab per mix bus - a scrollable row of channel strips (send fader +
    drawn peak-hold meter + rotary pan pot + mute/solo) with the bus MASTER
    strip pinned on the right, outside the scroll area;
  * an "Inputs" tab - per-input analog conditioning (trim fader, pad, phase,
    stereo pair, mute);
  * an "Outputs" tab - one mono monitor strip per output (volume fader, meter,
    mute, ST stereo link) with the routed source shown under the strip;
    ST-linked pairs move their volume faders together;
  * channel counts and layout adapt to the attached AudioWire interfaces: the
    driver names channels per slot and bank ("Input B AES 01 ...", see
    docs/cuemix-control-map.md) and the strips regroup under "slot - model -
    bank" headers (analog / ADAT / AES-EBU / main out / phones);
  * a "Patchbay" tab - drawn like a real bay: source jacks (PCM feeds, mix
    bus L/R) wired to output jacks by virtual cables you drag between them.
    It is optional and ships with no cords - every output sits at its
    NORMAL: its own direct PCM feed, except Main outs which are normalled to
    the system stereo program (PCM 1/2, drawn as a dashed line) so desktop
    sound reaches the monitors unconfigured. Double-click an output jack to
    unpatch it (back to its normal), "Unpatch all" pulls every cord in one
    undoable step, hovering a jack prelights it and tells you what it
    carries; ST-linked pairs patch together as L/R. One switch disables the
    whole patchbay: off = fall back to the normals, and the bay dims (the
    demo ships with it off);
  * a "Clock & format" tab - clock source, a sample-rate selector grouped by
    1x/2x/4x clock family, locked rate, metering;
  * a "Diagnostics" tab - PCI / kernel-module / ALSA / kernel-log health
    checks that work even with no card and no driver loaded.

Console behaviours beyond the basics:

  * meters are custom-drawn LED ladders with peak-hold and a latching clip
    lamp (click a meter to clear its clip);
  * control writes are coalesced and flushed from a worker thread, so fader
    drags never block on `motu424-ctl` round-trips;
  * the hardware is re-polled in the background: value changes made elsewhere
    (alsamixer, a second instance) flow in without fighting the control you
    are currently touching, and if the control set itself changes - module
    loaded or unloaded, converters hot-plugged, channel counts shrinking in
    the 2x/4x rate families - the console rebuilds itself around the new set,
    keeping the tab you were on;
  * inputs flagged as a stereo pair get their send faders linked (moving one
    moves its partner);
  * a global SOLO indicator lights whenever any solo is engaged - click it to
    clear every solo at once;
  * pan is a real rotary pot: drag it vertically (hold Ctrl for fine moves),
    scroll over it, double-click to recentre;
  * A/B scenes in the header (arm SET, then A or B stores; A/B alone recalls),
    persisted across sessions;
  * TALK / LISTEN talkback buttons in the header (shown when the card exposes
    them; the atten level lives in Clock & format) - push-to-talk like the
    console button they mimic: hold to talk momentarily, a quick click
    latches;
  * per-bus COPY (push this mix's sends to another mix) and RST (sends back to
    unity, pans centred, mutes/solos off) on the master strip;
  * gang groups: send strips with G engaged move together (keeping their
    relative offsets), on top of stereo-pair linking;
  * click any value readout to type an exact number (pan accepts L20 / C / R35);
  * mix snapshots: save/recall the whole console state as JSON from the menu
    (Ctrl+S / Ctrl+O);
  * Ctrl+Z undoes mix-wide operations (copy, reset, scene recall, snapshot
    load), walking back through the last twenty;
  * channel names are editable in place, persist across sessions and stay in
    sync between the mix tabs and the Inputs tab;
  * window size and the active tab are remembered across sessions;
    Alt+1..9 switches tabs, Ctrl+R reloads;
  * double-click a fader to reset it (sends/master to unity, pan to centre,
    trim to 0); "Shortcuts & tips" in the menu (or F1) lists every gesture.

The mixer is card-gated (driver Phase 5), so with no MOTU card the kcontrols do
not exist yet and the console has nothing to populate. Run with `--demo` to see
the full console against a synthetic PCI-424 rig - a 24I/O in slot A plus a
1224 in slot B (8 analog I/O, an AES/EBU pair, main outs), 3 mix buses; the
demo card keeps state, so snapshots and linking can be exercised, and switching
the sample rate re-enumerates it live (the 2x/4x analog/ADAT shrink). On real
hardware it fills in automatically once the driver registers its CueMix
kcontrols.

Run:  motu424-gui            (needs python-gobject + gtk4; deps: ./install.sh --gui)
      motu424-gui --demo     (synthetic card, to preview the console)
      motu424-gui --rig 24io,2408   (pick the simulated converters per slot:
                                     24io, 1224, 2408, hd192 — implies --demo)
"""
import json
import math
import os
import re
import shutil
import subprocess
import sys
import threading
import time

if any(a in ("-h", "--help") for a in sys.argv[1:]):
    sys.stdout.write(__doc__)
    sys.exit(0)
_skip = False
for _a in sys.argv[1:]:
    if _skip:
        _skip = False               # value consumed by --rig
    elif _a == "--rig":
        _skip = True
    elif _a != "--demo" and not _a.startswith("--rig="):
        sys.exit(f"motu424-gui: unknown option '{_a}' (see --help)")

try:
    import gi
    gi.require_version("Gtk", "4.0")
    from gi.repository import Gtk, Gdk, GLib, Gio
except Exception as e:  # noqa: BLE001
    sys.stderr.write(
        "motu424-gui needs PyGObject + GTK4.\n"
        "  Arch:   sudo pacman -S python-gobject gtk4\n"
        "  Debian: sudo apt install python3-gi gir1.2-gtk-4.0\n"
        "  Fedora: sudo dnf install python3-gobject gtk4\n"
        f"(import error: {e})\n")
    sys.exit(1)

APP_ID = "org.motu.motu424ctl"
DEMO = ("--demo" in sys.argv[1:]
        or any(a == "--rig" or a.startswith("--rig=")
               for a in sys.argv[1:]))    # --rig implies the synthetic card

CONF_DIR = os.path.join(GLib.get_user_config_dir(), "motu424")
NAMES_FILE = os.path.join(CONF_DIR, "channel-names.json")
SCENES_FILE = os.path.join(CONF_DIR, "scenes.json")
UI_FILE = os.path.join(CONF_DIR, "ui-state.json")
SNAP_DIR = os.path.join(CONF_DIR, "snapshots")

POLL_SECONDS = 2          # background hardware re-poll
TOUCH_GRACE = 3.0         # ignore polled values for controls touched this recently
METER_TICK_MS = 50
PEAK_DECAY = 0.35         # peak-hold fall, fraction of full scale per second
CLIP_AT = 0.98

# CueMix-ish dark console theme, scoped to our widgets.
CSS = b"""
.console { background:#15181c; }
.strip { background:#1b1f24; border:1px solid #262c33; border-radius:7px;
         padding:8px 7px; margin:4px; }
.strip.master { background:#20262e; border-color:#3a4a3f; }
.strip .cap { font-family:monospace; font-size:10px; letter-spacing:.06em;
              color:#8b97a1; }
.strip.master .cap { color:#79c98a; }
.strip .db { font-family:monospace; font-size:11px; color:#d7dee4; }
.strip .sub { font-family:monospace; font-size:9px; color:#6f7b85;
              letter-spacing:.08em; }
.strip editablelabel, .strip editablelabel text {
  font-family:monospace; font-size:10px; color:#c3ccd4; }
scale.fader trough { min-width:7px; background:#0d1013; border-radius:4px; }
scale.fader highlight { background:#3a4653; border-radius:4px; }
scale.fader slider { min-width:22px; min-height:20px; margin:0; background:#c9d2da;
                     border:1px solid #7c8791; border-radius:3px; }
scale.fader mark indicator { background:#4a5560; min-height:1px; min-width:6px; }
.strip.master scale.fader slider { background:#57c96a; border-color:#3a8f49; }
.tbtn { font-family:monospace; font-size:10px; padding:2px 0; min-width:22px; }
.tbtn.m:checked { background:#e5534b; color:#0d1013; }
.tbtn.s:checked { background:#f0a63a; color:#0d1013; }
.tbtn.p:checked { background:#5a8bd6; color:#0d1013; }
.tbtn.link:checked { background:#5a8bd6; color:#0d1013; }
.tbtn.g:checked { background:#8a6fd1; color:#0d1013; }
.scene { font-family:monospace; font-size:10px; padding:2px 7px; }
.scene.set:checked { background:#f0a63a; color:#0d1013; }
.scene.talk:checked { background:#e5534b; color:#0d1013; }
.scene.listen:checked { background:#5a8bd6; color:#0d1013; }
.strip editablelabel.db, .strip editablelabel.db text {
  font-size:11px; color:#d7dee4; }
.diagview, .diagview text { background:#11151a; color:#c3ccd4; }
.soloind { font-family:monospace; font-size:10px; font-weight:bold;
           padding:2px 8px; color:#57606a; background:#171b20;
           border:1px solid #262c33; border-radius:4px; }
.soloind.on { background:#f0a63a; color:#0d1013; border-color:#f0a63a; }
.clock { font-family:monospace; font-size:10px; color:#8b97a1; }
.statusbar { font-family:monospace; font-size:10px; color:#8b97a1;
             padding:3px 10px; background:#11151a; border-top:1px solid #262c33; }
.statusbar.err { color:#e5534b; }
.help { background:#15181c; }
.help .db { font-family:monospace; font-size:11px; color:#d7dee4; }
.help .dim-label { color:#8b97a1; }
.patch .cap { font-family:monospace; font-size:10px; letter-spacing:.06em;
              color:#8b97a1; }
.patch .sub { font-family:monospace; font-size:9px; color:#6f7b85;
              letter-spacing:.08em; }
.bankhdr { font-family:monospace; font-size:9px; letter-spacing:.1em;
           color:#6f7b85; }
"""


# --------------------------------------------------------------------- backend

def find_ctl():
    here = os.path.dirname(os.path.abspath(__file__))
    return (shutil.which("motu424-ctl")
            or (os.path.join(here, "motu424-ctl")
                if os.path.exists(os.path.join(here, "motu424-ctl")) else None))


CTL = find_ctl()


def run_ctl(args, dev=None):
    if not CTL:
        return 127, "", "motu424-ctl not found in PATH"
    cmd = [CTL] + (["-D", dev] if dev else []) + args
    p = subprocess.run(cmd, capture_output=True, text=True)
    return p.returncode, p.stdout, p.stderr


def list_cards():
    cards = [(None, "Auto (find MOTU)")]
    try:
        with open("/proc/asound/cards") as f:
            for line in f:
                m = re.match(r"\s*(\d+)\s+\[.*?\]:\s*(.*)", line)
                if m:
                    cards.append((f"hw:{m.group(1)}",
                                  f"hw:{m.group(1)}  {m.group(2).strip()}"))
    except OSError:
        pass
    return cards


def parse_list(text):
    """Parse `motu424-ctl list` into control dicts (name/type/value/min/max/items)."""
    out = []
    for line in text.splitlines():
        if " = " not in line:
            continue
        left, right = line.split(" = ", 1)
        parts = left.rsplit(None, 1)
        if len(parts) != 2:
            continue
        name, ctype = parts[0].strip(), parts[1].strip()
        ctrl = {"name": name, "type": ctype, "value": right.strip(),
                "min": None, "max": None, "items": None}
        if ctype == "int" and "[" in right:
            val, rng = right.rsplit("[", 1)
            ctrl["value"] = val.strip()
            m = re.match(r"\s*(-?\d+)\.\.(-?\d+)", rng)
            if m:
                ctrl["min"], ctrl["max"] = int(m.group(1)), int(m.group(2))
        elif ctype == "enum" and "{" in right:
            val, items = right.rsplit("{", 1)
            ctrl["value"] = val.strip()
            ctrl["items"] = items.rstrip("}").split("|")
        if ctype not in ("int", "enum", "bool"):
            continue
        out.append(ctrl)
    return out


# CueMix kcontrol-name grammar (docs/cuemix-control-map.md).
# A channel is addressed by a TOKEN: either a bare index ("03", the legacy
# single-bank form) or "<slot> <bank> <index>" ("B Main 00") once the driver
# has enumerated the AudioWire interfaces attached to each slot (Phase 3.5).
# Tokens are opaque dict keys everywhere in this file; only the helpers below
# look inside them.
_BANKS = ("Analog", "ADAT", "TDIF", "AES", "SPDIF", "Main", "Phones")
_CHAN = r"((?:[A-D] )?(?:(?:%s) )?\d+)" % "|".join(_BANKS)
_IN = re.compile(r"^Input %s (Trim Volume|Pad Switch|Phase Switch|"
                 r"Stereo Switch|Mute Switch)$" % _CHAN)
_MAS = re.compile(r"^Mix (\d+) Master Volume$")
_MMU = re.compile(r"^Mix (\d+) Mute Switch$")
_SND = re.compile(r"^Mix (\d+) Input %s (Volume|Pan|Mute Switch|Solo Switch)$"
                  % _CHAN)
_OUT = re.compile(r"^Output %s (Volume|Mute Switch|Stereo Switch|Source)$"
                  % _CHAN)
_SLOT = re.compile(r"^Slot ([A-D]) Interface$")
_INKEY = {"Trim Volume": "trim", "Pad Switch": "pad", "Phase Switch": "phase",
          "Stereo Switch": "stereo", "Mute Switch": "mute"}
_SNDKEY = {"Volume": "vol", "Pan": "pan", "Mute Switch": "mute",
           "Solo Switch": "solo"}
_OUTKEY = {"Volume": "vol", "Mute Switch": "mute", "Stereo Switch": "stereo",
           "Source": "src"}

_TOKP = re.compile(r"^(?:([A-D]) )?(?:(%s) )?(\d+)$" % "|".join(_BANKS))
_BANK_RANK = {b: i for i, b in enumerate(("",) + _BANKS)}
_BANK_ABBR = {"Analog": "AN", "ADAT": "AD", "TDIF": "TD", "AES": "AES",
              "SPDIF": "SP", "Main": "MAIN", "Phones": "PH"}
_BANK_DISP = {"Analog": "ANALOG", "ADAT": "ADAT", "TDIF": "TDIF",
              "AES": "AES/EBU", "SPDIF": "S/PDIF", "Main": "MAIN OUT",
              "Phones": "PHONES"}


def tok_parts(tok):
    """Channel token -> (slot, bank, index); slot/bank are "" on legacy
    bare-index tokens."""
    m = _TOKP.match(tok)
    if not m:
        return ("", "", 0)
    return (m.group(1) or "", m.group(2) or "", int(m.group(3)))


def tok_sort(tok):
    slot, bank, idx = tok_parts(tok)
    return (slot, _BANK_RANK.get(bank, len(_BANK_RANK)), idx)


def tok_partner(tok):
    """The other half of tok's even/odd stereo pair (same slot/bank/padding)."""
    m = _TOKP.match(tok)
    if not m:
        return None
    idx = int(m.group(3))
    other = idx + 1 if idx % 2 == 0 else idx - 1
    return tok[:m.start(3)] + f"{other:0{len(m.group(3))}d}"


def tok_cap(tok, out=False):
    """Short strip caption — bank-local, the bank header carries the rest."""
    _slot, bank, idx = tok_parts(tok)
    if bank:
        return f"{_BANK_ABBR[bank]} {idx + 1}"
    return f"OUT {idx + 1}" if out else f"IN {idx:02d}"


def tok_label(tok, out=False):
    """Longer label (patchbay jacks, default names) — carries the slot too,
    since two interfaces can both have an "AN 1"."""
    slot, _bank, _idx = tok_parts(tok)
    cap = tok_cap(tok, out)
    return f"{slot}·{cap}" if slot else cap


def tok_normal_val(tok):
    """An output's factory NORMAL — what it carries with nothing patched.
    Main outs are normalled to the system stereo program (PCM 1/2) so desktop
    sound reaches the monitors unconfigured; everything else to its own
    direct feed. The patchbay bypass falls back to these normals."""
    _slot, bank, idx = tok_parts(tok)
    return f"PCM {idx % 2:02d}" if bank == "Main" else "Direct"


def build_model(controls):
    """Group flat controls into the CueMix matrix model."""
    inputs, buses, outputs, talkback, slots = {}, {}, {}, {}, {}
    glob, other = [], []
    patchbay = None
    for c in controls:
        n = c["name"]
        m = _IN.match(n)
        if m:
            inputs.setdefault(m.group(1), {})[_INKEY[m.group(2)]] = c
            continue
        m = _OUT.match(n)
        if m:
            outputs.setdefault(m.group(1), {})[_OUTKEY[m.group(2)]] = c
            continue
        if n == "Patchbay Switch":
            patchbay = c
            continue
        if n in ("Talkback Switch", "Listenback Switch"):
            talkback[n] = c
            continue
        m = _SLOT.match(n)
        if m:
            slots[m.group(1)] = c["value"].split(",")[0].strip()
            continue
        m = _MAS.match(n)
        if m:
            buses.setdefault(int(m.group(1)), {}).setdefault("sends", {})
            buses[int(m.group(1))]["master"] = c
            continue
        m = _MMU.match(n)
        if m:
            buses.setdefault(int(m.group(1)), {}).setdefault("sends", {})
            buses[int(m.group(1))]["mute"] = c
            continue
        m = _SND.match(n)
        if m:
            k, tok, field = int(m.group(1)), m.group(2), _SNDKEY[m.group(3)]
            b = buses.setdefault(k, {})
            b.setdefault("sends", {}).setdefault(tok, {})[field] = c
            continue
        if n.startswith(("Clock", "Sample", "Meters")):
            glob.append(c)
        else:
            other.append(c)
    return {"inputs": inputs, "buses": buses, "outputs": outputs,
            "patchbay": patchbay, "talkback": talkback, "slots": slots,
            "globals": glob, "other": other}


# slot -> interface model -> banks (bank, inputs, outputs). The default demo
# rig is a 24I/O in slot A plus a 1224 in slot B (8 analog I/O, an AES/EBU
# pair, main outs) — everything the console shows flows from this table.
#
# `--rig 24io,2408` picks other converters for the PCI-424's four AudioWire
# slots (A-D, in order; demo approximations of the vendor lineup).
RIG_IFACES = {
    "24io": ("24I/O", (("Analog", 24, 24),)),
    "1224": ("1224", (("Analog", 8, 8), ("AES", 2, 2), ("Main", 0, 2))),
    "2408": ("2408mk3", (("Analog", 8, 8), ("ADAT", 24, 24),
                         ("SPDIF", 2, 2), ("Main", 0, 2))),
    "hd192": ("HD192", (("Analog", 12, 12), ("AES", 2, 2))),
}


def _rig_from_argv(args):
    spec = None
    for k, a in enumerate(args):
        if a == "--rig" and k + 1 < len(args):
            spec = args[k + 1]
        elif a.startswith("--rig="):
            spec = a.split("=", 1)[1]
    if spec is None:
        return None
    rig = []
    for slot, name in zip("ABCD", spec.replace("+", ",").split(",")):
        name = name.strip().lower()
        if not name or name in ("none", "empty"):
            continue
        if name not in RIG_IFACES:
            sys.exit(f"motu424-gui: unknown --rig interface '{name}' "
                     f"(available: {', '.join(RIG_IFACES)}, none)")
        model, banks = RIG_IFACES[name]
        rig.append((slot, model, banks))
    if not rig:
        sys.exit("motu424-gui: --rig lists no interface "
                 f"(available: {', '.join(RIG_IFACES)})")
    return tuple(rig)


RIG_ARG = _rig_from_argv(sys.argv[1:])
DEMO_RIG = RIG_ARG or (
    ("A", "24I/O", (("Analog", 24, 24),)),
    ("B", "1224", (("Analog", 8, 8), ("AES", 2, 2), ("Main", 0, 2))),
)


def demo_controls(rig=DEMO_RIG, n_bus=3, family=1):
    """Synthetic CueMix kcontrol set so the console can be previewed without a
    card. Channel counts, banks and slots all flow from `rig`; the GUI adapts
    to whatever is listed, exactly as it will to the driver's enumeration.
    `family` (1/2/4) models the AudioWire bandwidth shrink: analog and ADAT/
    TDIF banks carry half the channels at 2x rates, a quarter at 4x."""
    import random

    def bank_count(bank, n):
        if bank in ("Analog", "ADAT", "TDIF") and family > 1:
            return max(2, (n // family) & ~1)      # whole stereo pairs only
        return n
    out = [
        {"name": "Clock Source", "type": "enum", "value": "Internal",
         "items": ["Internal", "Word Clock", "ADAT", "SPDIF", "AES/EBU"],
         "min": None, "max": None},
        {"name": "Sample Rate", "type": "enum", "value": "48000",
         "items": ["44100", "48000", "88200", "96000", "176400", "192000"],
         "min": None, "max": None},
        {"name": "Clock Rate", "type": "int", "value": "48000",
         "min": 44100, "max": 192000},
        {"name": "Meters", "type": "bool", "value": "on",
         "min": None, "max": None, "items": None},
    ]
    def i(name, v, lo, hi):
        return {"name": name, "type": "int", "value": str(v),
                "min": lo, "max": hi, "items": None}
    def b(name, v):
        return {"name": name, "type": "bool", "value": v,
                "min": None, "max": None, "items": None}
    ins, outs_ = [], []
    for slot, iface, banks in rig:
        out.append({"name": f"Slot {slot} Interface", "type": "enum",
                    "value": iface, "items": [iface], "min": None, "max": None})
        for bank, n_in, n_out in banks:
            ins += [f"{slot} {bank} {nn:02d}"
                    for nn in range(bank_count(bank, n_in))]
            outs_ += [f"{slot} {bank} {nn:02d}"
                      for nn in range(bank_count(bank, n_out))]
    for tok in ins:
        _slot, bank, idx = tok_parts(tok)
        p = f"Input {tok} "
        st = bank == "AES" or (tok.startswith("A Analog") and idx < 2)
        out += [i(p + "Trim Volume", random.randint(0, 60), 0, 60),
                b(p + "Pad Switch", "off"), b(p + "Phase Switch", "off"),
                b(p + "Stereo Switch", "on" if st and idx % 2 == 0 else "off"),
                b(p + "Mute Switch", "off")]
    for kk in range(n_bus):
        mp = f"Mix {kk:02d} "
        out += [i(mp + "Master Volume", 100, 0, 127), b(mp + "Mute Switch", "off")]
        for tok in ins:
            sp = f"Mix {kk:02d} Input {tok} "
            out += [i(sp + "Volume", random.randint(40, 110), 0, 127),
                    i(sp + "Pan", random.randint(-60, 60), -100, 100),
                    b(sp + "Mute Switch", "off"), b(sp + "Solo Switch", "off")]
    out += [b("Talkback Switch", "off"), b("Listenback Switch", "off"),
            i("Talkback Atten Volume", 20, 0, 40)]
    out.append(b("Patchbay Switch", "off"))    # optional feature: ships bypassed
    # outputs are mono channels; an even channel can stereo-pair with the next
    # one in its bank. PCM MM = the feed output #MM (enumeration order) carries
    # when Direct.
    sources = (["Direct"] + [f"PCM {mm:02d}" for mm in range(len(outs_))]
               + [f"Mix {kk:02d} {s}" for kk in range(n_bus)
                  for s in ("L", "R")])
    for tok in outs_:
        _slot, bank, idx = tok_parts(tok)
        op = f"Output {tok} "
        st = bank in ("AES", "Main") or (tok.startswith("A Analog") and idx < 2)
        # nothing is patched out of the box; outputs sit at their NORMALS,
        # like a bay with no cords in it: own direct PCM feed everywhere,
        # except Main outs which are normalled to the system stereo program
        # (PCM 1/2) so desktop sound reaches the monitors unconfigured
        value = f"PCM {idx % 2:02d}" if bank == "Main" else "Direct"
        out += [i(op + "Volume", 100, 0, 127), b(op + "Mute Switch", "off"),
                b(op + "Stereo Switch", "on" if st and idx % 2 == 0 else "off"),
                {"name": op + "Source", "type": "enum", "value": value,
                 "items": sources, "min": None, "max": None}]
    return out


class CtlWriter:
    """Coalescing async writer: keeps only the latest value per control and
    flushes them from a worker thread, so a fader drag costs at most one
    in-flight `motu424-ctl set` at a time instead of one per motion event."""

    def __init__(self, on_error):
        self._cond = threading.Condition()
        self._pending = {}                 # name -> (dev, value)
        self._on_error = on_error          # called on the GTK thread
        threading.Thread(target=self._loop, daemon=True).start()

    def push(self, name, value, dev):
        with self._cond:
            self._pending[name] = (dev, value)
            self._cond.notify()

    def pending(self, name):
        with self._cond:
            return name in self._pending

    def _loop(self):
        while True:
            with self._cond:
                while not self._pending:
                    self._cond.wait()
            time.sleep(0.04)               # let a drag coalesce
            with self._cond:
                batch, self._pending = self._pending, {}
            for name, (dev, value) in batch.items():
                rc, _o, err = run_ctl(["set", name, value], dev)
                if rc != 0:
                    msg = err.strip() or f"set '{name}' failed"
                    GLib.idle_add(self._on_error, f"{name}: {msg}")


def _load_json(path, default):
    try:
        with open(path) as f:
            return json.load(f)
    except (OSError, ValueError):
        return default


def _save_json(path, data):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    tmp = path + ".tmp"
    with open(tmp, "w") as f:
        json.dump(data, f, indent=1, sort_keys=True)
    os.replace(tmp, path)


# ------------------------------------------------------------------------- gui

def _install_css():
    prov = Gtk.CssProvider()
    prov.load_from_data(CSS)
    Gtk.StyleContext.add_provider_for_display(
        Gdk.Display.get_default(), prov,
        Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION)


class Meter(Gtk.DrawingArea):
    """LED-ladder level meter with peak-hold and a latching clip lamp."""

    LAMP = 7      # px reserved at the top for the clip lamp
    ZONES = ((0.00, 0.72, (0.34, 0.79, 0.42)),
             (0.72, 0.92, (0.94, 0.65, 0.23)),
             (0.92, 1.00, (0.90, 0.33, 0.29)))

    def __init__(self):
        super().__init__()
        self.level, self.peak, self.clip = 0.0, 0.0, False
        self.set_content_width(11)
        self.set_content_height(168)
        self.set_draw_func(self._draw)
        click = Gtk.GestureClick()
        click.connect("pressed", self._clear_clip)
        self.add_controller(click)
        self.set_tooltip_text("Click to clear clip")

    def set_level(self, v):
        v = max(0.0, min(1.0, v))
        self.level = v
        if v > self.peak:
            self.peak = v
        if v >= CLIP_AT:
            self.clip = True
        self.queue_draw()

    def decay(self, dt):
        if self.peak > 0.0:
            self.peak = max(self.level, self.peak - PEAK_DECAY * dt)
            self.queue_draw()

    def _clear_clip(self, *_a):
        self.clip = False
        self.queue_draw()

    def _draw(self, _area, cr, w, h):
        bar_h = h - self.LAMP - 2
        cr.set_source_rgb(0.05, 0.06, 0.075)
        cr.rectangle(0, 0, w, h)
        cr.fill()
        # clip lamp
        cr.set_source_rgb(*((0.90, 0.33, 0.29) if self.clip else (0.13, 0.09, 0.09)))
        cr.rectangle(0, 0, w, self.LAMP)
        cr.fill()
        # zone fills, clipped by the current level (bottom-up)
        top = self.LAMP + 2
        lvl_y = top + bar_h * (1.0 - self.level)
        for z0, z1, rgb in self.ZONES:
            y1 = top + bar_h * (1.0 - z0)   # zone bottom
            y0 = top + bar_h * (1.0 - z1)   # zone top
            lit_y0 = max(y0, lvl_y)
            if lit_y0 < y1:
                cr.set_source_rgb(*rgb)
                cr.rectangle(0, lit_y0, w, y1 - lit_y0)
                cr.fill()
            dim = tuple(c * 0.16 for c in rgb)
            if y0 < lit_y0:
                cr.set_source_rgb(*dim)
                cr.rectangle(0, y0, w, min(lit_y0, y1) - y0)
                cr.fill()
        # LED segmentation
        cr.set_source_rgb(0.082, 0.094, 0.11)
        y = top
        while y < top + bar_h:
            cr.rectangle(0, y, w, 1)
            y += 4
        cr.fill()
        # peak-hold line
        if self.peak > 0.0:
            cr.set_source_rgb(0.85, 0.88, 0.9)
            cr.rectangle(0, top + bar_h * (1.0 - self.peak) - 1, w, 2)
            cr.fill()


class Knob(Gtk.DrawingArea):
    """Rotary pot (270° sweep): drag vertically to turn (Ctrl = fine), scroll
    over it, double-click to recentre. `on_change` fires on user moves only;
    `set_value_silent` is the polling/snapshot path."""

    SWEEP = math.radians(135)          # each side of 12 o'clock

    def __init__(self, lo, hi, value, on_change):
        super().__init__()
        self.lo, self.hi = lo, hi
        self.value = max(lo, min(hi, value))
        self.on_change = on_change
        self._v0 = self.value
        self.set_content_width(36)
        self.set_content_height(36)
        self.set_draw_func(self._draw)
        drag = Gtk.GestureDrag()
        drag.connect("drag-begin", lambda *_: setattr(self, "_v0", self.value))
        drag.connect("drag-update", self._drag)
        self.add_controller(drag)
        scroll = Gtk.EventControllerScroll.new(
            Gtk.EventControllerScrollFlags.VERTICAL)
        scroll.connect("scroll", self._scroll)
        self.add_controller(scroll)
        dbl = Gtk.GestureClick()
        dbl.connect("pressed",
                    lambda _g, n, _x, _y:
                    self._set((self.lo + self.hi) // 2) if n == 2 else None)
        self.add_controller(dbl)

    def set_value_silent(self, v):
        self.value = max(self.lo, min(self.hi, v))
        self.queue_draw()

    def _set(self, v):
        v = max(self.lo, min(self.hi, int(round(v))))
        if v != self.value:
            self.value = v
            self.queue_draw()
            self.on_change(v)

    def _drag(self, gesture, _dx, dy):
        fine = False
        try:
            fine = bool(gesture.get_current_event_state()
                        & Gdk.ModifierType.CONTROL_MASK)
        except Exception:  # noqa: BLE001
            pass
        span = self.hi - self.lo
        self._set(self._v0 - dy * span / (600.0 if fine else 150.0))

    def _scroll(self, _ctl, _dx, dy):
        step = max(1, (self.hi - self.lo) // 40)
        self._set(self.value - dy * step)
        return True

    def _draw(self, _area, cr, w, h):
        cx, cy, r = w / 2.0, h / 2.0, min(w, h) / 2.0 - 2
        span = (self.hi - self.lo) or 1
        frac = (self.value - self.lo) / span
        up = -math.pi / 2
        a = up + (frac * 2 - 1) * self.SWEEP
        cr.set_line_width(3)
        cr.set_source_rgb(0.05, 0.06, 0.075)          # track ring
        cr.arc(cx, cy, r, up - self.SWEEP, up + self.SWEEP)
        cr.stroke()
        cr.set_source_rgb(0.35, 0.55, 0.84)           # deviation arc from centre
        if a >= up:
            cr.arc(cx, cy, r, up, a)
        else:
            cr.arc(cx, cy, r, a, up)
        cr.stroke()
        cr.set_source_rgb(0.15, 0.18, 0.22)           # body
        cr.arc(cx, cy, r - 4.5, 0, 2 * math.pi)
        cr.fill()
        cr.set_line_width(1)
        cr.set_source_rgb(0.29, 0.34, 0.39)
        cr.arc(cx, cy, r - 4.5, 0, 2 * math.pi)
        cr.stroke()
        cr.set_line_width(2)                          # pointer
        cr.set_source_rgb(0.79, 0.82, 0.86)
        cr.move_to(cx + (r - 10) * math.cos(a), cy + (r - 10) * math.sin(a))
        cr.line_to(cx + (r - 4.5) * math.cos(a), cy + (r - 4.5) * math.sin(a))
        cr.stroke()


class PatchBay(Gtk.DrawingArea):
    """A drawn patchbay: source jacks on the left (PCM feeds + each mix bus
    L/R), output jacks on the right, a virtual cable per *patched* output.
    Like a normalled hardware bay it starts with no cords: an output sitting
    at its NORMAL draws no cable — except Main outs, whose normal is the
    system stereo program (PCM 1/2) and shows as a faint dashed line. Drag a
    cable between a source and an output to patch; double-click an output
    jack to unpatch it (back to its normal). ST-linked pairs (bracketed)
    patch together as L/R."""

    ROW = 30
    PAD_Y = 34
    JACK_R = 6.5
    GUTTER = 130                       # label gutter each side
    HIT = 16                           # jack grab radius
    HUES = ((0.35, 0.55, 0.84), (0.94, 0.65, 0.23), (0.54, 0.44, 0.82),
            (0.34, 0.79, 0.42), (0.90, 0.33, 0.29), (0.36, 0.76, 0.78))
    PCM_RGB = (0.47, 0.52, 0.57)

    def __init__(self, sources, outs, pair_of, get_conn, set_conn, out_label,
                 normal_of):
        # sources: ordered [(sid, label)]; sid = ("pcm", mm) | ("mix", kk, "L"/"R")
        super().__init__()
        self.sources = sources
        self.outs = outs               # ordered output channel tokens
        self.pair_of = pair_of         # token -> ST partner (for the bracket)
        self.get_conn = get_conn       # token -> sid | None
        self.set_conn = set_conn       # (token, sid) — the panel writes controls
        self.out_label = out_label     # token -> persisted display name
        self.normal_of = normal_of     # token -> its factory-normal sid
        self._drag_from = None         # ("src", sid) | ("out", nn)
        self._drag_a = None            # drag start point
        self._drag_xy = None           # current pointer
        self._hover = None             # jack under the pointer
        rows = max(len(sources), len(outs))
        self.set_content_height(2 * self.PAD_Y + rows * self.ROW)
        self.set_content_width(2 * self.GUTTER + 320)
        self.set_hexpand(True)
        self.set_vexpand(True)
        self.set_draw_func(self._draw)
        drag = Gtk.GestureDrag()
        drag.connect("drag-begin", self._begin)
        drag.connect("drag-update", self._update)
        drag.connect("drag-end", self._end)
        self.add_controller(drag)
        dbl = Gtk.GestureClick()
        dbl.connect("pressed", self._dbl)
        self.add_controller(dbl)
        mot = Gtk.EventControllerMotion()
        mot.connect("motion", self._motion)
        mot.connect("leave", self._leave)
        self.add_controller(mot)
        self.set_has_tooltip(True)
        self.connect("query-tooltip", self._tooltip)

    # geometry ---------------------------------------------------------------
    def _src_xy(self, i):
        return (self.GUTTER,
                self.PAD_Y + i * self.ROW + self.ROW / 2)

    def _out_xy(self, j):
        return (self.get_width() - self.GUTTER,
                self.PAD_Y + j * self.ROW + self.ROW / 2)

    def _jack_at(self, x, y):
        for i, (sid, _l) in enumerate(self.sources):
            jx, jy = self._src_xy(i)
            if (x - jx) ** 2 + (y - jy) ** 2 <= self.HIT ** 2:
                return ("src", sid)
        for j, nn in enumerate(self.outs):
            jx, jy = self._out_xy(j)
            if (x - jx) ** 2 + (y - jy) ** 2 <= self.HIT ** 2:
                return ("out", nn)
        return None

    def _rgb(self, sid):
        if sid[0] == "mix":
            return self.HUES[sid[1] % len(self.HUES)]
        return self.PCM_RGB

    # interaction ------------------------------------------------------------
    def _begin(self, _g, x, y):
        if not self.is_sensitive():
            return
        self._drag_from = self._jack_at(x, y)
        self._drag_a = (x, y)
        self._drag_xy = (x, y)
        if self._drag_from:
            self.queue_draw()

    def _update(self, _g, dx, dy):
        if self._drag_from:
            self._drag_xy = (self._drag_a[0] + dx, self._drag_a[1] + dy)
            self.queue_draw()

    def _end(self, _g, dx, dy):
        start, self._drag_from = self._drag_from, None
        if start and self._drag_xy:
            target = self._jack_at(*self._drag_xy)
            if target and target[0] != start[0]:
                out = start[1] if start[0] == "out" else target[1]
                sid = start[1] if start[0] == "src" else target[1]
                self.set_conn(out, sid)
        self._drag_xy = None
        self.queue_draw()

    def _dbl(self, _g, n, x, y):
        if n == 2 and self.is_sensitive():
            hit = self._jack_at(x, y)
            if hit and hit[0] == "out":
                self.set_conn(hit[1], self.normal_of(hit[1]))

    def _motion(self, _c, x, y):
        hover = self._jack_at(x, y)
        if hover != self._hover:
            self._hover = hover
            self.set_cursor_from_name(
                "pointer" if hover and self.is_sensitive() else None)
            self.queue_draw()

    def _leave(self, _c):
        if self._hover:
            self._hover = None
            self.set_cursor_from_name(None)
            self.queue_draw()

    def _src_label(self, sid):
        return next((l for s, l in self.sources if s == sid), "?")

    def _tooltip(self, _w, x, y, _kb, tip):
        """Hovering a jack tells you what it carries / feeds."""
        hit = self._jack_at(x, y)
        if not hit:
            return False
        if hit[0] == "out":
            nn = hit[1]
            sid, normal = self.get_conn(nn), self.normal_of(nn)
            what = self._src_label(sid) if sid else "?"
            txt = f"{self.out_label(nn)}  ←  {what}"
            txt += ("   (its normal — nothing patched)" if sid == normal
                    else "   — double-click to unpatch")
        else:
            sid = hit[1]
            fed = [self.out_label(nn) for nn in self.outs
                   if self.get_conn(nn) == sid]
            txt = self._src_label(sid) + (
                "  →  " + ", ".join(fed) if fed
                else "  —  feeds nothing; drag it to an output jack")
        tip.set_text(txt)
        return True

    # drawing ----------------------------------------------------------------
    def _cable(self, cr, x1, y1, x2, y2, rgb, alpha, width, dashed=False):
        sag = 16 + abs(y2 - y1) * 0.08
        c1 = (x1 + (x2 - x1) * 0.35, y1 + sag)
        c2 = (x2 - (x2 - x1) * 0.35, y2 + sag)
        if dashed:
            cr.set_dash((5, 4))
        cr.set_line_width(width + 2)
        cr.set_source_rgba(0, 0, 0, 0.35 * alpha)     # shadow under the cable
        cr.move_to(x1, y1)
        cr.curve_to(*c1, *c2, x2, y2)
        cr.stroke()
        cr.set_line_width(width)
        cr.set_source_rgba(*rgb, alpha)
        cr.move_to(x1, y1)
        cr.curve_to(*c1, *c2, x2, y2)
        cr.stroke()
        cr.set_dash(())

    def _jack(self, cr, x, y, rgb, dim, lit=False):
        cr.set_source_rgba(*rgb, 0.28 * dim + (0.5 if lit else 0))
        cr.arc(x, y, self.JACK_R + 3.5, 0, 2 * math.pi)
        cr.fill()
        cr.set_line_width(2)
        cr.set_source_rgba(*rgb, dim)
        cr.arc(x, y, self.JACK_R, 0, 2 * math.pi)
        cr.stroke()
        cr.set_source_rgba(0.04, 0.05, 0.06, dim)     # the hole
        cr.arc(x, y, self.JACK_R - 2.5, 0, 2 * math.pi)
        cr.fill()

    def _text(self, cr, x, y, s, dim, align_right=False, bright=False):
        cr.select_font_face("monospace")
        cr.set_font_size(10)
        if align_right:
            x -= cr.text_extents(s).width
        cr.set_source_rgba(*((0.84, 0.87, 0.89) if bright
                             else (0.55, 0.60, 0.65)), dim)
        cr.move_to(x, y + 3.5)
        cr.show_text(s)

    def _draw(self, _area, cr, w, h):
        dim = 1.0 if self.is_sensitive() else 0.35
        cr.set_source_rgba(0.09, 0.106, 0.125, dim)   # bay panel
        cr.rectangle(0, 0, w, h)
        cr.fill()
        self._text(cr, self.GUTTER - 8, 16, "SOURCES", dim, align_right=True)
        self._text(cr, w - self.GUTTER + 8, 16, "OUTPUTS", dim)
        # ST brackets between linked output jacks (partner sits just below)
        for j, nn in enumerate(self.outs):
            if (j + 1 < len(self.outs)
                    and self.pair_of.get(nn) == self.outs[j + 1]):
                x, y1 = self._out_xy(j)
                _x, y2 = self._out_xy(j + 1)
                cr.set_line_width(1.5)
                cr.set_source_rgba(0.55, 0.60, 0.65, 0.7 * dim)
                cr.move_to(x + 12, y1)
                cr.curve_to(x + 20, y1, x + 20, y2, x + 12, y2)
                cr.stroke()
        # cables — an output at its normal draws none (identity) or a faint
        # dashed "normal" line (Main outs <- system program); a real cord
        # only appears once something is actually patched
        for j, nn in enumerate(self.outs):
            sid = self.get_conn(nn)
            if sid is None:
                continue
            normal = self.normal_of(nn)
            if sid == normal and sid == ("pcm", j):
                continue                        # identity normal: empty bay
            try:
                i = next(k for k, (s, _l) in enumerate(self.sources)
                         if s == sid)
            except StopIteration:
                continue
            x1, y1 = self._src_xy(i)
            x2, y2 = self._out_xy(j)
            if sid == normal:
                self._cable(cr, x1, y1, x2, y2, self.PCM_RGB, 0.45 * dim, 2,
                            dashed=True)
            else:
                self._cable(cr, x1, y1, x2, y2, self._rgb(sid), 0.9 * dim, 3)
        # drag preview
        target = None
        if self._drag_from and self._drag_xy:
            hit = self._jack_at(*self._drag_xy)
            if hit and hit[0] != self._drag_from[0]:
                target = hit
            if self._drag_from[0] == "src":
                i = next(k for k, (s, _l) in enumerate(self.sources)
                         if s == self._drag_from[1])
                x1, y1 = self._src_xy(i)
                rgb = self._rgb(self._drag_from[1])
            else:
                j = self.outs.index(self._drag_from[1])
                x1, y1 = self._out_xy(j)
                rgb = (0.84, 0.87, 0.89)
            self._cable(cr, x1, y1, *self._drag_xy, rgb, 0.8, 2.5, dashed=True)
        # jacks + labels (hover prelights a jack when nothing is being dragged)
        hov = self._hover if self.is_sensitive() and not self._drag_from else None
        for i, (sid, label) in enumerate(self.sources):
            x, y = self._src_xy(i)
            self._jack(cr, x, y, self._rgb(sid), dim,
                       lit=target == ("src", sid) or hov == ("src", sid))
            self._text(cr, x - 14, y, label, dim, align_right=True)
        for j, nn in enumerate(self.outs):
            x, y = self._out_xy(j)
            sid = self.get_conn(nn)
            rgb = self._rgb(sid) if sid else self.PCM_RGB
            self._jack(cr, x, y, rgb, dim,
                       lit=target == ("out", nn) or hov == ("out", nn))
            self._text(cr, x + 14, y, self.out_label(nn), dim, bright=True)


class Panel(Gtk.ApplicationWindow):
    def __init__(self, app):
        super().__init__(application=app, title="motu424 — CueMix")
        ui = _load_json(UI_FILE, {})
        self.set_default_size(ui.get("w", 980), ui.get("h", 660))
        if ui.get("max"):
            self.maximize()
        self._restore_page = ui.get("page")
        self.connect("close-request", self._on_close)

        self.meters = []                    # [Meter, phase] for demo animation
        self._sync = {}                     # kcontrol name -> setter(value_str)
        self._obs = {}                      # kcontrol name -> passive callbacks
        self._byname = {}                   # kcontrol name -> control dict
        self._touched = {}                  # name -> monotonic time of last user edit
        self._send_scales = {}              # (bus, input) -> Gtk.Scale
        self._out_scales = {}               # output -> Gtk.Scale
        self._out_pair_of = {}              # output -> stereo partner output
        self._solo_btns = []
        self._pair_of = {}                  # input -> stereo partner input
        self._gang = set()                  # (bus, input) strips ganged together
        self._last_val = {}                 # (bus, input) -> last send value
        self._syncing = False               # widget updates coming from us
        self._fanning = False               # stereo-link / gang fan-out in progress
        self._poll_busy = False
        self._undo_stack = []               # previous values of mix-wide ops
        self._name_labels = {}              # "in:<token>" -> [EditableLabel, ...]
        self._slots = {}                    # AudioWire slot -> interface model
        self._ctl_names = set()             # control set at the last rebuild
        self._demo_fam = 1                  # demo card's current rate family
        self._nb = None
        self._help_win = None
        self.demo = demo_controls() if DEMO else None
        self.names = _load_json(NAMES_FILE, {})
        self.scenes = _load_json(SCENES_FILE, {})
        self.writer = CtlWriter(self._on_write_error)

        header = Gtk.HeaderBar()
        self.set_titlebar(header)

        self.cards = list_cards()
        self.card_dd = Gtk.DropDown.new_from_strings([c[1] for c in self.cards])
        self.card_dd.connect("notify::selected", lambda *_: self.reload())
        header.pack_start(self.card_dd)
        if DEMO:
            cap = "demo" if not RIG_ARG else (
                "demo · " + "+".join(m for _s, m, _b in DEMO_RIG))
            tag = Gtk.Label(label=cap)
            tag.add_css_class("dim-label")
            header.pack_start(tag)

        menu = Gio.Menu()
        menu.append("Undo mix change", "win.undo")
        menu.append("Save mix snapshot…", "win.snap-save")
        menu.append("Load mix snapshot…", "win.snap-load")
        menu.append("Reload controls", "win.refresh")
        menu.append("Shortcuts & tips", "win.help")
        mbtn = Gtk.MenuButton(icon_name="open-menu-symbolic", menu_model=menu)
        header.pack_end(mbtn)
        for name, cb in (("undo", self._on_undo),
                         ("snap-save", self._snap_save),
                         ("snap-load", self._snap_load),
                         ("refresh", lambda *_: self.reload()),
                         ("help", self._show_help)):
            act = Gio.SimpleAction.new(name, None)
            act.connect("activate", cb)
            self.add_action(act)
        for i in range(9):
            act = Gio.SimpleAction.new(f"tab{i}", None)
            act.connect("activate", self._on_tab, i)
            self.add_action(act)

        self.solo_ind = Gtk.Button(label="SOLO")
        self.solo_ind.add_css_class("soloind")
        self.solo_ind.set_tooltip_text("Lit while any solo is engaged — click to clear all solos")
        self.solo_ind.connect("clicked", self._clear_solos)
        header.pack_end(self.solo_ind)

        self.clock_lbl = Gtk.Label(label="")
        self.clock_lbl.add_css_class("clock")
        header.pack_end(self.clock_lbl)

        sc_box = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=2)
        self.scene_set = Gtk.ToggleButton(label="SET")
        self.scene_set.add_css_class("scene")
        self.scene_set.add_css_class("set")
        self.scene_set.set_tooltip_text("Arm, then press A or B to store the current mix")
        sc_box.append(self.scene_set)
        for slot in ("A", "B"):
            b = Gtk.Button(label=slot)
            b.add_css_class("scene")
            b.set_tooltip_text(f"Recall scene {slot} (SET then {slot} stores it)")
            b.connect("clicked", self._on_scene, slot)
            sc_box.append(b)
        header.pack_end(sc_box)

        # talkback / listenback — shown only when the card exposes them
        self.tb_box = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=2)
        self.tb_btns = {}
        for cap, cls, key, tip in (
                ("TALK", "talk", "Talkback Switch",
                 "Talkback — route the talkback input into the mixes; "
                 "hold to talk momentarily, a quick click latches "
                 "(atten level lives in Clock & format)"),
                ("LISTEN", "listen", "Listenback Switch",
                 "Listenback — route the listenback input into the mixes; "
                 "hold for momentary, a quick click latches")):
            b = Gtk.ToggleButton(label=cap)
            b.add_css_class("scene")
            b.add_css_class(cls)
            b.set_tooltip_text(tip)
            b.connect("toggled", self._on_toggle, key)
            # push-to-talk, like the console button it mimics: engage on
            # *press*; released after a beat = momentary (drops back off),
            # a quick click latches instead
            hold = Gtk.GestureClick()
            hold.set_propagation_phase(Gtk.PropagationPhase.CAPTURE)
            hold.connect("pressed", self._tb_press, b)
            hold.connect("released", self._tb_release, b)
            b.add_controller(hold)
            self.tb_btns[key] = b
            self.tb_box.append(b)
        self.tb_box.set_visible(False)
        header.pack_end(self.tb_box)

        outer = Gtk.Box(orientation=Gtk.Orientation.VERTICAL)
        outer.add_css_class("console")
        self.body = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, vexpand=True)
        self.status = Gtk.Label(label="", xalign=0)
        self.status.add_css_class("statusbar")
        self._status_seq = 0
        outer.append(self.body)
        outer.append(self.status)
        self.set_child(outer)

        self.reload()
        GLib.timeout_add(METER_TICK_MS, self._tick_meters)
        GLib.timeout_add_seconds(POLL_SECONDS, self._poll)

    # -- data ---------------------------------------------------------------
    def current_dev(self):
        return self.cards[self.card_dd.get_selected()][0]

    def reload(self):
        cur_tab = None                          # keep the active tab across rebuilds
        if self._nb is not None and self._nb.get_current_page() >= 0:
            cur_tab = self._nb.get_tab_label_text(
                self._nb.get_nth_page(self._nb.get_current_page()))
        self.meters = []
        self._sync = {}
        self._obs = {}
        self._byname = {}
        self._send_scales = {}
        self._out_scales = {}
        self._out_pair_of = {}
        self._solo_btns = []
        self._pair_of = {}
        self._gang = set()
        self._last_val = {}
        self._name_labels = {}
        self._nb = None
        child = self.body.get_first_child()
        while child:
            self.body.remove(child)
            child = self.body.get_first_child()

        err_page = None
        if DEMO:
            controls = self.demo
        else:
            rc, out, err = run_ctl(["list"], self.current_dev())
            if rc != 0:
                controls = []
                err_page = self._notice(
                    err.strip() or "No MOTU card found.",
                    "Pick a card above, load the module, or run with --demo.")
            else:
                controls = parse_list(out)

        self._byname = {c["name"]: c for c in controls}
        model = build_model(controls)
        self._slots = model["slots"]
        for key, btn in self.tb_btns.items():
            c = self._byname.get(key)
            if not c:
                continue
            self._syncing = True
            try:
                btn.set_active(c["value"].split(",")[0].strip() == "on")
            finally:
                self._syncing = False
            self._register(key, lambda v, b=btn:
                           b.set_active(v.split(",")[0].strip() == "on"))
        self.tb_box.set_visible(any(k in self._byname for k in self.tb_btns))
        self._update_clock_label()

        nb = Gtk.Notebook(scrollable=True)
        nb.set_vexpand(True)
        if err_page is not None:
            nb.append_page(err_page, Gtk.Label(label="Console"))
        elif (not model["buses"] and not model["inputs"]
              and not model["outputs"] and not model["other"]):
            nb.append_page(self._notice(
                "This card exposes no adjustable controls yet.",
                "The CueMix mixer is driver Phase 5 (needs kcontrols on real "
                "hardware). Try  motu424-gui --demo  to preview the console."),
                Gtk.Label(label="Console"))
        elif not model["buses"] and not model["inputs"] and not model["outputs"]:
            # generic ALSA card: fall back to a plain control list
            nb.append_page(self._generic(model["other"]),
                           Gtk.Label(label="Controls"))
        else:
            # stereo pairs: an even channel's Stereo Switch pairs it with the
            # next one in its bank (inputs and outputs alike)
            for d, pairs in ((model["inputs"], self._pair_of),
                             (model["outputs"], self._out_pair_of)):
                for tok, chan in d.items():
                    st = chan.get("stereo")
                    if not (st and st["value"].split(",")[0].strip() == "on"):
                        continue
                    p = tok_partner(tok)
                    if tok_parts(tok)[2] % 2 == 0 and p in d:
                        pairs[tok] = p
                        pairs[p] = tok
            for kk in sorted(model["buses"]):
                nb.append_page(self._bus_page(kk, model),
                               Gtk.Label(label=f"Mix {kk:02d}"))
            if model["inputs"]:
                nb.append_page(self._inputs_page(model["inputs"]),
                               Gtk.Label(label="Inputs"))
            if model["outputs"]:
                nb.append_page(self._outputs_page(model["outputs"]),
                               Gtk.Label(label="Outputs"))
            if model["patchbay"] or any(
                    "src" in o for o in model["outputs"].values()):
                nb.append_page(self._patchbay_page(model),
                               Gtk.Label(label="Patchbay"))
            if model["globals"] or model["other"] or model["slots"]:
                nb.append_page(self._clock_page(model["globals"] + model["other"],
                                                model["slots"]),
                               Gtk.Label(label="Clock & format"))
        nb.append_page(self._diag_page(), Gtk.Label(label="Diagnostics"))
        self.body.append(nb)
        self._nb = nb
        if self._restore_page is not None:
            if 0 <= self._restore_page < nb.get_n_pages():
                nb.set_current_page(self._restore_page)
            self._restore_page = None
        elif cur_tab is not None:
            for i in range(nb.get_n_pages()):
                if nb.get_tab_label_text(nb.get_nth_page(i)) == cur_tab:
                    nb.set_current_page(i)
                    break
        self._ctl_names = set(self._byname)
        self._update_solo_ind()

    def _update_clock_label(self):
        src = self._byname.get("Clock Source")
        rate = self._byname.get("Clock Rate")
        bits = []
        if src:
            bits.append(src["value"].strip().upper())
        if rate:
            try:
                hz = int(rate["value"].split(",")[0])
                fam = "1x" if hz <= 48000 else ("2x" if hz <= 96000 else "4x")
                bits.append(f"{hz} Hz · {fam}")
            except ValueError:
                pass
        self.clock_lbl.set_text(" · ".join(bits))

    # -- pages --------------------------------------------------------------
    def _grouped_row(self, chans, strip_fn):
        """A horizontal run of channel strips, split per (slot, bank) with a
        header naming the attached interface when the card exposes banks."""
        row = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=2,
                      margin_top=8, margin_bottom=8, margin_start=8,
                      margin_end=8)
        cur, grp_row = None, None
        for tok in sorted(chans, key=tok_sort):
            g = tok_parts(tok)[:2]
            if grp_row is None or g != cur:
                if grp_row is not None:
                    row.append(Gtk.Separator(
                        orientation=Gtk.Orientation.VERTICAL))
                cur = g
                grp = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=2)
                slot, bank = g
                if slot or bank:
                    bits = [x for x in (slot, self._slots.get(slot),
                                        _BANK_DISP.get(bank, bank)) if x]
                    hdr = Gtk.Label(label=" · ".join(bits), xalign=0,
                                    margin_start=6)
                    hdr.add_css_class("bankhdr")
                    grp.append(hdr)
                grp_row = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL,
                                  spacing=2)
                grp.append(grp_row)
                row.append(grp)
            grp_row.append(strip_fn(tok))
        return row

    def _bus_page(self, kk, model):
        page = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL)
        sc = Gtk.ScrolledWindow(hexpand=True, vexpand=True)
        sc.set_policy(Gtk.PolicyType.AUTOMATIC, Gtk.PolicyType.NEVER)
        sends = model["buses"][kk].get("sends", {})
        sc.set_child(self._grouped_row(
            sends, lambda tok: self._send_strip(kk, tok, sends[tok])))
        page.append(sc)
        page.append(Gtk.Separator(orientation=Gtk.Orientation.VERTICAL))
        master = self._master_strip(kk, model["buses"][kk],
                                    sorted(model["buses"]))
        master.set_margin_top(8)
        master.set_margin_bottom(8)
        master.set_margin_end(8)
        page.append(master)
        return page

    def _inputs_page(self, inputs):
        sc = Gtk.ScrolledWindow(hexpand=True, vexpand=True)
        sc.set_policy(Gtk.PolicyType.AUTOMATIC, Gtk.PolicyType.NEVER)
        sc.set_child(self._grouped_row(
            inputs, lambda tok: self._input_strip(tok, inputs[tok])))
        return sc

    def _clock_page(self, ctrls, slots=None):
        sc = Gtk.ScrolledWindow(hexpand=True, vexpand=True)
        grid = Gtk.Grid(row_spacing=8, column_spacing=14, margin_top=16,
                        margin_bottom=16, margin_start=16, margin_end=16)
        if slots:
            lbl = Gtk.Label(label="Interfaces", xalign=0, hexpand=True,
                            valign=Gtk.Align.START)
            lbl.add_css_class("dim-label")
            grid.attach(lbl, 0, -1, 1, 1)
            val = Gtk.Label(label="   ".join(
                f"{s} · {m}" for s, m in sorted(slots.items())), xalign=0)
            val.add_css_class("db")
            grid.attach(val, 1, -1, 1, 1)
        for r, c in enumerate(ctrls):
            lbl = Gtk.Label(label=c["name"], xalign=0, hexpand=True,
                            valign=Gtk.Align.START)
            lbl.add_css_class("dim-label")
            grid.attach(lbl, 0, r, 1, 1)
            if c["name"] == "Sample Rate" and c["type"] == "enum":
                grid.attach(self._rate_selector(c), 1, r, 1, 1)
            else:
                grid.attach(self._inline_widget(c), 1, r, 1, 1)
        sc.set_child(grid)
        return sc

    def _rate_selector(self, c):
        """Sample-rate picker with the rates grouped by 1x/2x/4x clock family."""
        def fam_of(hz):
            return "1x" if hz <= 48000 else ("2x" if hz <= 96000 else "4x")
        outer = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=3)
        row = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=10)
        btns, groups, first = {}, {}, None
        for it in (c["items"] or []):
            try:
                hz = int(it)
            except ValueError:
                continue
            fam = fam_of(hz)
            if fam not in groups:
                cell = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=1)
                linked = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL)
                linked.add_css_class("linked")
                fl = Gtk.Label(label=fam)
                fl.add_css_class("sub")
                cell.append(linked)
                cell.append(fl)
                row.append(cell)
                groups[fam] = linked
            b = Gtk.ToggleButton(label=f"{hz / 1000:g}k")
            b.add_css_class("tbtn")
            if first is None:
                first = b
            else:
                b.set_group(first)              # radio behaviour
            groups[fam].append(b)
            btns[it] = b
        self._syncing = True                    # initial selection is not a write
        try:
            if c["value"].strip() in btns:
                btns[c["value"].strip()].set_active(True)
        finally:
            self._syncing = False
        for it, b in btns.items():
            b.connect("toggled", self._on_rate, c["name"], it)
        self._register(c["name"],
                       lambda v, bt=btns:
                       bt[v.strip()].set_active(True) if v.strip() in bt else None)
        warn = Gtk.Label(label="Channel counts per interface shrink in the "
                               "2x/4x families (AudioWire bandwidth).",
                         xalign=0, wrap=True)
        warn.add_css_class("sub")
        outer.append(row)
        outer.append(warn)
        return outer

    def _on_rate(self, btn, name, item):
        if self._syncing or not btn.get_active():
            return
        self._touch(name)
        self._apply(name, item)

    # -- strips -------------------------------------------------------------
    def _send_strip(self, kk, nn, send):
        box = self._strip_box(tok_cap(nn), master=False)
        vol = send.get("vol")
        row = self._fader_row(vol, default=self._unity(vol),
                              on_change=lambda v, user, kk=kk, nn=nn:
                              self._fan_send(kk, nn, v, user))
        box.append(row)
        pan = send.get("pan")
        if pan:
            box.append(self._pan(pan))
        btns = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=4,
                       halign=Gtk.Align.CENTER)
        if send.get("mute"):
            btns.append(self._toggle(send["mute"], "M", "m"))
        if send.get("solo"):
            btn = self._toggle(send["solo"], "S", "s", solo=True)
            btns.append(btn)
        g = Gtk.ToggleButton(label="G")
        g.add_css_class("tbtn")
        g.add_css_class("g")
        g.set_tooltip_text("Gang — ganged strips in this mix move together, "
                           "keeping their relative offsets")
        g.connect("toggled", self._on_gang, kk, nn)
        btns.append(g)
        box.append(btns)
        box.append(self._name_label(nn))
        return box

    def _master_strip(self, kk, bus, all_buses):
        box = self._strip_box(f"MIX {kk:02d}", master=True)
        mv = bus.get("master")
        box.append(self._fader_row(mv, default=self._unity(mv)))
        if bus.get("mute"):
            btn = self._toggle(bus["mute"], "MUTE", "m")
            btn.set_halign(Gtk.Align.CENTER)
            box.append(btn)
        ops = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=3,
                      halign=Gtk.Align.CENTER)
        others = [t for t in all_buses if t != kk]
        if others:
            mb = Gtk.MenuButton(label="CPY")
            mb.add_css_class("tbtn")
            mb.set_tooltip_text("Copy this mix's sends to another mix")
            pop = Gtk.Popover()
            pb = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=2)
            for t in others:
                b = Gtk.Button(label=f"→ Mix {t:02d}")
                b.connect("clicked", self._on_copy_mix, kk, t, pop)
                pb.append(b)
            pop.set_child(pb)
            mb.set_popover(pop)
            ops.append(mb)
        rst = Gtk.Button(label="RST")
        rst.add_css_class("tbtn")
        rst.set_tooltip_text("Reset this mix: sends to unity, pans centred, "
                             "mutes and solos off")
        rst.connect("clicked", lambda *_, kk=kk: self._reset_mix(kk))
        ops.append(rst)
        box.append(ops)
        sub = Gtk.Label(label="MASTER")
        sub.add_css_class("sub")
        box.append(sub)
        return box

    def _input_strip(self, nn, inp):
        box = self._strip_box(tok_cap(nn), master=False)
        trim = inp.get("trim")
        box.append(self._fader_row(trim, meter=False, default=trim["min"] if trim else 0,
                                   fmt=lambda v: f"+{v} dB" if v > 0 else f"{v} dB"))
        sub = Gtk.Label(label="TRIM", xalign=0.5)
        sub.add_css_class("sub")
        box.append(sub)
        btns = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=3,
                       halign=Gtk.Align.CENTER)
        for key, cap, cls in (("pad", "PAD", "p"), ("phase", "Ø", "p"),
                              ("stereo", "ST", "link"), ("mute", "M", "m")):
            if inp.get(key):
                b = self._toggle(inp[key], cap, cls)
                if key == "stereo":
                    b.set_tooltip_text("Stereo pair — links this input's send "
                                       "faders with its partner")
                    b.connect("toggled", lambda *_: GLib.idle_add(self.reload)
                              if not self._syncing else None)
                btns.append(b)
        box.append(btns)
        box.append(self._name_label(nn))
        return box

    def _fan_out(self, nn, v, user):
        """Fan an output-volume move out to its stereo partner (absolute)."""
        if not user or self._fanning:
            return
        partner = self._out_pair_of.get(nn)
        s = self._out_scales.get(partner) if partner is not None else None
        if not s:
            return
        self._fanning = True
        try:
            s.set_value(v)                      # its handler writes it out
        finally:
            self._fanning = False

    def _output_strip(self, nn, out):
        """Mono monitor strip for one output: volume, mute, stereo link,
        routed source. ST-linked pairs move their volume faders together."""
        box = self._strip_box(tok_cap(nn, out=True), master=False)
        vol = out.get("vol")
        box.append(self._fader_row(vol, default=self._unity(vol),
                                   on_change=lambda v, user, nn=nn:
                                   self._fan_out(nn, v, user)))
        btns = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=3,
                       halign=Gtk.Align.CENTER)
        if out.get("mute"):
            btns.append(self._toggle(out["mute"], "M", "m"))
        if out.get("stereo"):
            b = self._toggle(out["stereo"], "ST", "link")
            b.set_tooltip_text("Stereo pair — links this pair's volume faders; "
                               "on the patchbay the pair patches as one (L/R)")
            b.connect("toggled", lambda *_: GLib.idle_add(self.reload)
                      if not self._syncing else None)
            btns.append(b)
        box.append(btns)
        src = out.get("src")
        if src:
            lbl = Gtk.Label(label="")
            lbl.add_css_class("sub")
            def show(_v=None, l=lbl, name=src["name"],
                     normal=tok_normal_val(nn)):
                pb = self._byname.get("Patchbay Switch")
                bypassed = bool(pb) and pb["value"].split(",")[0].strip() != "on"
                # bypassed, the hardware falls back to the output's normal
                v = normal if bypassed else self._byname[name]["value"].strip()
                txt = ("← direct" if v == "Direct"
                       else "← system stereo" if v == normal
                       else f"← {v}")
                l.set_text(txt + (" (patchbay off)" if bypassed else ""))
            show()
            self._observe(src["name"], show)
            self._observe("Patchbay Switch", show)
            box.append(lbl)
        box.append(self._name_label(nn, kind="out",
                                    default=tok_label(nn, out=True).lower()))
        return box

    def _outputs_page(self, outputs):
        sc = Gtk.ScrolledWindow(hexpand=True, vexpand=True)
        sc.set_policy(Gtk.PolicyType.AUTOMATIC, Gtk.PolicyType.NEVER)
        sc.set_child(self._grouped_row(
            outputs, lambda tok: self._output_strip(tok, outputs[tok])))
        return sc

    def _patchbay_page(self, model):
        """The drawn patchbay: source jacks wired to output jacks by virtual
        cables. The Patchbay switch bypasses the whole thing (direct routing)
        and dims the bay."""
        outputs, pb = model["outputs"], model["patchbay"]
        outer = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=10,
                        margin_top=16, margin_bottom=16, margin_start=16,
                        margin_end=16)
        outer.add_css_class("patch")

        outs = [t for t in sorted(outputs, key=tok_sort)
                if outputs[t].get("src")]
        # PCM channel MM = the direct feed of output #MM in enumeration order
        pcm_of = {t: j for j, t in enumerate(outs)}
        items = (outputs[outs[0]]["src"]["items"] or []) if outs else []
        sources = [(("pcm", j), f"PCM {j + 1}") for j in range(len(outs))]
        for it in items:
            m = re.match(r"^Mix (\d+) ([LR])$", it)
            if m:
                sources.append(((("mix", int(m.group(1)), m.group(2))),
                                f"Mix {int(m.group(1)):02d} {m.group(2)}"))

        def conn_of(tok):
            v = self._byname[outputs[tok]["src"]["name"]]["value"].strip()
            if v == "Direct":
                return ("pcm", pcm_of[tok])
            m = re.match(r"^PCM (\d+)$", v)
            if m:
                return ("pcm", int(m.group(1)))
            m = re.match(r"^Mix (\d+) ([LR])$", v)
            if m:
                return ("mix", int(m.group(1)), m.group(2))
            return None

        def normal_of(tok):
            v = tok_normal_val(tok)
            m = re.match(r"^PCM (\d+)$", v)
            if m and int(m.group(1)) < len(outs):
                return ("pcm", int(m.group(1)))
            return ("pcm", pcm_of[tok])

        bay = PatchBay(sources, outs, self._out_pair_of, conn_of,
                       lambda t, sid, o=outputs, p=pcm_of:
                       self._patch_set(o, p, t, sid),
                       lambda t: self.names.get(f"out:{t}",
                                                tok_label(t, out=True)),
                       normal_of)
        for nn in outs:
            name = outputs[nn]["src"]["name"]
            # _sync keeps Source in snapshots/scenes and follows external
            # changes; _obs redraws on local cable drags too
            self._register(name, lambda _v, b=bay: b.queue_draw())
            self._observe(name, lambda _v, b=bay: b.queue_draw())

        head = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=10)
        head.append(Gtk.Label(label="Virtual patchbay", xalign=0, hexpand=True))

        def unpatch_all(_b):
            # pull every cord: back to the normals, as one undo step
            want = {}
            for t in outs:
                c = outputs[t]["src"]
                val = tok_normal_val(t)
                if val != "Direct" and val not in (c["items"] or []):
                    val = "Direct"
                if self._byname[c["name"]]["value"].strip() != val:
                    want[c["name"]] = val
            if not want:
                self._show_status("patchbay already at its normals")
                return
            n = self._apply_snapshot(want)
            self._show_status(f"unpatched {n} outputs back to their normals "
                              "(Ctrl+Z undoes)")

        clear = Gtk.Button(label="Unpatch all")
        clear.add_css_class("tbtn")
        clear.set_tooltip_text("Pull every cord — return all outputs to "
                               "their normals (one Ctrl+Z undoes it)")
        clear.connect("clicked", unpatch_all)
        head.append(clear)
        if pb:
            def on(v):
                return v.split(",")[0].strip() == "on"
            sw = self._inline_widget(pb)
            sw.set_tooltip_text("Enable the patchbay — off bypasses it: every "
                                "output carries its own PCM channel")
            head.append(sw)
            bay.set_sensitive(on(pb["value"]))
            clear.set_sensitive(on(pb["value"]))
            self._observe(pb["name"],
                          lambda v, b=bay, c=clear: (b.set_sensitive(on(v)),
                                                     c.set_sensitive(on(v)),
                                                     b.queue_draw()))
        outer.append(head)
        note = Gtk.Label(label="Optional — nothing is patched out of the box; "
                               "every output sits at its normal, like a bay "
                               "with no cords: its own direct PCM feed, except "
                               "Main outs which are normalled to the system "
                               "stereo program (PCM 1-2, the dashed line) so "
                               "desktop sound reaches the monitors. Drag a "
                               "virtual cable between a source jack and an "
                               "output jack to patch; double-click an output "
                               "jack to unpatch (back to its normal). "
                               "ST-linked pairs (bracketed) patch together as "
                               "L/R. Switched off, the whole patchbay is "
                               "bypassed onto those same normals.",
                         xalign=0, wrap=True)
        note.add_css_class("sub")
        outer.append(note)
        outer.append(bay)
        sc = Gtk.ScrolledWindow(hexpand=True, vexpand=True)
        sc.set_child(outer)
        return sc

    def _patch_set(self, outputs, pcm_of, tok, sid):
        """Write one patchbay connection. An ST-linked pair patches as a unit:
        a mix lands as L/R, a PCM feed as the even-aligned consecutive pair."""
        partner = self._out_pair_of.get(tok)
        if partner is not None:
            e, o = ((tok, partner) if tok_parts(tok)[2] % 2 == 0
                    else (partner, tok))
            if sid[0] == "mix":
                writes = [(e, ("mix", sid[1], "L")), (o, ("mix", sid[1], "R"))]
            else:
                mm = sid[1] - (sid[1] % 2)
                writes = [(e, ("pcm", mm)), (o, ("pcm", mm + 1))]
        else:
            writes = [(tok, sid)]
        for ch, s in writes:
            c = outputs.get(ch, {}).get("src")
            if not c:
                continue
            if s[0] == "pcm":
                val = ("Direct" if s[1] == pcm_of.get(ch)
                       else f"PCM {s[1]:02d}")
            else:
                val = f"Mix {s[1]:02d} {s[2]}"
            if val != "Direct" and val not in (c["items"] or []):
                self._show_status(f"{c['name']}: '{val}' is not available on "
                                  "this card", err=True)
                continue
            self._touch(c["name"])
            self._apply(c["name"], val)

    def _name_label(self, nn, kind="in", default=None):
        """Editable, persisted channel name (kept in sync across tabs)."""
        key = f"{kind}:{nn}"
        if default is None:
            slot, bank, idx = tok_parts(nn)
            default = (tok_label(nn).lower() if bank or slot
                       else f"ch {idx + 1}")
        lbl = Gtk.EditableLabel(text=self.names.get(key, default))
        lbl.set_halign(Gtk.Align.CENTER)
        lbl.set_max_width_chars(7)
        lbl.connect("notify::editing", self._on_name_edit, key, default)
        self._name_labels.setdefault(key, []).append(lbl)
        return lbl

    def _strip_box(self, caption, master):
        box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=6,
                      halign=Gtk.Align.CENTER)
        box.add_css_class("strip")
        if master:
            box.add_css_class("master")
        cap = Gtk.Label(label=caption)
        cap.add_css_class("cap")
        box.append(cap)
        return box

    @staticmethod
    def _unity(c):
        """Double-click reset target for a level fader (vendor default = 100/127)."""
        if not c:
            return 0
        lo = c["min"] if c["min"] is not None else 0
        hi = c["max"] if c["max"] is not None else 127
        return 100 if (lo, hi) == (0, 127) else hi

    def _fader_row(self, c, meter=True, fmt=None, default=None, on_change=None):
        fmt = fmt or str
        row = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=5,
                      halign=Gtk.Align.CENTER)
        if meter:
            m = Meter()
            self.meters.append([m, len(self.meters) * 0.7])
            row.append(m)
        lo = c["min"] if c and c["min"] is not None else 0
        hi = c["max"] if c and c["max"] is not None else 127
        if hi <= lo:
            hi = lo + 1
        f = Gtk.Scale.new_with_range(Gtk.Orientation.VERTICAL, lo, hi, 1)
        f.add_css_class("fader")
        f.set_inverted(True)
        f.set_draw_value(False)
        f.set_size_request(-1, 168)
        if default is not None and lo < default < hi:
            f.add_mark(default, Gtk.PositionType.LEFT, None)
        val = Gtk.EditableLabel()
        val.add_css_class("db")
        val.set_halign(Gtk.Align.CENTER)
        try:
            f.set_value(int(c["value"].split(",")[0]) if c else lo)
        except (ValueError, AttributeError):
            f.set_value(lo)
        val.set_text(fmt(int(f.get_value())))
        if c:
            f.set_tooltip_text(c["name"])
            val.set_tooltip_text("Click to type an exact value")
            f.connect("value-changed", self._on_fader, c["name"], val, fmt, on_change)
            val.connect("notify::editing", self._on_val_edit, f, fmt)
            if default is not None:
                dbl = Gtk.GestureClick()
                dbl.connect("pressed",
                            lambda _g, n, _x, _y, s=f, d=default:
                            s.set_value(d) if n == 2 else None)
                f.add_controller(dbl)
            self._register(c["name"],
                           lambda v, s=f: s.set_value(self._as_int(v, lo)))
            m = _SND.match(c["name"])
            if m and m.group(3) == "Volume":
                key = (int(m.group(1)), m.group(2))
                self._send_scales[key] = f
                self._last_val[key] = int(f.get_value())
            m = _OUT.match(c["name"])
            if m and m.group(2) == "Volume":
                self._out_scales[m.group(1)] = f
        wrap = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=3,
                       halign=Gtk.Align.CENTER)
        row.append(f)
        wrap.append(row)
        wrap.append(val)
        return wrap

    def _pan(self, c):
        box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=1,
                      halign=Gtk.Align.CENTER)
        name = c["name"]
        lo = c["min"] if c["min"] is not None else -100
        hi = c["max"] if c["max"] is not None else 100
        centre = (lo + hi) // 2
        val = self._as_int(c["value"], centre)
        lbl = Gtk.EditableLabel()
        lbl.add_css_class("sub")
        lbl.set_halign(Gtk.Align.CENTER)
        lbl.set_tooltip_text("Click to type: L20, C, R35 or a raw value")
        def show(v):
            lbl.set_text("C" if v == centre
                         else (f"L{centre - v}" if v < centre else f"R{v - centre}"))
        def changed(v):
            show(v)
            self._touch(name)
            self._apply(name, str(v))
        knob = Knob(lo, hi, val, changed)
        knob.set_halign(Gtk.Align.CENTER)
        knob.set_tooltip_text(f"{name} — drag up/down (Ctrl: fine), "
                              "scroll, double-click: centre")
        show(val)
        def typed(_l, _p):
            if lbl.get_editing():
                return
            t = lbl.get_text().strip().upper()
            v = None
            if t in ("C", ""):
                v = centre
            else:
                m = re.match(r"^([LR])\s*(\d+)$", t)
                if m:
                    off = int(m.group(2))
                    v = centre - off if m.group(1) == "L" else centre + off
                elif re.match(r"^-?\d+$", t):
                    v = int(t)
            if v is not None and v != knob.value:
                knob._set(max(lo, min(hi, v)))  # user path: applies + re-shows
            else:
                show(knob.value)
        lbl.connect("notify::editing", typed)
        self._register(name,
                       lambda v, k=knob:
                       (k.set_value_silent(self._as_int(v, centre)), show(k.value)))
        box.append(knob)
        box.append(lbl)
        return box

    def _toggle(self, c, cap, cls, solo=False):
        b = Gtk.ToggleButton(label=cap)
        b.add_css_class("tbtn")
        b.add_css_class(cls)
        b.set_active(c["value"].split(",")[0].strip() == "on")
        b.set_tooltip_text(c["name"])
        b.connect("toggled", self._on_toggle, c["name"])
        self._register(c["name"],
                       lambda v, b=b: b.set_active(v.split(",")[0].strip() == "on"))
        if solo:
            self._solo_btns.append(b)
            b.connect("toggled", lambda *_: self._update_solo_ind())
        return b

    def _inline_widget(self, c):
        if c["type"] == "bool":
            sw = Gtk.Switch(halign=Gtk.Align.START, valign=Gtk.Align.CENTER)
            sw.set_active(c["value"].split(",")[0].strip() == "on")
            sw.connect("state-set", self._on_switch, c["name"])
            self._register(c["name"],
                           lambda v, sw=sw: sw.set_active(v.split(",")[0].strip() == "on"))
            return sw
        if c["type"] == "enum":
            items = c["items"] or []
            dd = Gtk.DropDown.new_from_strings(items)
            if c["value"].strip() in items:
                dd.set_selected(items.index(c["value"].strip()))
            dd.connect("notify::selected", self._on_enum, c["name"], items)
            self._register(c["name"],
                           lambda v, dd=dd, it=items:
                           dd.set_selected(it.index(v.strip())) if v.strip() in it else None)
            return dd
        lbl = Gtk.Label(label=c["value"], xalign=0)
        lbl.add_css_class("db")
        self._register(c["name"], lambda v, l=lbl: l.set_text(v))
        return lbl  # RO-ish (e.g. Clock Rate); editable ints live in strips

    def _generic(self, ctrls):
        sc = Gtk.ScrolledWindow(hexpand=True, vexpand=True)
        grid = Gtk.Grid(row_spacing=8, column_spacing=14, margin_top=16,
                        margin_bottom=16, margin_start=16, margin_end=16)
        for r, c in enumerate(ctrls):
            lbl = Gtk.Label(label=c["name"], xalign=0, hexpand=True)
            lbl.add_css_class("dim-label")
            grid.attach(lbl, 0, r, 1, 1)
            if c["type"] == "int":
                lo = c["min"] if c["min"] is not None else 0
                hi = max((c["max"] if c["max"] is not None else 100), lo + 1)
                w = Gtk.Scale.new_with_range(Gtk.Orientation.HORIZONTAL, lo, hi, 1)
                w.set_hexpand(True)
                w.set_size_request(220, -1)
                w.set_draw_value(True)
                try:
                    w.set_value(int(c["value"].split(",")[0]))
                except ValueError:
                    pass
                w.connect("value-changed", self._on_scale, c["name"])
                self._register(c["name"],
                               lambda v, s=w, lo=lo: s.set_value(self._as_int(v, lo)))
            else:
                w = self._inline_widget(c)
            grid.attach(w, 1, r, 1, 1)
        sc.set_child(grid)
        return sc

    def _notice(self, title, subtitle):
        b = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=8,
                    valign=Gtk.Align.CENTER, vexpand=True)
        t = Gtk.Label()
        t.set_markup(f"<big>{GLib.markup_escape_text(title)}</big>")
        s = Gtk.Label(label=subtitle, wrap=True,
                      justify=Gtk.Justification.CENTER)
        s.add_css_class("dim-label")
        b.append(t)
        b.append(s)
        return b

    # -- sync / polling -------------------------------------------------------
    def _register(self, name, setter):
        self._sync[name] = setter

    def _observe(self, name, cb):
        """Passive follower of a control's value (labels, sensitivity, …) —
        called on local writes and external changes alike, unlike _register
        which owns the control's primary widget."""
        self._obs.setdefault(name, []).append(cb)

    def _notify(self, name, value):
        for cb in self._obs.get(name, ()):
            cb(value)

    @staticmethod
    def _as_int(v, fallback):
        try:
            return int(str(v).split(",")[0])
        except ValueError:
            return fallback

    @staticmethod
    def _family_div(hz):
        """Rate -> AudioWire family divisor (1x/2x/4x)."""
        try:
            hz = int(str(hz).split(",")[0])
        except ValueError:
            return 1
        return 1 if hz <= 48000 else (2 if hz <= 96000 else 4)

    def _poll(self):
        """Follow changes made outside the GUI (alsamixer, another instance),
        and the card itself coming, going, or re-registering controls."""
        if DEMO or self._poll_busy:
            return True
        self._poll_busy = True
        dev = self.current_dev()
        def worker():
            rc, out, _err = run_ctl(["list"], dev)
            GLib.idle_add(self._poll_done, rc, out)
        threading.Thread(target=worker, daemon=True).start()
        return True

    def _poll_done(self, rc, out):
        self._poll_busy = False
        if rc != 0:
            if self._ctl_names:                 # card was there, is gone now
                self.reload()
                self._show_status("card disappeared — console cleared",
                                  err=True)
            return False
        ctrls = parse_list(out)
        if {c["name"] for c in ctrls} != self._ctl_names:
            # driver (un)registered controls: module load, hot-plug, or the
            # 2x/4x rate-family channel shrink — rebuild around the new set
            self.reload()
            self._show_status("card control set changed — console rebuilt")
            return False
        now = time.monotonic()
        self._syncing = True
        try:
            for c in ctrls:
                name = c["name"]
                if name not in self._sync:
                    continue
                if now - self._touched.get(name, 0) < TOUCH_GRACE:
                    continue                    # user is on this control
                if self.writer.pending(name):
                    continue                    # our own write is in flight
                if name in self._byname and self._byname[name]["value"] == c["value"]:
                    continue
                self._byname.setdefault(name, c)["value"] = c["value"]
                self._sync[name](c["value"])
                self._notify(name, c["value"])
        finally:
            self._syncing = False
        self._update_clock_label()
        self._update_solo_ind()
        return False

    # -- solo / stereo link ----------------------------------------------------
    def _update_solo_ind(self):
        if any(b.get_active() for b in self._solo_btns):
            self.solo_ind.add_css_class("on")
        else:
            self.solo_ind.remove_css_class("on")

    def _clear_solos(self, *_):
        for b in self._solo_btns:
            if b.get_active():
                b.set_active(False)             # handler pushes the write
        self._show_status("all solos cleared")

    def _on_gang(self, btn, kk, nn):
        if btn.get_active():
            self._gang.add((kk, nn))
        else:
            self._gang.discard((kk, nn))

    def _fan_send(self, kk, nn, v, user):
        """Fan a send-fader move out to its stereo partner (absolute) and to
        ganged strips in the same mix (relative, keeping offsets)."""
        prev = self._last_val.get((kk, nn), v)
        self._last_val[(kk, nn)] = v
        if not user or self._fanning:
            return
        targets = {}
        partner = self._pair_of.get(nn)
        if partner is not None:
            targets[partner] = "abs"
        if (kk, nn) in self._gang:
            for (k, m) in self._gang:
                if k == kk and m != nn:
                    targets.setdefault(m, "rel")
        if not targets:
            return
        delta = v - prev
        self._fanning = True
        try:
            for m, mode in targets.items():
                s = self._send_scales.get((kk, m))
                if s:                           # each handler writes it out
                    s.set_value(v if mode == "abs" else s.get_value() + delta)
        finally:
            self._fanning = False

    # -- snapshots --------------------------------------------------------------
    def _snapshot(self):
        return {c["name"]: c["value"]
                for c in self._byname.values()
                if c["name"] in self._sync and c["name"] != "Clock Rate"}

    def _snap_save(self, *_):
        if not self._byname:
            self._show_status("nothing to save", err=True)
            return
        os.makedirs(SNAP_DIR, exist_ok=True)
        dlg = Gtk.FileDialog()
        dlg.set_initial_folder(Gio.File.new_for_path(SNAP_DIR))
        dlg.set_initial_name(time.strftime("mix-%Y%m%d-%H%M%S.json"))
        dlg.save(self, None, self._snap_save_done)

    def _snap_save_done(self, dlg, res):
        try:
            gfile = dlg.save_finish(res)
        except GLib.Error:
            return
        path = gfile.get_path()
        try:
            _save_json(path, {"motu424-mix": 1, "controls": self._snapshot()})
        except OSError as e:
            self._show_status(f"snapshot save failed: {e}", err=True)
            return
        self._show_status(f"snapshot saved: {os.path.basename(path)}")

    def _snap_load(self, *_):
        os.makedirs(SNAP_DIR, exist_ok=True)
        dlg = Gtk.FileDialog()
        dlg.set_initial_folder(Gio.File.new_for_path(SNAP_DIR))
        dlg.open(self, None, self._snap_load_done)

    def _snap_load_done(self, dlg, res):
        try:
            gfile = dlg.open_finish(res)
        except GLib.Error:
            return
        data = _load_json(gfile.get_path(), None)
        if not isinstance(data, dict) or "controls" not in data:
            self._show_status("not a motu424 mix snapshot", err=True)
            return
        n = self._apply_snapshot(data["controls"])
        self._show_status(f"snapshot applied ({n} controls)")

    def _apply_snapshot(self, controls, undoable=True):
        """Set widgets silently and push every value to the card. Unless
        `undoable` is off, the previous values go onto the undo stack."""
        prev = {n: self._byname[n]["value"] for n in controls
                if n in self._byname and n in self._sync}
        applied = 0
        self._syncing = True
        try:
            for name, value in controls.items():
                if name not in self._sync:
                    continue
                self._sync[name](value)
                self._apply(name, value)
                applied += 1
        finally:
            self._syncing = False
        if undoable and applied and prev:
            self._undo_stack.append(prev)
            del self._undo_stack[:-20]
        self._update_solo_ind()
        return applied

    def _on_undo(self, *_):
        if not self._undo_stack:
            self._show_status("nothing to undo", err=True)
            return
        n = self._apply_snapshot(self._undo_stack.pop(), undoable=False)
        self._show_status(f"undid last mix change ({n} controls, "
                          f"{len(self._undo_stack)} more)")

    # -- scenes / mix operations ---------------------------------------------
    def _on_scene(self, _btn, slot):
        if self.scene_set.get_active():
            self.scene_set.set_active(False)
            if not self._byname:
                self._show_status("nothing to store", err=True)
                return
            self.scenes[slot] = self._snapshot()
            try:
                _save_json(SCENES_FILE, self.scenes)
            except OSError as e:
                self._show_status(f"cannot save scenes: {e}", err=True)
                return
            self._show_status(
                f"scene {slot} stored ({len(self.scenes[slot])} controls)")
            return
        snap = self.scenes.get(slot)
        if not snap:
            self._show_status(
                f"scene {slot} is empty — press SET then {slot} to store it",
                err=True)
            return
        n = self._apply_snapshot(snap)
        self._show_status(f"scene {slot} recalled ({n} controls)")

    def _on_copy_mix(self, _btn, src, dst, pop):
        pop.popdown()
        payload = {}
        for name, c in self._byname.items():
            m = _SND.match(name)
            if not m or int(m.group(1)) != src:
                continue
            payload[f"Mix {dst:02d} Input {m.group(2)} {m.group(3)}"] = c["value"]
        n = self._apply_snapshot(payload)
        self._show_status(f"Mix {src:02d} sends copied to Mix {dst:02d} "
                          f"({n} controls)")

    def _reset_mix(self, kk):
        payload = {}
        for name, c in self._byname.items():
            m = _SND.match(name)
            if not m or int(m.group(1)) != kk:
                continue
            field = m.group(3)
            if field == "Volume":
                payload[name] = str(self._unity(c))
            elif field == "Pan":
                lo = c["min"] if c["min"] is not None else -100
                hi = c["max"] if c["max"] is not None else 100
                payload[name] = str((lo + hi) // 2)
            else:
                payload[name] = "off"
        n = self._apply_snapshot(payload)
        self._show_status(f"Mix {kk:02d} reset ({n} controls)")

    # -- diagnostics -----------------------------------------------------------
    def _diag_page(self):
        box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL)
        bar = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=6,
                      margin_top=6, margin_bottom=6, margin_start=8, margin_end=8)
        btn = Gtk.Button(label="Refresh diagnostics")
        btn.connect("clicked", lambda *_: self._diag_refresh())
        bar.append(btn)
        cp = Gtk.Button(label="Copy report")
        cp.set_tooltip_text("Copy the whole report to the clipboard — handy "
                            "for bug reports")
        cp.connect("clicked", self._diag_copy)
        bar.append(cp)
        self.diag_spin = Gtk.Spinner()
        bar.append(self.diag_spin)
        box.append(bar)
        sc = Gtk.ScrolledWindow(hexpand=True, vexpand=True)
        self.diag_view = Gtk.TextView(editable=False, monospace=True,
                                      cursor_visible=False,
                                      left_margin=10, right_margin=10,
                                      top_margin=6, bottom_margin=6)
        self.diag_view.add_css_class("diagview")
        sc.set_child(self.diag_view)
        box.append(sc)
        self._diag_refresh()
        return box

    def _diag_refresh(self):
        self.diag_spin.start()
        dev = self.current_dev()
        threading.Thread(target=self._diag_collect, args=(dev,),
                         daemon=True).start()

    def _diag_collect(self, dev):
        lines = []
        add = lines.append
        add("== MOTU PCI card (vendor 0x137a) ==")
        models = {"0x0003": "PCI-324", "0x0004": "PCI-424",
                  "0x0005": "PCIe-424 (HD Express)"}
        found = False
        base = "/sys/bus/pci/devices"
        try:
            for d in sorted(os.listdir(base)):
                p = os.path.join(base, d)
                def rd(attr, p=p):
                    try:
                        with open(os.path.join(p, attr)) as f:
                            return f.read().strip()
                    except OSError:
                        return "?"
                if rd("vendor").lower() != "0x137a":
                    continue
                found = True
                did = rd("device").lower()
                drv = os.path.join(p, "driver")
                bound = (os.path.basename(os.path.realpath(drv))
                         if os.path.exists(drv) else "(unbound)")
                add(f"  {d}  device {did} ({models.get(did, 'unknown model')})"
                    f"  rev {rd('revision')}  irq {rd('irq')}  driver {bound}")
        except OSError as e:
            add(f"  scan failed: {e}")
        if not found:
            add("  none found")
        add("")
        add("== motu424 kernel module ==")
        if os.path.isdir("/sys/module/motu424"):
            add("  loaded: yes")
            pdir = "/sys/module/motu424/parameters"
            if os.path.isdir(pdir):
                for prm in sorted(os.listdir(pdir)):
                    try:
                        with open(os.path.join(pdir, prm)) as f:
                            add(f"  param {prm} = {f.read().strip()}")
                    except OSError:
                        pass
        else:
            add("  loaded: no   (make load)")
        add("")
        add("== ALSA cards ==")
        try:
            with open("/proc/asound/cards") as f:
                txt = f.read().rstrip()
            add("  " + "\n  ".join(txt.splitlines()) if txt else "  (none)")
        except OSError as e:
            add(f"  {e}")
        add("")
        add(f"== motu424-ctl status ({dev or 'auto'}) ==")
        rc, out, err = run_ctl(["status"], dev)
        txt = (out or err or f"exit {rc}").strip()
        add("  " + "\n  ".join(txt.splitlines()) if txt else "  (empty)")
        add("")
        add("== kernel log (motu424) ==")
        try:
            p = subprocess.run(["journalctl", "-k", "--no-pager", "-n", "10",
                                "-g", "motu424"],
                               capture_output=True, text=True, timeout=5)
            txt = (p.stdout or p.stderr).strip()
            add("  " + "\n  ".join(txt.splitlines()) if txt else "  (nothing)")
        except (OSError, subprocess.TimeoutExpired) as e:
            add(f"  journalctl unavailable: {e}")
        GLib.idle_add(self._diag_done, "\n".join(lines))

    def _diag_done(self, text):
        self.diag_spin.stop()
        self.diag_view.get_buffer().set_text(text)
        return False

    def _diag_copy(self, *_):
        buf = self.diag_view.get_buffer()
        text = buf.get_text(buf.get_start_iter(), buf.get_end_iter(), False)
        self.get_clipboard().set(text)
        self._show_status("diagnostics report copied to the clipboard")

    # -- window niceties -------------------------------------------------------
    def _on_tab(self, _act, _param, i):
        if self._nb and i < self._nb.get_n_pages():
            self._nb.set_current_page(i)

    def _on_close(self, *_):
        state = {"w": self.get_width(), "h": self.get_height(),
                 "max": self.is_maximized(),
                 "page": self._nb.get_current_page() if self._nb else 0}
        try:
            _save_json(UI_FILE, state)
        except OSError:
            pass
        return False

    HELP_ROWS = (
        ("drag a fader", "set a send / master / trim level"),
        ("drag the pan pot vertically", "pan (hold Ctrl for fine moves)"),
        ("scroll on a fader or pot", "step the value"),
        ("double-click a fader / pot", "reset to unity / centre / 0"),
        ("click a value readout", "type an exact number (pan: L20, C, R35)"),
        ("click a meter", "clear its clip lamp"),
        ("click a channel name", "rename it (saved, shared across tabs)"),
        ("M / S / G buttons", "mute, solo, gang (ganged strips move together)"),
        ("ST on an input / output", "stereo pair — links the pair's faders"),
        ("SOLO (header)", "lit while any solo is on; click clears them all"),
        ("SET then A or B", "store a scene; A / B alone recalls it"),
        ("TALK / LISTEN (header)", "talkback / listenback into the mixes — "
                                   "hold to talk momentarily, a quick click "
                                   "latches"),
        ("CPY / RST (master strip)", "copy sends to another mix / reset the mix"),
        ("Patchbay tab", "optional, no cords by default (outputs sit at "
                         "their normals; Main outs carry the system stereo, "
                         "dashed); drag virtual cables to patch, double-click "
                         "a jack to unpatch, 'Unpatch all' pulls every cord, "
                         "hovering a jack shows what it carries; the switch "
                         "bypasses the bay"),
        ("Ctrl+Z", "undo the last mix-wide change"),
        ("Ctrl+S / Ctrl+O", "save / load a mix snapshot"),
        ("Ctrl+R", "reload controls"),
        ("Alt+1 … Alt+9", "switch tab"),
        ("F1", "this help (Esc closes it)"),
    )

    def _show_help(self, *_):
        if self._help_win:
            self._help_win.present()
            return
        win = Gtk.Window(transient_for=self, title="Shortcuts & tips",
                         default_width=520)
        win.add_css_class("help")
        grid = Gtk.Grid(row_spacing=6, column_spacing=18, margin_top=14,
                        margin_bottom=14, margin_start=16, margin_end=16)
        for r, (what, does) in enumerate(self.HELP_ROWS):
            k = Gtk.Label(label=what, xalign=0)
            k.add_css_class("db")
            d = Gtk.Label(label=does, xalign=0, wrap=True, hexpand=True)
            d.add_css_class("dim-label")
            grid.attach(k, 0, r, 1, 1)
            grid.attach(d, 1, r, 1, 1)
        sc = Gtk.ScrolledWindow(propagate_natural_height=True,
                                max_content_height=560)
        sc.set_child(grid)
        win.set_child(sc)
        esc = Gtk.EventControllerKey()
        esc.connect("key-pressed",
                    lambda _c, kv, *_a: (win.close(), True)[1]
                    if kv == Gdk.KEY_Escape else False)
        win.add_controller(esc)
        win.connect("close-request", self._on_help_close)
        self._help_win = win
        win.present()

    def _on_help_close(self, *_):
        self._help_win = None
        return False

    # -- meter animation (demo only; no per-channel level kcontrol exists) ---
    def _tick_meters(self):
        if not self.meters:
            return True
        dt = METER_TICK_MS / 1000.0
        if DEMO:
            t = time.monotonic()
            for m in self.meters:
                meter, ph = m
                v = 0.5 + 0.5 * math.sin(t * 3 + ph)
                v *= 0.35 + 0.65 * (0.5 + 0.5 * math.sin(t * 0.7 + ph * 2))
                meter.set_level(v)
        for meter, _ph in self.meters:
            meter.decay(dt)
        return True

    # -- handlers -----------------------------------------------------------
    def _apply(self, name, value):
        if name in self._byname:
            self._byname[name]["value"] = value
        self._notify(name, value)
        if DEMO:
            # the demo card locks instantly onto the requested rate, and
            # re-enumerates like the real driver will: analog/ADAT banks
            # carry fewer channels in the 2x/4x families
            if name == "Sample Rate" and "Clock Rate" in self._byname:
                self._byname["Clock Rate"]["value"] = value
                if "Clock Rate" in self._sync:
                    self._sync["Clock Rate"](value)
                self._update_clock_label()
                fam = self._family_div(value)
                if fam != self._demo_fam:
                    self._demo_fam = fam
                    old = {c["name"]: c["value"] for c in self.demo}
                    self.demo = demo_controls(family=fam)
                    for c in self.demo:
                        if c["name"] in ("Sample Rate", "Clock Rate"):
                            c["value"] = value
                        elif c["name"] in old:      # surviving channels keep state
                            c["value"] = old[c["name"]]
                    self._show_status(
                        f"demo: {fam}x family — analog/ADAT banks carry "
                        "fewer channels" if fam > 1 else
                        "demo: 1x family — full channel counts restored")
                    GLib.idle_add(self.reload)
            return
        self.writer.push(name, value, self.current_dev())

    def _on_write_error(self, msg):
        self._show_status(msg, err=True)
        return False

    def _show_status(self, msg, err=False):
        self.status.set_text(msg)
        if err:
            self.status.add_css_class("err")
        else:
            self.status.remove_css_class("err")
        self._status_seq += 1
        seq = self._status_seq
        GLib.timeout_add_seconds(
            5, lambda: (self.status.set_text("")
                        if self._status_seq == seq else None) and False)

    def _touch(self, name):
        self._touched[name] = time.monotonic()

    def _on_fader(self, scale, name, val_label, fmt, on_change):
        v = int(scale.get_value())
        val_label.set_text(fmt(v))
        if on_change:
            on_change(v, not self._syncing)     # fan-out tracks values either way
        if self._syncing:
            return
        self._touch(name)
        self._apply(name, str(v))

    def _on_val_edit(self, lbl, _pspec, scale, fmt):
        if lbl.get_editing():
            return
        m = re.search(r"-?\d+", lbl.get_text())
        if m:
            scale.set_value(int(m.group()))     # user path: handler applies
        lbl.set_text(fmt(int(scale.get_value())))

    def _on_scale(self, scale, name):
        if self._syncing:
            return
        self._touch(name)
        self._apply(name, str(int(scale.get_value())))

    def _on_toggle(self, btn, name):
        if self._syncing:
            return
        self._touch(name)
        self._apply(name, "on" if btn.get_active() else "off")

    TB_HOLD_S = 0.4      # held longer than this = momentary push-to-talk

    def _tb_press(self, g, _n, _x, _y, btn):
        g.set_state(Gtk.EventSequenceState.CLAIMED)    # we own the click
        btn._tb_t0 = time.monotonic()
        btn.set_active(not btn.get_active())           # engage on press

    def _tb_release(self, _g, _n, _x, _y, btn):
        # still on after a long press: it was push-to-talk — let go = stop
        if (btn.get_active()
                and time.monotonic() - getattr(btn, "_tb_t0", 0) > self.TB_HOLD_S):
            btn.set_active(False)

    def _on_switch(self, _sw, state, name):
        if not self._syncing:
            self._touch(name)
            self._apply(name, "on" if state else "off")
        return False

    def _on_enum(self, dd, _pspec, name, items):
        if self._syncing:
            return
        i = dd.get_selected()
        if 0 <= i < len(items):
            self._touch(name)
            self._apply(name, items[i])
            if name == "Clock Source":
                self._update_clock_label()

    def _on_name_edit(self, lbl, _pspec, key, default):
        if lbl.get_editing():
            return
        text = lbl.get_text().strip()
        if text:
            self.names[key] = text
        else:
            self.names.pop(key, None)
            lbl.set_text(default)
        for other in self._name_labels.get(key, []):
            if other is not lbl:
                other.set_text(text or default)
        try:
            _save_json(NAMES_FILE, self.names)
        except OSError as e:
            self._show_status(f"cannot save channel names: {e}", err=True)


class App(Gtk.Application):
    def __init__(self):
        super().__init__(application_id=APP_ID,
                         flags=Gio.ApplicationFlags.FLAGS_NONE)

    def do_activate(self):
        _install_css()
        self.set_accels_for_action("win.refresh", ["<Control>r"])
        self.set_accels_for_action("win.undo", ["<Control>z"])
        self.set_accels_for_action("win.snap-save", ["<Control>s"])
        self.set_accels_for_action("win.snap-load", ["<Control>o"])
        self.set_accels_for_action("win.help", ["F1"])
        for i in range(9):
            self.set_accels_for_action(f"win.tab{i}", [f"<Alt>{i + 1}"])
        win = self.props.active_window or Panel(self)
        win.present()


def main():
    if not CTL and not DEMO:
        sys.stderr.write(
            "motu424-ctl not found. Build/install it first "
            "(make tools, or ./install.sh), or preview with --demo.\n")
        return 1
    # Gtk.Application would choke on --demo; hand it only the program name.
    return App().run([sys.argv[0]])


if __name__ == "__main__":
    raise SystemExit(main())
