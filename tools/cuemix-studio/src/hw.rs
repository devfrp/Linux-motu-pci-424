// SPDX-License-Identifier: GPL-2.0-or-later
//! Link to a real MOTU PCI-324/424 on Linux through the person's `motu424-ctl`
//! (their driver's ALSA kcontrols, naming per docs/cuemix-control-map.md).
//! A worker thread polls `motu424-ctl list` and flushes coalesced `set`s, so the
//! UI never waits on the tool. Without the tool or a card the console stays simulated.

use crate::mech::*;
use crate::model::*;
use std::collections::HashMap;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq)]
pub struct Ctl {
    pub name: String,
    pub kind: String,
    pub value: String,
    pub min: Option<i64>,
    pub max: Option<i64>,
    pub items: Vec<String>,
}

/// Parse `motu424-ctl list`: `NAME  TYPE = VALUE [min..max]` / `{a|b|c}`.
pub fn parse_list(text: &str) -> Vec<Ctl> {
    let mut out = vec![];
    for line in text.lines() {
        let Some((left, right)) = line.split_once(" = ") else { continue };
        let left = left.trim_end();
        let Some((name, kind)) = left.rsplit_once(char::is_whitespace) else { continue };
        let (name, kind) = (name.trim().to_string(), kind.trim().to_string());
        if !matches!(kind.as_str(), "int" | "bool" | "enum") {
            continue;
        }
        let mut c = Ctl { name, kind: kind.clone(), value: right.trim().to_string(), min: None, max: None, items: vec![] };
        if kind == "int" {
            if let Some((v, rng)) = right.rsplit_once('[') {
                c.value = v.trim().to_string();
                if let Some((a, b)) = rng.trim_end_matches(']').split_once("..") {
                    c.min = a.trim().parse().ok();
                    c.max = b.trim().parse().ok();
                }
            }
        } else if kind == "enum" {
            if let Some((v, items)) = right.rsplit_once('{') {
                c.value = v.trim().to_string();
                c.items = items.trim_end_matches('}').split('|').map(|s| s.to_string()).collect();
            }
        }
        out.push(c);
    }
    out
}

#[derive(Default)]
struct Shared {
    polled: Option<Vec<Ctl>>,
    status: String,
    pending: HashMap<String, String>,
    last_write: HashMap<String, Instant>,
}

pub struct HwLink {
    shared: Arc<Mutex<Shared>>,
    pub tool: Option<String>,
    pub controls: HashMap<String, Ctl>,
    pub connected: bool,
    pub status: String,
    sent: HashMap<String, String>,
}

fn find_tool() -> Option<String> {
    if let Ok(p) = std::env::var("MOTU424_CTL") {
        return Some(p);
    }
    let path = std::env::var("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path) {
        let p = dir.join("motu424-ctl");
        if p.is_file() {
            return Some(p.to_string_lossy().into_owned());
        }
    }
    None
}

impl HwLink {
    pub fn start(enabled: bool) -> Self {
        let tool = if enabled && cfg!(target_os = "linux") { find_tool() } else { None };
        let shared = Arc::new(Mutex::new(Shared::default()));
        let status = match &tool {
            None if !cfg!(target_os = "linux") => "PCI-424 driver is Linux-only — simulated".to_string(),
            None => "motu424-ctl not found — simulated".to_string(),
            Some(_) => "looking for a MOTU card…".to_string(),
        };
        if let Some(t) = tool.clone() {
            let sh = shared.clone();
            std::thread::spawn(move || worker(t, sh));
        }
        HwLink { shared, tool, controls: HashMap::new(), connected: false, status, sent: HashMap::new() }
    }

    /// Pull the latest poll. Returns true when the control *set* changed (rebuild needed).
    pub fn poll(&mut self) -> (bool, bool) {
        let mut sh = self.shared.lock().unwrap();
        if !sh.status.is_empty() {
            self.status = sh.status.clone();
        }
        let Some(list) = sh.polled.take() else { return (false, false) };
        let recent: Vec<String> = sh
            .last_write
            .iter()
            .filter(|(_, t)| t.elapsed() < Duration::from_millis(1500))
            .map(|(k, _)| k.clone())
            .collect();
        drop(sh);
        let has_mixer = list.iter().any(|c| c.name.ends_with("Master Volume") || c.name.starts_with("Input "));
        let new: HashMap<String, Ctl> = list.into_iter().map(|c| (c.name.clone(), c)).collect();
        let set_changed = {
            let mut a: Vec<&String> = new.keys().collect();
            let mut b: Vec<&String> = self.controls.keys().collect();
            a.sort();
            b.sort();
            a != b
        };
        let mut values_changed = false;
        for (k, c) in &new {
            if recent.contains(k) {
                continue;
            }
            if self.controls.get(k).map(|o| &o.value) != Some(&c.value) {
                values_changed = true;
            }
        }
        // keep our just-written values for controls still settling
        let mut merged = new;
        for k in &recent {
            if let (Some(c), Some(v)) = (merged.get_mut(k), self.sent.get(k)) {
                c.value = v.clone();
            }
        }
        self.controls = merged;
        let was = self.connected;
        self.connected = has_mixer;
        if has_mixer {
            self.sent = self.controls.iter().map(|(k, c)| (k.clone(), c.value.clone())).collect();
        }
        (set_changed || was != self.connected, values_changed)
    }

