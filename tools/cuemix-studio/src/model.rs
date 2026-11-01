// SPDX-License-Identifier: GPL-2.0-or-later
use crate::mech::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum Kind {
    Mic,
    Line,
    Digital,
}

/// Which console layout the device gets.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum Family {
    /// PCI-324/424 core card + AudioWire interfaces: CueMix DSP mixer, no effects.
    Pci424,
    /// 828mk3-class: CueMix FX with EQ, dynamics and reverb.
    Mk3,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum BandType {
    One,
    Two,
    Shelf,
}

#[derive(Clone, PartialEq, Serialize, Deserialize, Debug)]
pub struct Band {
    pub on: bool,
    pub freq: f32,
    pub gain: f32,
    pub q: f32,
    pub kind: BandType,
}

#[derive(Clone, PartialEq, Serialize, Deserialize, Debug)]
pub struct Eq {
    pub on: bool,
    pub hpf_on: bool,
    pub hpf_freq: f32,
    pub hpf_slope12: bool,
    pub lpf_on: bool,
    pub lpf_freq: f32,
    pub lpf_slope12: bool,
    pub bands: [Band; 5],
}

pub const BAND_NAMES: [&str; 5] = ["low", "lo mid", "mid", "hi mid", "high"];

impl Default for Eq {
    fn default() -> Self {
        let b = |freq: f32, kind| Band { on: true, freq, gain: 0.0, q: 1.0, kind };
        Eq {
            on: false,
            hpf_on: false,
            hpf_freq: EQ_FREQ.0,
            hpf_slope12: true,
            lpf_on: false,
            lpf_freq: EQ_FREQ.1,
            lpf_slope12: true,
            bands: [
                b(60.0, BandType::Shelf),
                b(250.0, BandType::One),
                b(1000.0, BandType::One),
                b(4000.0, BandType::One),
                b(12000.0, BandType::Shelf),
            ],
        }
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize, Debug)]
pub struct Dyn {
    pub on: bool,
    pub comp_on: bool,
    pub threshold: f32,
    pub ratio: f32,
    pub attack: f32,
    pub release: f32,
    pub trim: f32,
    pub rms: bool,
    pub lev_on: bool,
    pub lev_limit: bool,
    pub lev_reduction: f32,
    pub lev_makeup: f32,
}

impl Default for Dyn {
    fn default() -> Self {
        Dyn {
            on: false,
            comp_on: true,
            threshold: -20.0,
            ratio: 2.0,
            attack: 10.0,
            release: 100.0,
            trim: 0.0,
            rms: false,
            lev_on: false,
            lev_limit: false,
            lev_reduction: 0.0,
            lev_makeup: 0.0,
        }
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize, Debug)]
pub struct Input {
    /// Channel token of the driver contract, e.g. "A Analog 00".
    pub token: String,
    /// Group header, e.g. "A · 24I/O — Analog".
    pub group: String,
    pub name: String,
    pub kind: Kind,
    pub trim: f32,
    pub trim_range: (f32, f32),
    pub pad: bool,
    pub phase: bool,
    pub stereo: bool,
    pub mute: bool,
    pub ms: bool,
    pub width: f32,
    pub swap: bool,
    pub ref_plus4: bool,
    pub vlimit: bool,
    pub lookahead: bool,
    pub softclip: bool,
    pub rev_send: f32,
    pub talk: bool,
    pub listen: bool,
    pub eq: Eq,
    pub dynamics: Dyn,
}

#[derive(Clone, PartialEq, Serialize, Deserialize, Debug)]
pub struct Send {
    pub fader: f32,
    pub pan: f32,
    pub mute: bool,
    pub solo: bool,
}

#[derive(Clone, PartialEq, Serialize, Deserialize, Debug)]
pub struct Mix {
    pub name: String,
    pub sends: Vec<Send>,
    pub master: f32,
    pub mute: bool,
    /// Index of the left output channel the mix is assigned to (usize::MAX = none).
    pub output: usize,
    pub rev_send: f32,
    pub rev_return: f32,
}

#[derive(Clone, PartialEq, Serialize, Deserialize, Debug)]
pub struct Output {
    pub token: String,
    pub group: String,
    pub name: String,
    pub fader: f32,
    pub mute: bool,
    pub stereo: bool,
    pub rev_send: f32,
    pub rev_return: f32,
    pub monitor: bool,
    pub talk: bool,
    pub listen: bool,
    pub eq: Eq,
    pub dynamics: Dyn,
}

