// SPDX-License-Identifier: GPL-2.0-or-later
//! Numbers from MECHANICS.md / mechanics.json, used as written there.

// Fader: gain = position², readout 20·log10(gain), "-inf" below -90 dB.
// Value range 0…1 set by the base value setup (0x472b20), not widened by the
// fader constructor (0x44efd0): the fader tops out at gain 1 = 0 dB.
pub const FADER_TOP: f32 = 1.0;
pub const FADER_UNITY: f32 = 1.0; // 0 dB, hardware word 32768
#[allow(dead_code)]
pub const FADER_HW_UNITY: f32 = 32768.0;
pub const DB_FLOOR: f32 = -90.0;

// Pan −1 … +1, 128 hardware steps.
pub const PAN_STEPS: f32 = 128.0;

// Trim hardware word: 64 + (x−min)·191/(max−min) → 64…255 (function 0x44f1d0).
pub const TRIM_HW_LOW: f32 = 64.0;
pub const TRIM_HW_SPAN: f32 = 191.0;

pub fn trim_hw_word(x: f32, min: f32, max: f32) -> u32 {
    if max <= min {
        return TRIM_HW_LOW as u32;
    }
    (TRIM_HW_LOW + (x - min) * TRIM_HW_SPAN / (max - min)).round() as u32
}

// Trim (estimated, the device reports it).
pub const MIC_TRIM: (f32, f32) = (0.0, 53.0);
pub const LINE_TRIM: (f32, f32) = (-96.0, 22.0);
// PCI-424 rig trim, from the person's motu424 driver contract (demo range 0..60).
pub const PCI_TRIM: (f32, f32) = (0.0, 60.0);

// PCI-424 hardware link (motu424-ctl kcontrols): volume steps 0..127, vendor default 100.
// The top step is taken as the fader top (0 dB), linear in fader position — estimated.
pub const HW_VOL_MAX: f32 = 127.0;
pub const HW_PAN_RANGE: f32 = 100.0;

// EQ
pub const EQ_FREQ: (f32, f32) = (20.0, 20000.0);
pub const EQ_GAIN: (f32, f32) = (-20.0, 20.0);
pub const EQ_Q: (f32, f32) = (0.01, 3.0);

// Dynamics
pub const COMP_THRESHOLD: (f32, f32) = (-48.0, 0.0);
pub const COMP_RATIO: (f32, f32) = (1.0, 10.0);
pub const COMP_ATTACK: (f32, f32) = (10.0, 100.0);
pub const COMP_RELEASE: (f32, f32) = (10.0, 2000.0);
pub const COMP_TRIM: (f32, f32) = (-6.0, 0.0);
pub const LEVELER: (f32, f32) = (0.0, 100.0);

// Reverb
pub const REV_PREDELAY: (f32, f32) = (0.0, 100.0);
pub const REV_SHELF_FREQ: (f32, f32) = (1000.0, 20000.0);
pub const REV_SHELF_FREQ_DEFAULT: f32 = 16000.0;
pub const REV_SHELF_CUT: (f32, f32) = (-40.0, 0.0);
pub const REV_TIME: (f32, f32) = (100.0, 60000.0);
pub const REV_TIME_CENTRE: f32 = 2000.0;
pub const REV_REFL_SIZE: (f32, f32) = (50.0, 400.0);
pub const REV_REFL_SIZE_DEFAULT: f32 = 100.0;
pub const REV_TIME_PCT: (f32, f32) = (0.0, 100.0);
pub const REV_TIME_LOW_DEFAULT: f32 = 100.0;
pub const REV_TIME_MID_DEFAULT: f32 = 50.0;
pub const REV_TIME_HIGH_DEFAULT: f32 = 25.0;
pub const REV_XOVER_LOW: (f32, f32) = (100.0, 20000.0);
pub const REV_XOVER_LOW_DEFAULT: f32 = 500.0;
pub const REV_XOVER_HIGH: (f32, f32) = (1000.0, 20000.0);
pub const REV_XOVER_HIGH_DEFAULT: f32 = 2000.0;
pub const REV_WIDTH: (f32, f32) = (-1.0, 1.0);

// Meters
pub const METER_SPLITS: [f32; 4] = [0.25, 0.5, 0.75, 0.95];
pub const PEAK_HOLD: [(&str, f32); 7] = [
    ("Off", 0.0),
    ("2 Seconds", 2.0),
    ("4 Seconds", 4.0),
    ("10 Seconds", 10.0),
    ("1 Minute", 60.0),
    ("5 Minutes", 300.0),
    ("Infinite", f32::INFINITY),
];

pub const MIX_COUNT: usize = 8; // estimated

pub fn gain_from_pos(p: f32) -> f32 {
    p * p
}

pub fn db_from_gain(g: f32) -> f32 {
    if g <= 0.0 {
        f32::NEG_INFINITY
    } else {
        20.0 * g.log10()
    }
}

pub fn fader_db(p: f32) -> f32 {
    db_from_gain(gain_from_pos(p))
}

/// Readout of 0x422fa0: "%0.1f", or "inf"/"-inf" when not finite.
pub fn fmt_db(db: f32) -> String {
    // the OSC readout (0x5a7250) also floors at -90 dB; the console follows it
    if !db.is_finite() || db < DB_FLOOR {
        if db < 0.0 { "-inf dB".into() } else { "inf dB".into() }
    } else {
        format!("{:.1} dB", db)
    }
}

/// Fader readout of 0x4770c0: 20·log10(gain) through 0x422fa0, then " dB".
pub fn fmt_fader(p: f32) -> String {
    fmt_db(fader_db(p))
}

/// Pan readout of 0x475650: "%0.2f" of the −1 … +1 value.
pub fn fmt_pan(pan: f32) -> String {
    format!("{:.2}", pan)
}

/// Pan hardware word: (pan + 1) · 128 · 0.5 → 0 … 128 (0x475280).
pub fn pan_hw_word(pan: f32) -> u32 {
    ((pan + 1.0) * PAN_STEPS * 0.5).round() as u32
}

pub fn fmt_freq(f: f32) -> String {
    if f < 1000.0 {
        format!("{:.1} Hz", f)
    } else {
        format!("{:.2} kHz", f / 1000.0)
    }
}

pub fn fmt_ms(ms: f32) -> String {
    if ms < 1000.0 {
        format!("{:.0} ms", ms)
    } else {
        format!("{:.2} sec", ms / 1000.0)
    }
}