    fn set(&mut self, name: &str, value: String) {
        if !self.controls.contains_key(name) {
            return;
        }
        if self.sent.get(name) == Some(&value) {
            return;
        }
        self.sent.insert(name.to_string(), value.clone());
        if let Some(c) = self.controls.get_mut(name) {
            c.value = value.clone();
        }
        let mut sh = self.shared.lock().unwrap();
        sh.pending.insert(name.to_string(), value);
        sh.last_write.insert(name.to_string(), Instant::now());
    }

    fn int(&self, name: &str) -> Option<i64> {
        self.controls.get(name).and_then(|c| c.value.parse().ok())
    }
    fn bool(&self, name: &str) -> Option<bool> {
        self.controls.get(name).map(|c| c.value == "on")
    }
    fn enm(&self, name: &str) -> Option<String> {
        self.controls.get(name).map(|c| c.value.clone())
    }

    /// Build the console's channel list from the card's control set.
    pub fn build_state(&self, rig_hint: [String; 4]) -> State {
        let mut ins: Vec<String> = vec![];
        let mut outs: Vec<String> = vec![];
        let mut mixes = 0usize;
        let mut names: Vec<&String> = self.controls.keys().collect();
        names.sort();
        for n in names {
            if let Some(t) = n.strip_prefix("Input ").and_then(|r| r.strip_suffix(" Trim Volume")) {
                ins.push(t.to_string());
            }
            if let Some(t) = n.strip_prefix("Output ").and_then(|r| r.strip_suffix(" Volume")) {
                outs.push(t.to_string());
            }
            if let Some(k) = n.strip_prefix("Mix ").and_then(|r| r.strip_suffix(" Master Volume")) {
                if let Ok(k) = k.parse::<usize>() {
                    mixes = mixes.max(k + 1);
                }
            }
        }
        let order = |t: &String| {
            let (s, b, i) = tok_parts(t);
            let bank = ["Analog", "ADAT", "TDIF", "AES", "SPDIF", "Main", "Phones"].iter().position(|x| *x == b).unwrap_or(9);
            (s, bank, i)
        };
        ins.sort_by_key(order);
        outs.sort_by_key(order);
        let mut st = State::pci424(rig_hint, 48000);
        let group = |tok: &str| {
            let (s, b, _) = tok_parts(tok);
            let model = self
                .enm(&format!("Slot {} Interface", s))
                .unwrap_or_default();
            if s.is_empty() {
                "Inputs".to_string()
            } else {
                format!("{} · {} — {}", s, model, bank_label(&b))
            }
        };
        let trim_range = |tok: &str| {
            let c = self.controls.get(&format!("Input {} Trim Volume", tok));
            (c.and_then(|c| c.min).unwrap_or(0) as f32, c.and_then(|c| c.max).unwrap_or(60) as f32)
        };
        let mut template_in = st.inputs.first().cloned();
        st.inputs = ins
            .iter()
            .map(|t| {
                let (_, b, i) = tok_parts(t);
                let mut v = template_in.clone().unwrap_or_else(|| State::mk3().inputs[2].clone());
                v.token = t.clone();
                v.group = group(t);
                v.name = chan_name(&b, i);
                v.kind = if b == "Analog" || b.is_empty() { Kind::Line } else { Kind::Digital };
                v.trim_range = trim_range(t);
                v
            })
            .collect();
        template_in.take();
        let template_out = st.outputs.first().cloned().unwrap_or_else(|| State::mk3().outputs[0].clone());
        st.outputs = outs
            .iter()
            .map(|t| {
                let (_, b, i) = tok_parts(t);
                let mut o = template_out.clone();
                o.token = t.clone();
                o.group = group(t);
                o.name = chan_name(&b, i);
                o
            })
            .collect();
        let n = st.inputs.len();
        let base = st.mixes[0].clone();
        st.mixes = (0..mixes.max(1))
            .map(|m| {
                let mut x = base.clone();
                x.name = format!("Mix {}", m + 1);
                x.sends = (0..n).map(|_| Send { fader: 0.0, pan: 0.0, mute: false, solo: false }).collect();
                x.output = usize::MAX;
                x
            })
            .collect();
        let card = self.enm("Slot A Interface").map(|m| format!(" · {}", m)).unwrap_or_default();
        st.device = format!("PCI-424 (card){}", card);
        self.apply(&mut st);
        st
    }