#[derive(Clone, PartialEq, Serialize, Deserialize, Debug)]
pub struct Reverb {
    pub on: bool,
    pub split_outputs: bool,
    pub time: f32,
    pub predelay: f32,
    pub shelf_freq: f32,
    pub shelf_cut: f32,
    pub refl_size: f32,
    pub refl_level: f32,
    pub refl_set: usize,
    pub time_low: f32,
    pub time_mid: f32,
    pub time_high: f32,
    pub xover_low: f32,
    pub xover_high: f32,
    pub width: f32,
}

impl Default for Reverb {
    fn default() -> Self {
        Reverb {
            on: true,
            split_outputs: false,
            time: REV_TIME_CENTRE,
            predelay: REV_PREDELAY.0,
            shelf_freq: REV_SHELF_FREQ_DEFAULT,
            shelf_cut: 0.0,
            refl_size: REV_REFL_SIZE_DEFAULT,
            refl_level: 1.0,
            refl_set: 0,
            time_low: REV_TIME_LOW_DEFAULT,
            time_mid: REV_TIME_MID_DEFAULT,
            time_high: REV_TIME_HIGH_DEFAULT,
            xover_low: REV_XOVER_LOW_DEFAULT,
            xover_high: REV_XOVER_HIGH_DEFAULT,
            width: REV_WIDTH.1,
        }
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize, Debug)]
pub struct State {
    pub device: String,
    pub family: Family,
    /// AudioWire slots A–D → interface key ("24io", "1224", "2408", "hd192", "" = empty).
    pub rig: [String; 4],
    pub inputs: Vec<Input>,
    pub mixes: Vec<Mix>,
    pub outputs: Vec<Output>,
    pub reverb: Reverb,
    pub monitor_level: f32,
    pub talk: bool,
    pub listen: bool,
    pub talk_atten: f32,
    pub clock_source: String,
    pub sample_rate: u32,
    pub meters: bool,
    pub patchbay: bool,
}

/// AudioWire interfaces the PCI-424 rig can hold (banks: name, inputs, outputs),
/// from the person's motu424-gui RIG_IFACES table.
pub const RIG_IFACES: [(&str, &str, &[(&str, usize, usize)]); 4] = [
    ("24io", "24I/O", &[("Analog", 24, 24)]),
    ("1224", "1224", &[("Analog", 8, 8), ("AES", 2, 2), ("Main", 0, 2)]),
    ("2408", "2408mk3", &[("Analog", 8, 8), ("ADAT", 24, 24), ("SPDIF", 2, 2), ("Main", 0, 2)]),
    ("hd192", "HD192", &[("Analog", 12, 12), ("AES", 2, 2)]),
];

pub const CLOCK_SOURCES: [&str; 5] = ["Internal", "Word Clock", "ADAT", "SPDIF", "AES/EBU"];
pub const SAMPLE_RATES: [u32; 6] = [44100, 48000, 88200, 96000, 176400, 192000];

pub fn rig_model(key: &str) -> Option<&'static str> {
    RIG_IFACES.iter().find(|r| r.0 == key).map(|r| r.1)
}

pub fn bank_label(bank: &str) -> &str {
    match bank {
        "AES" => "AES/EBU",
        "SPDIF" => "S/PDIF",
        b => b,
    }
}

/// Channels carried by a bank at the current rate family (analog/ADAT/TDIF halve at 2x, quarter at 4x).
pub fn bank_count(bank: &str, n: usize, rate: u32) -> usize {
    let family = if rate > 96000 { 4 } else if rate > 48000 { 2 } else { 1 };
    if family > 1 && matches!(bank, "Analog" | "ADAT" | "TDIF") && n > 0 {
        ((n / family) & !1).max(2)
    } else {
        n
    }
}

/// A token "A Analog 00" → ("A", "Analog", 0).
pub fn tok_parts(tok: &str) -> (String, String, usize) {
    let parts: Vec<&str> = tok.split_whitespace().collect();
    match parts.as_slice() {
        [s, b, i] => (s.to_string(), b.to_string(), i.parse().unwrap_or(0)),
        [b, i] if b.len() > 1 => (String::new(), b.to_string(), i.parse().unwrap_or(0)),
        [i] => (String::new(), String::new(), i.parse().unwrap_or(0)),
        _ => (String::new(), String::new(), 0),
    }
}

pub fn chan_name(bank: &str, idx: usize) -> String {
    match bank {
        "" => format!("Ch {}", idx + 1),
        "Main" | "Phones" => format!("{} {}", bank, if idx % 2 == 0 { "L" } else { "R" }),
        b => format!("{} {}", bank_label(b), idx + 1),
    }
}

fn new_input(token: String, group: String, name: String, kind: Kind, trim_range: (f32, f32)) -> Input {
    Input {
        token,
        group,
        name,
        kind,
        trim: trim_range.0.max(0.0).min(trim_range.1),
        trim_range,
        pad: false,
        phase: false,
        stereo: false,
        mute: false,
        ms: false,
        width: 1.0,
        swap: false,
        ref_plus4: true,
        vlimit: false,
        lookahead: false,
        softclip: false,
        rev_send: 0.0,
        talk: false,
        listen: false,
        eq: Eq::default(),
        dynamics: Dyn::default(),
    }
}

