// SPDX-License-Identifier: GPL-2.0-or-later
//! EQ response for the graph: standard biquad magnitude at 48 kHz.

use crate::model::{BandType, Eq};
use std::f32::consts::PI;

const FS: f32 = 48000.0;

fn mag(b: [f32; 3], a: [f32; 3], f: f32) -> f32 {
    let w = 2.0 * PI * f / FS;
    let (c1, s1, c2, s2) = (w.cos(), w.sin(), (2.0 * w).cos(), (2.0 * w).sin());
    let nr = b[0] + b[1] * c1 + b[2] * c2;
    let ni = -(b[1] * s1 + b[2] * s2);
    let dr = a[0] + a[1] * c1 + a[2] * c2;
    let di = -(a[1] * s1 + a[2] * s2);
    ((nr * nr + ni * ni) / (dr * dr + di * di)).sqrt()
}

fn peak(f0: f32, gain: f32, q: f32) -> ([f32; 3], [f32; 3]) {
    let a = 10f32.powf(gain / 40.0);
    let w0 = 2.0 * PI * f0.min(FS * 0.49) / FS;
    let alpha = w0.sin() / (2.0 * q.max(0.05));
    (
        [1.0 + alpha * a, -2.0 * w0.cos(), 1.0 - alpha * a],
        [1.0 + alpha / a, -2.0 * w0.cos(), 1.0 - alpha / a],
    )
}

fn shelf(f0: f32, gain: f32, high: bool) -> ([f32; 3], [f32; 3]) {
    let a = 10f32.powf(gain / 40.0);
    let w0 = 2.0 * PI * f0.min(FS * 0.49) / FS;
    let (cw, sw) = (w0.cos(), w0.sin());
    let alpha = sw / 2.0 * 2f32.sqrt();
    let sa = 2.0 * a.sqrt() * alpha;
    if high {
        (
            [a * ((a + 1.0) + (a - 1.0) * cw + sa), -2.0 * a * ((a - 1.0) + (a + 1.0) * cw), a * ((a + 1.0) + (a - 1.0) * cw - sa)],
            [(a + 1.0) - (a - 1.0) * cw + sa, 2.0 * ((a - 1.0) - (a + 1.0) * cw), (a + 1.0) - (a - 1.0) * cw - sa],
        )
    } else {
        (
            [a * ((a + 1.0) - (a - 1.0) * cw + sa), 2.0 * a * ((a - 1.0) - (a + 1.0) * cw), a * ((a + 1.0) - (a - 1.0) * cw - sa)],
            [(a + 1.0) + (a - 1.0) * cw + sa, -2.0 * ((a - 1.0) + (a + 1.0) * cw), (a + 1.0) + (a - 1.0) * cw - sa],
        )
    }
}

fn pass(f0: f32, high: bool) -> ([f32; 3], [f32; 3]) {
    let w0 = 2.0 * PI * f0.min(FS * 0.49) / FS;
    let (cw, sw) = (w0.cos(), w0.sin());
    let alpha = sw / 2.0 * 2f32.sqrt();
    if high {
        ([(1.0 + cw) / 2.0, -(1.0 + cw), (1.0 + cw) / 2.0], [1.0 + alpha, -2.0 * cw, 1.0 - alpha])
    } else {
        ([(1.0 - cw) / 2.0, 1.0 - cw, (1.0 - cw) / 2.0], [1.0 + alpha, -2.0 * cw, 1.0 - alpha])
    }
}

/// Response in dB at frequency `f`.
pub fn response_db(eq: &Eq, f: f32) -> f32 {
    let mut g = 1.0f32;
    for (i, b) in eq.bands.iter().enumerate() {
        if !b.on {
            continue;
        }
        let (nb, na) = match b.kind {
            BandType::Shelf => shelf(b.freq, b.gain, i == 4),
            // Type II is the steeper (narrower) bell
            BandType::Two => peak(b.freq, b.gain, b.q * 1.6),
            BandType::One => peak(b.freq, b.gain, b.q),
        };
        g *= mag(nb, na, f);
    }
    if eq.hpf_on {
        let (b, a) = pass(eq.hpf_freq, true);
        let m = mag(b, a, f);
        g *= if eq.hpf_slope12 { m } else { m.sqrt() };
    }
    if eq.lpf_on {
        let (b, a) = pass(eq.lpf_freq, false);
        let m = mag(b, a, f);
        g *= if eq.lpf_slope12 { m } else { m.sqrt() };
    }
    20.0 * g.max(1e-6).log10()
}