    /// Card values → console.
    pub fn apply(&self, st: &mut State) {
        let pos = |v: i64| (v as f32 / HW_VOL_MAX * FADER_TOP).clamp(0.0, FADER_TOP);
        for i in 0..st.inputs.len() {
            let p = format!("Input {} ", st.inputs[i].token);
            let v = &mut st.inputs[i];
            if let Some(x) = self.int(&(p.clone() + "Trim Volume")) {
                v.trim = x as f32;
            }
            if let Some(x) = self.bool(&(p.clone() + "Pad Switch")) {
                v.pad = x;
            }
            if let Some(x) = self.bool(&(p.clone() + "Phase Switch")) {
                v.phase = x;
            }
            if let Some(x) = self.bool(&(p.clone() + "Stereo Switch")) {
                v.stereo = x;
            }
            if let Some(x) = self.bool(&(p + "Mute Switch")) {
                v.mute = x;
            }
        }
        for k in 0..st.mixes.len() {
            let mp = format!("Mix {:02} ", k);
            if let Some(x) = self.int(&(mp.clone() + "Master Volume")) {
                st.mixes[k].master = pos(x);
            }
            if let Some(x) = self.bool(&(mp.clone() + "Mute Switch")) {
                st.mixes[k].mute = x;
            }
            for i in 0..st.inputs.len() {
                let sp = format!("{}Input {} ", mp, st.inputs[i].token);
                let s = &mut st.mixes[k].sends[i];
                if let Some(x) = self.int(&(sp.clone() + "Volume")) {
                    s.fader = pos(x);
                }
                if let Some(x) = self.int(&(sp.clone() + "Pan")) {
                    s.pan = (x as f32 / HW_PAN_RANGE).clamp(-1.0, 1.0);
                }
                if let Some(x) = self.bool(&(sp.clone() + "Mute Switch")) {
                    s.mute = x;
                }
                if let Some(x) = self.bool(&(sp + "Solo Switch")) {
                    s.solo = x;
                }
            }
            st.mixes[k].output = usize::MAX;
        }
        for o in 0..st.outputs.len() {
            let op = format!("Output {} ", st.outputs[o].token);
            if let Some(x) = self.int(&(op.clone() + "Volume")) {
                st.outputs[o].fader = pos(x);
            }
            if let Some(x) = self.bool(&(op.clone() + "Mute Switch")) {
                st.outputs[o].mute = x;
            }
            if let Some(x) = self.bool(&(op.clone() + "Stereo Switch")) {
                st.outputs[o].stereo = x;
            }
            if let Some(src) = self.enm(&(op + "Source")) {
                if let Some(rest) = src.strip_prefix("Mix ") {
                    if let Some((k, side)) = rest.split_once(' ') {
                        if let (Ok(k), "L") = (k.parse::<usize>(), side) {
                            if k < st.mixes.len() {
                                st.mixes[k].output = o;
                            }
                        }
                    }
                }
            }
        }
        if let Some(x) = self.enm("Clock Source") {
            st.clock_source = x;
        }
        if let Some(x) = self.enm("Sample Rate").and_then(|x| x.parse().ok()) {
            st.sample_rate = x;
        }
        if let Some(x) = self.bool("Meters") {
            st.meters = x;
        }
        if let Some(x) = self.bool("Patchbay Switch") {
            st.patchbay = x;
        }
        if let Some(x) = self.bool("Talkback Switch") {
            st.talk = x;
        }
        if let Some(x) = self.bool("Listenback Switch") {
            st.listen = x;
        }
        if let Some(x) = self.int("Talkback Atten Volume") {
            st.talk_atten = x as f32;
        }
    }