fn new_output(token: String, group: String, name: String) -> Output {
    Output {
        token,
        group,
        name,
        fader: FADER_UNITY,
        mute: false,
        stereo: false,
        rev_send: 0.0,
        rev_return: 0.0,
        monitor: false,
        talk: false,
        listen: false,
        eq: Eq::default(),
        dynamics: Dyn::default(),
    }
}

fn new_mixes(n_inputs: usize, count: usize) -> Vec<Mix> {
    (0..count)
        .map(|m| Mix {
            name: format!("Mix {}", m + 1),
            sends: (0..n_inputs)
                .map(|i| Send {
                    fader: if m == 0 { FADER_UNITY } else { 0.0 },
                    pan: if i % 2 == 0 { -1.0 } else { 1.0 },
                    mute: false,
                    solo: false,
                })
                .collect(),
            master: FADER_UNITY,
            mute: false,
            output: usize::MAX,
            rev_send: 0.0,
            rev_return: 0.0,
        })
        .collect()
}

impl State {
    /// PCI-424 with the given AudioWire rig (the person's driver contract).
    pub fn pci424(rig: [String; 4], rate: u32) -> Self {
        let mut inputs = vec![];
        let mut outputs = vec![];
        for (s, key) in rig.iter().enumerate() {
            let slot = (b'A' + s as u8) as char;
            let Some(&(_, model, banks)) = RIG_IFACES.iter().find(|r| r.0 == key) else { continue };
            for &(bank, n_in, n_out) in banks {
                let group = format!("{} · {} — {}", slot, model, bank_label(bank));
                let kind = if bank == "Analog" { Kind::Line } else { Kind::Digital };
                for i in 0..bank_count(bank, n_in, rate) {
                    let tok = format!("{} {} {:02}", slot, bank, i);
                    inputs.push(new_input(tok, group.clone(), chan_name(bank, i), kind, PCI_TRIM));
                }
                for i in 0..bank_count(bank, n_out, rate) {
                    let tok = format!("{} {} {:02}", slot, bank, i);
                    outputs.push(new_output(tok, group.clone(), chan_name(bank, i)));
                }
            }
        }
        // stereo pairs as the driver's demo ships them: AES/Main banks and A Analog 1-2
        for v in inputs.iter_mut() {
            let (s, b, i) = tok_parts(&v.token);
            v.stereo = i % 2 == 0 && (b == "AES" || (s == "A" && b == "Analog" && i < 2));
        }
        for v in outputs.iter_mut() {
            let (s, b, i) = tok_parts(&v.token);
            v.stereo = i % 2 == 0 && (b == "AES" || b == "Main" || (s == "A" && b == "Analog" && i < 2));
        }
        let n = inputs.len();
        let mut s = State {
            device: "PCI-424 (simulated)".into(),
            family: Family::Pci424,
            rig,
            inputs,
            mixes: new_mixes(n, MIX_COUNT),
            outputs,
            reverb: Reverb::default(),
            monitor_level: 0.6,
            talk: false,
            listen: false,
            talk_atten: -20.0,
            clock_source: "Internal".into(),
            sample_rate: rate,
            meters: true,
            patchbay: false,
        };
        s.route_default();
        s
    }

    pub fn default_rig() -> [String; 4] {
        ["24io".into(), "1224".into(), String::new(), String::new()]
    }

    /// 828mk3-class CueMix FX device.
    pub fn mk3() -> Self {
        let mut inputs = vec![];
        for i in 0..2 {
            inputs.push(new_input(format!("Mic {:02}", i), "Mic / Guitar".into(), format!("Mic {}", i + 1), Kind::Mic, MIC_TRIM));
        }
        for i in 0..8 {
            inputs.push(new_input(format!("Analog {:02}", i), "Analog".into(), format!("Analog {}", i + 1), Kind::Line, LINE_TRIM));
        }
        for i in 0..2 {
            inputs.push(new_input(format!("SPDIF {:02}", i), "S/PDIF".into(), format!("S/PDIF {}", i + 1), Kind::Digital, LINE_TRIM));
        }
        for i in 0..8 {
            inputs.push(new_input(format!("ADAT {:02}", i), "ADAT A".into(), format!("ADAT {}", i + 1), Kind::Digital, LINE_TRIM));
        }
        for v in inputs.iter_mut() {
            if v.kind != Kind::Mic {
                v.trim = 0.0;
            }
        }
        let mut outputs = vec![];
        let banks: [(&str, &str, usize); 5] =
            [("Main", "Main", 2), ("Analog", "Analog", 8), ("Phones", "Phones", 2), ("SPDIF", "S/PDIF", 2), ("ADAT", "ADAT A", 8)];
        for (bank, group, n) in banks {
            for i in 0..n {
                outputs.push(new_output(format!("{} {:02}", bank, i), group.into(), chan_name(bank, i)));
            }
        }
        for (k, o) in outputs.iter_mut().enumerate() {
            o.stereo = k % 2 == 0;
        }
        let n = inputs.len();
        let mut s = State {
            device: "828mk3 (simulated)".into(),
            family: Family::Mk3,
            rig: Default::default(),
            inputs,
            mixes: new_mixes(n, MIX_COUNT),
            outputs,
            reverb: Reverb::default(),
            monitor_level: 0.6,
            talk: false,
            listen: false,
            talk_atten: -20.0,
            clock_source: "Internal".into(),
            sample_rate: 48000,
            meters: true,
            patchbay: true,
        };
        s.outputs[0].monitor = true;
        s.route_default();
        s
    }

