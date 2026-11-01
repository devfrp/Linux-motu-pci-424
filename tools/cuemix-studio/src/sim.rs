// SPDX-License-Identifier: GPL-2.0-or-later
//! Simulated 828mk3: generated test signals instead of the MOTU driver,
//! run through trim, dynamics, faders, pan and mix buses to drive the meters.

use crate::mech::*;
use crate::model::*;

#[derive(Clone, Default)]
pub struct Meter {
    pub level: f32, // 0..1 of the meter scale
    pub peak: f32,
    pub peak_age: f32,
    pub clip: bool,
}

/// Meter scale: −60 dB … 0 dBFS (estimated) mapped to 0 … 1.
pub fn meter_pos(db: f32) -> f32 {
    ((db + 60.0) / 60.0).clamp(0.0, 1.0)
}

impl Meter {
    pub fn feed(&mut self, db: f32, dt: f32, hold: f32) {
        let v = meter_pos(db);
        // fast attack, smooth fall
        self.level = if v > self.level { v } else { (self.level - dt * 0.9).max(v) };
        if db >= 0.0 {
            self.clip = true;
        }
        if self.level >= self.peak {
            self.peak = self.level;
            self.peak_age = 0.0;
        } else {
            self.peak_age += dt;
            if hold <= 0.0 {
                self.peak = self.level;
            } else if self.peak_age > hold {
                self.peak = (self.peak - dt * 0.6).max(self.level);
            }
        }
    }
    pub fn clear(&mut self) {
        self.peak = self.level;
        self.clip = false;
    }
}

pub struct Sim {
    pub t: f32,
    pub in_pre: Vec<Meter>,
    pub in_post: Vec<Meter>,
    pub in_gr: Vec<f32>,
    pub out_l: Vec<Meter>,
    pub out_r: Vec<Meter>,
    pub out_gr: Vec<f32>,
    pub mix_l: Vec<Meter>,
    pub mix_r: Vec<Meter>,
    pub send_meters: Vec<Meter>, // per input on the current mix
    pub hold_idx: usize,
    seed: u32,
}

impl Sim {
    pub fn new(state: &State) -> Self {
        let n = state.inputs.len();
        let o = state.outputs.len();
        Sim {
            t: 0.0,
            in_pre: vec![Meter::default(); n],
            in_post: vec![Meter::default(); n],
            in_gr: vec![0.0; n],
            out_l: vec![Meter::default(); o],
            out_r: vec![Meter::default(); o],
            out_gr: vec![0.0; o],
            mix_l: vec![Meter::default(); state.mixes.len()],
            mix_r: vec![Meter::default(); state.mixes.len()],
            send_meters: vec![Meter::default(); n],
            hold_idx: 3,
            seed: 12345,
        }
    }

    fn noise(&mut self) -> f32 {
        self.seed ^= self.seed << 13;
        self.seed ^= self.seed >> 17;
        self.seed ^= self.seed << 5;
        (self.seed as f32 / u32::MAX as f32) * 2.0 - 1.0
    }

    /// Raw source level (dBFS before trim) of simulated input `i`.
    fn source_db(&mut self, i: usize, kind: Kind, n_inputs: usize) -> f32 {
        let t = self.t;
        let fi = i as f32;
        let n = self.noise() * 1.5;
        // the last third of the inputs is left unplugged, like a real rig
        if i * 3 >= n_inputs * 2 && n_inputs > 6 {
            return -140.0;
        }
        match kind {
            // mic level sources sit low and need trim
            Kind::Mic => -48.0 + 9.0 * (t * (1.3 + fi * 0.4)).sin().abs() + n,
            Kind::Line => -30.0 + 14.0 * ((t * (0.7 + fi * 0.37)).sin() * (t * 3.1 + fi).cos()).abs() + n,
            Kind::Digital => -18.0 + 6.0 * (t * 0.5 + fi).sin() + n,
        }
    }

    fn comp_gr(d: &Dyn, db: f32) -> f32 {
        if !d.on {
            return 0.0;
        }
        let mut gr = 0.0;
        if d.comp_on && db > d.threshold {
            gr += (db - d.threshold) * (1.0 - 1.0 / d.ratio);
        }
        if d.lev_on {
            gr += d.lev_reduction / LEVELER.1 * 12.0 * meter_pos(db);
        }
        gr
    }