    /// Console → card: queue every control whose value differs from what the card has.
    pub fn push(&mut self, st: &State) {
        if !self.connected {
            return;
        }
        let onoff = |b: bool| if b { "on".to_string() } else { "off".to_string() };
        let vol = |p: f32| ((p / FADER_TOP * HW_VOL_MAX).round().clamp(0.0, HW_VOL_MAX) as i64).to_string();
        let mut w: Vec<(String, String)> = vec![];
        for v in &st.inputs {
            let p = format!("Input {} ", v.token);
            w.push((p.clone() + "Trim Volume", (v.trim.round() as i64).to_string()));
            w.push((p.clone() + "Pad Switch", onoff(v.pad)));
            w.push((p.clone() + "Phase Switch", onoff(v.phase)));
            w.push((p.clone() + "Stereo Switch", onoff(v.stereo)));
            w.push((p + "Mute Switch", onoff(v.mute)));
        }
        for (k, m) in st.mixes.iter().enumerate() {
            let mp = format!("Mix {:02} ", k);
            w.push((mp.clone() + "Master Volume", vol(m.master)));
            w.push((mp.clone() + "Mute Switch", onoff(m.mute)));
            for (i, s) in m.sends.iter().enumerate() {
                let sp = format!("{}Input {} ", mp, st.inputs[i].token);
                w.push((sp.clone() + "Volume", vol(s.fader)));
                w.push((sp.clone() + "Pan", ((s.pan * HW_PAN_RANGE).round() as i64).to_string()));
                w.push((sp.clone() + "Mute Switch", onoff(s.mute)));
                w.push((sp + "Solo Switch", onoff(s.solo)));
            }
        }
        // routing: a mix assigned to a pair patches its L/R into the pair's Source
        let mut src: Vec<Option<String>> = vec![None; st.outputs.len()];
        for (k, m) in st.mixes.iter().enumerate() {
            if m.output < st.outputs.len().saturating_sub(1) {
                src[m.output] = Some(format!("Mix {:02} L", k));
                src[m.output + 1] = Some(format!("Mix {:02} R", k));
            }
        }
        for (o, out) in st.outputs.iter().enumerate() {
            let op = format!("Output {} ", out.token);
            w.push((op.clone() + "Volume", vol(out.fader)));
            w.push((op.clone() + "Mute Switch", onoff(out.mute)));
            w.push((op.clone() + "Stereo Switch", onoff(out.stereo)));
            let name = op + "Source";
            let cur = self.enm(&name).unwrap_or_default();
            match &src[o] {
                Some(s) => w.push((name, s.clone())),
                // un-assigning a mix puts the output back on its normal
                None if cur.starts_with("Mix ") => {
                    let normal = if out.token.contains("Main") { format!("PCM {:02}", o % 2) } else { "Direct".into() };
                    w.push((name, normal));
                }
                None => {}
            }
        }
        w.push(("Clock Source".into(), st.clock_source.clone()));
        w.push(("Sample Rate".into(), st.sample_rate.to_string()));
        w.push(("Meters".into(), onoff(st.meters)));
        w.push(("Patchbay Switch".into(), onoff(st.patchbay)));
        w.push(("Talkback Switch".into(), onoff(st.talk)));
        w.push(("Listenback Switch".into(), onoff(st.listen)));
        w.push(("Talkback Atten Volume".into(), (st.talk_atten.round() as i64).to_string()));
        for (n, v) in w {
            self.set(&n, v);
        }
    }
}

fn worker(tool: String, sh: Arc<Mutex<Shared>>) {
    let mut last_poll = Instant::now() - Duration::from_secs(10);
    loop {
        let batch: Vec<(String, String)> = {
            let mut s = sh.lock().unwrap();
            s.pending.drain().collect()
        };
        for (name, value) in batch {
            let r = Command::new(&tool).args(["set", &name, &value]).output();
            if let Ok(o) = r {
                if !o.status.success() {
                    let mut s = sh.lock().unwrap();
                    s.status = format!("write failed: {}", String::from_utf8_lossy(&o.stderr).trim());
                }
            }
        }
        if last_poll.elapsed() > Duration::from_millis(1000) {
            last_poll = Instant::now();
            match Command::new(&tool).arg("list").output() {
                Ok(o) => {
                    let text = String::from_utf8_lossy(&o.stdout).to_string();
                    let list = parse_list(&text);
                    let mut s = sh.lock().unwrap();
                    if o.status.success() && !list.is_empty() {
                        let mixer = list.iter().any(|c| c.name.starts_with("Input ") || c.name.ends_with("Master Volume"));
                        s.status = if mixer {
                            format!("connected via motu424-ctl · {} controls", list.len())
                        } else {
                            "card found, driver exposes no mixer controls yet — simulated".into()
                        };
                        s.polled = Some(list);
                    } else {
                        let err = String::from_utf8_lossy(&o.stderr).trim().to_string();
                        s.status = if err.is_empty() { "no MOTU card — simulated".into() } else { format!("{} — simulated", err) };
                        s.polled = Some(vec![]);
                    }
                }
                Err(e) => {
                    sh.lock().unwrap().status = format!("motu424-ctl: {} — simulated", e);
                }
            }
        }
        std::thread::sleep(Duration::from_millis(40));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_list() {
        let t = "Clock Source                     enum   = Internal {Internal|Word Clock|ADAT}\n\
                 Mix 00 Input A Analog 03 Volume  int    = 92 [0..127]\n\
                 Input A Analog 00 Pad Switch     bool   = off\n";
        let l = parse_list(t);
        assert_eq!(l.len(), 3);
        assert_eq!(l[0].items.len(), 3);
        assert_eq!(l[1].name, "Mix 00 Input A Analog 03 Volume");
        assert_eq!(l[1].value, "92");
        assert_eq!(l[1].max, Some(127));
        assert_eq!(l[2].value, "off");
    }
}