    /// Mix 1 → the main pair (or the first pair), Mix 2 → phones / the next pair.
    fn route_default(&mut self) {
        let main = self.outputs.iter().position(|o| o.token.contains("Main")).unwrap_or(0);
        let second = self
            .outputs
            .iter()
            .position(|o| o.token.contains("Phones"))
            .unwrap_or(if main == 0 { 2 } else { 0 });
        if !self.outputs.is_empty() {
            self.mixes[0].output = main;
        }
        if self.outputs.len() > second + 1 && self.mixes.len() > 1 {
            self.mixes[1].output = second;
        }
    }

    /// A lived-in starting mix so the console isn't flat on first launch.
    pub fn with_demo_mix(mut self) -> Self {
        let n = self.inputs.len();
        let faders = [0.92, 0.85, 1.0, 1.0, 0.78, 0.78, 0.66, 0.70, 0.0, 0.0, 0.55, 0.55, 0.8, 0.74, 0.6, 0.62];
        let pans = [-1.0, 1.0, -0.2, 0.2, -0.4, 0.4, -0.7, 0.7, 0.0, 0.0, -1.0, 1.0, -0.3, 0.3, 0.5, -0.5];
        for i in 0..n {
            let f = faders[i % faders.len()];
            let p = pans[i % pans.len()];
            self.mixes[0].sends[i].fader = f;
            self.mixes[0].sends[i].pan = p;
            if self.mixes.len() > 1 {
                self.mixes[1].sends[i].fader = f * 0.8;
                self.mixes[1].sends[i].pan = p;
            }
        }
        self.mixes[0].name = "Main".into();
        if self.mixes.len() > 1 {
            self.mixes[1].name = "Phones".into();
        }
        for (i, v) in self.inputs.iter_mut().enumerate() {
            if v.kind == Kind::Mic {
                v.trim = 30.0 - i as f32 * 2.0;
            } else if self.family == Family::Pci424 {
                v.trim = ((i * 7) % 18) as f32;
            }
        }
        if n > 9 {
            self.mixes[0].sends[9].mute = true;
        }
        self.patchbay = true;
        if self.family == Family::Mk3 {
            let a = &mut self.inputs[0];
            a.dynamics.on = true;
            a.eq.on = true;
            a.eq.hpf_on = true;
            a.eq.hpf_freq = 80.0;
            a.eq.bands[1].gain = -3.0;
            a.eq.bands[3].gain = 4.0;
            a.eq.bands[3].q = 0.8;
            a.rev_send = 0.35;
            self.mixes[0].rev_return = 0.6;
        }
        self
    }

    pub fn has_effects(&self) -> bool {
        self.family == Family::Mk3
    }

    /// Output pairs a mix can be assigned to (index of the left channel).
    pub fn output_pairs(&self) -> Vec<usize> {
        (0..self.outputs.len().saturating_sub(1))
            .filter(|&k| {
                let (s0, b0, i0) = tok_parts(&self.outputs[k].token);
                let (s1, b1, _) = tok_parts(&self.outputs[k + 1].token);
                i0 % 2 == 0 && s0 == s1 && b0 == b1
            })
            .collect()
    }

    pub fn pair_name(&self, k: usize) -> String {
        if k >= self.outputs.len() {
            return "—".into();
        }
        let o = &self.outputs[k];
        let (s, b, i) = tok_parts(&o.token);
        let slot = if s.is_empty() { String::new() } else { format!("{} ", s) };
        match b.as_str() {
            "Main" | "Phones" => format!("{}{}", slot, b),
            _ => format!("{}{} {}-{}", slot, bank_label(&b), i + 1, i + 2),
        }
    }
}