    pub fn step(&mut self, s: &State, current_mix: usize, dt: f32) {
        self.t += dt;
        let hold = PEAK_HOLD[self.hold_idx].1;
        let n = s.inputs.len();
        let mut post_amp = vec![0.0f32; n];
        for i in 0..n {
            let inp = &s.inputs[i];
            let mut db = self.source_db(i, inp.kind, n);
            db += inp.trim;
            if inp.pad {
                db -= 20.0;
            }
            if !inp.ref_plus4 {
                db -= 11.8;
            }
            self.in_pre[i].feed(db, dt, hold);
            let gr = Self::comp_gr(&inp.dynamics, db);
            self.in_gr[i] = gr;
            let mut post = db - gr;
            if inp.mute {
                post = -150.0;
            }
            if inp.dynamics.on {
                post += inp.dynamics.trim + inp.dynamics.lev_makeup / LEVELER.1 * 12.0;
            }
            if inp.eq.on {
                let boost: f32 = inp.eq.bands.iter().filter(|b| b.on).map(|b| b.gain * 0.15).sum();
                post += boost;
            }
            self.in_post[i].feed(post, dt, hold);
            post_amp[i] = 10f32.powf(post / 20.0);
        }

        let mut out_amp = vec![(0.0f32, 0.0f32); s.outputs.len()];
        for (m, mix) in s.mixes.iter().enumerate() {
            let any_solo = mix.sends.iter().any(|x| x.solo);
            let (mut l, mut r) = (0.0f32, 0.0f32);
            for (i, send) in mix.sends.iter().enumerate() {
                let audible = !send.mute && (!any_solo || send.solo);
                let g = if audible { gain_from_pos(send.fader) } else { 0.0 };
                let a = post_amp[i] * g;
                if m == current_mix {
                    self.send_meters[i].feed(db_from_gain(a), dt, hold);
                }
                // equal-power pan (estimated)
                let th = (send.pan + 1.0) * std::f32::consts::FRAC_PI_4;
                l += (a * th.cos()).powi(2);
                r += (a * th.sin()).powi(2);
            }
            let mg = if mix.mute { 0.0 } else { gain_from_pos(mix.master) };
            let rev = if s.reverb.on { gain_from_pos(mix.rev_return) * 0.3 } else { 0.0 };
            let (l, r) = (l.sqrt() * mg * (1.0 + rev), r.sqrt() * mg * (1.0 + rev));
            self.mix_l[m].feed(db_from_gain(l), dt, hold);
            self.mix_r[m].feed(db_from_gain(r), dt, hold);
            if s.patchbay && mix.output < out_amp.len().saturating_sub(1) {
                out_amp[mix.output].0 += l;
                out_amp[mix.output + 1].0 += r;
            }
        }
        // with the patchbay bypassed, Main outs carry the computer's stereo program
        if !s.patchbay {
            let prog = 10f32.powf((-20.0 + 4.0 * (self.t * 0.9).sin()) / 20.0);
            for (o, out) in s.outputs.iter().enumerate() {
                if out.token.contains("Main") {
                    out_amp[o].0 += prog;
                }
            }
        }
        let talk_dim = if s.talk || s.listen { 10f32.powf(s.talk_atten / 20.0) } else { 1.0 };
        for (o, out) in s.outputs.iter().enumerate() {
            let g = if out.mute { 0.0 } else { gain_from_pos(out.fader) } * talk_dim;
            let mut l = db_from_gain(out_amp[o].0 * g);
            let gr = Self::comp_gr(&out.dynamics, l);
            self.out_gr[o] = gr;
            l -= gr;
            self.out_l[o].feed(l, dt, hold);
        }
    }

    pub fn clear_peaks(&mut self) {
        for m in self
            .in_pre
            .iter_mut()
            .chain(self.in_post.iter_mut())
            .chain(self.out_l.iter_mut())
            .chain(self.out_r.iter_mut())
            .chain(self.mix_l.iter_mut())
            .chain(self.mix_r.iter_mut())
            .chain(self.send_meters.iter_mut())
        {
            m.clear();
        }
    }
}
