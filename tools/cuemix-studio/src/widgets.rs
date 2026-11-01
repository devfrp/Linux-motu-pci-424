// SPDX-License-Identifier: GPL-2.0-or-later
use crate::mech::*;
use crate::sim::Meter;
use egui::{pos2, vec2, Align2, Color32, FontId, Rect, Response, Sense, Shape, Stroke, Ui};

pub const BG: Color32 = Color32::from_rgb(16, 16, 20);
pub const PANEL: Color32 = Color32::from_rgb(30, 30, 36);
pub const PANEL_HI: Color32 = Color32::from_rgb(44, 44, 52);
pub const WELL: Color32 = Color32::from_rgb(10, 10, 12);
pub const TEXT: Color32 = Color32::from_rgb(214, 214, 222);
pub const DIM: Color32 = Color32::from_rgb(130, 130, 142);
pub const ACCENT: Color32 = Color32::from_rgb(25, 195, 176);
pub const MIXCOL: Color32 = Color32::from_rgb(78, 52, 88);
pub const SOLO: Color32 = Color32::from_rgb(232, 196, 40);
pub const MUTE: Color32 = Color32::from_rgb(214, 64, 52);
pub const BLUE: Color32 = Color32::from_rgb(90, 160, 240);

/// How a knob maps its value to its travel.
#[derive(Clone, Copy)]
pub enum Curve {
    Lin,
    Log,
}

pub fn to_norm(v: f32, min: f32, max: f32, c: Curve) -> f32 {
    match c {
        Curve::Lin => (v - min) / (max - min),
        Curve::Log => (v.max(1e-6).ln() - min.ln()) / (max.ln() - min.ln()),
    }
    .clamp(0.0, 1.0)
}

pub fn from_norm(n: f32, min: f32, max: f32, c: Curve) -> f32 {
    let n = n.clamp(0.0, 1.0);
    match c {
        Curve::Lin => min + n * (max - min),
        Curve::Log => (min.ln() + n * (max.ln() - min.ln())).exp(),
    }
}

/// Rotary knob: drag up/down, double-click resets.
#[allow(clippy::too_many_arguments)]
pub fn knob(
    ui: &mut Ui,
    value: &mut f32,
    min: f32,
    max: f32,
    default: f32,
    curve: Curve,
    label: &str,
    readout: String,
    size: f32,
    color: Color32,
) -> Response {
    let (rect, mut resp) = ui.allocate_exact_size(vec2(size + 12.0, size + 30.0), Sense::click_and_drag());
    let mut n = to_norm(*value, min, max, curve);
    if resp.dragged() {
        let fine = if ui.input(|i| i.modifiers.shift) { 0.2 } else { 1.0 };
        n = (n - resp.drag_delta().y * 0.005 * fine + resp.drag_delta().x * 0.002 * fine).clamp(0.0, 1.0);
        *value = from_norm(n, min, max, curve);
        resp.mark_changed();
    }
    if resp.double_clicked() {
        *value = default;
        n = to_norm(*value, min, max, curve);
        resp.mark_changed();
    }
    let p = ui.painter();
    let c = pos2(rect.center().x, rect.top() + 12.0 + size / 2.0);
    let r = size / 2.0;
    p.text(pos2(rect.center().x, rect.top()), Align2::CENTER_TOP, label, FontId::proportional(10.0), DIM);
    let a0 = std::f32::consts::PI * 0.75;
    let sweep = std::f32::consts::PI * 1.5;
    let arc = |from: f32, to: f32, rad: f32| -> Vec<egui::Pos2> {
        let steps = 32;
        (0..=steps)
            .map(|k| {
                let a = from + (to - from) * k as f32 / steps as f32;
                pos2(c.x + rad * a.cos(), c.y + rad * a.sin())
            })
            .collect()
    };
    p.circle_filled(c, r, WELL);
    p.add(Shape::line(arc(a0, a0 + sweep, r - 3.0), Stroke::new(4.0_f32, PANEL_HI)));
    // bipolar knobs light from the centre
    let start = if min < 0.0 && max > 0.0 { to_norm(0.0, min, max, curve) } else { 0.0 };
    let (s0, s1) = if n >= start { (start, n) } else { (n, start) };
    if (s1 - s0) > 0.001 {
        p.add(Shape::line(arc(a0 + sweep * s0, a0 + sweep * s1, r - 3.0), Stroke::new(4.0_f32, color)));
    }
    let a = a0 + sweep * n;
    p.circle_filled(c, r - 8.0, PANEL);
    p.line_segment(
        [pos2(c.x + (r - 14.0) * a.cos(), c.y + (r - 14.0) * a.sin()), pos2(c.x + (r - 6.0) * a.cos(), c.y + (r - 6.0) * a.sin())],
        Stroke::new(2.0_f32, TEXT),
    );
    p.text(pos2(rect.center().x, rect.bottom()), Align2::CENTER_BOTTOM, readout, FontId::monospace(10.0), BLUE);
    resp.on_hover_text("Drag to change, Shift for fine, double-click to reset")
}

/// Vertical fader holding a fader position (gain = position²).
pub fn fader(ui: &mut Ui, pos: &mut f32, height: f32, width: f32, color: Color32) -> Response {
    let (rect, mut resp) = ui.allocate_exact_size(vec2(width, height), Sense::click_and_drag());
    let track = Rect::from_center_size(rect.center(), vec2(6.0, height - 24.0));
    let to_y = |p: f32| track.bottom() - (p / FADER_TOP) * track.height();
    if resp.dragged() {
        let fine = if ui.input(|i| i.modifiers.shift) { 0.2 } else { 1.0 };
        *pos = (*pos - resp.drag_delta().y / track.height() * FADER_TOP * fine).clamp(0.0, FADER_TOP);
        resp.mark_changed();
    }
    if resp.double_clicked() {
        *pos = FADER_UNITY;
        resp.mark_changed();
    }
    let p = ui.painter();
    p.rect_filled(track, 3.0, WELL);
    let fill = Rect::from_min_max(pos2(track.left(), to_y(*pos)), track.max);
    p.rect_filled(fill, 3.0, color.gamma_multiply(0.55));
    // scale marks
    for db in [0.0f32, -6.0, -12.0, -24.0, -48.0] {
        let pp = 10f32.powf(db / 40.0);
        let y = to_y(pp);
        p.line_segment([pos2(track.left() - 6.0, y), pos2(track.left() - 2.0, y)], Stroke::new(1.0_f32, DIM));
        if width > 40.0 {
            p.text(pos2(track.left() - 8.0, y), Align2::RIGHT_CENTER, format!("{}", db as i32), FontId::proportional(8.0), DIM);
        }
    }
    let y = to_y(*pos);
    let cap = Rect::from_center_size(pos2(track.center().x, y), vec2(width.min(34.0) - 4.0, 18.0));
    p.rect_filled(cap, 3.0, Color32::from_rgb(190, 190, 198));
    p.rect_filled(Rect::from_center_size(cap.center(), vec2(cap.width(), 2.0)), 0.0, Color32::from_rgb(40, 40, 46));
    resp.on_hover_text(fmt_fader(*pos))
}

/// LED-style vertical meter split at the layout's 0.25/0.5/0.75/0.95 marks.
pub fn meter(ui: &mut Ui, m: &Meter, size: egui::Vec2) {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    paint_meter(ui.painter(), rect, m);
    let _ = resp;
}

pub fn paint_meter(p: &egui::Painter, rect: Rect, m: &Meter) {
    p.rect_filled(rect, 1.0, WELL);
    let segs = 24;
    let gap = 1.0;
    let h = (rect.height() - 6.0 - gap * segs as f32) / segs as f32;
    for k in 0..segs {
        let f = (k as f32 + 0.5) / segs as f32;
        let col = if f >= METER_SPLITS[3] {
            Color32::from_rgb(230, 50, 40)
        } else if f >= METER_SPLITS[2] {
            Color32::from_rgb(235, 200, 40)
        } else if f >= METER_SPLITS[1] {
            Color32::from_rgb(120, 220, 80)
        } else {
            Color32::from_rgb(40, 190, 90)
        };
        let lit = f <= m.level;
        let pk = (m.peak - f).abs() < 0.5 / segs as f32 && m.peak > 0.02;
        let y1 = rect.bottom() - k as f32 * (h + gap);
        let r = Rect::from_min_max(pos2(rect.left() + 1.0, y1 - h), pos2(rect.right() - 1.0, y1));
        p.rect_filled(r, 0.0, if lit || pk { col } else { col.gamma_multiply(0.12) });
    }
    let clip = Rect::from_min_max(rect.min, pos2(rect.right(), rect.top() + 5.0));
    p.rect_filled(clip, 1.0, if m.clip { MUTE } else { Color32::from_rgb(50, 20, 20) });
}

/// Small latching button with a lit colour.
pub fn toggle(ui: &mut Ui, on: &mut bool, text: &str, color: Color32, w: f32) -> Response {
    let (rect, mut resp) = ui.allocate_exact_size(vec2(w, 18.0), Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let p = ui.painter();
    let fill = if *on { color } else if resp.hovered() { PANEL_HI } else { WELL };
    p.rect_filled(rect, 3.0, fill);
    p.rect_stroke(rect, 3.0, Stroke::new(1.0_f32, if *on { color } else { PANEL_HI }));
    p.text(
        rect.center(),
        Align2::CENTER_CENTER,
        text,
        FontId::proportional(10.0),
        if *on { Color32::BLACK } else { TEXT },
    );
    resp
}

/// Momentary-looking push button.
pub fn push(ui: &mut Ui, text: &str, w: f32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 20.0), Sense::click());
    let p = ui.painter();
    let fill = if resp.is_pointer_button_down_on() { ACCENT } else if resp.hovered() { PANEL_HI } else { WELL };
    p.rect_filled(rect, 3.0, fill);
    p.rect_stroke(rect, 3.0, Stroke::new(1.0_f32, PANEL_HI));
    p.text(rect.center(), Align2::CENTER_CENTER, text, FontId::proportional(11.0), TEXT);
    resp
}

/// Name well at the top/bottom of a strip.
pub fn name_label(ui: &mut Ui, text: &str, w: f32, selected: bool) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 18.0), Sense::click());
    let p = ui.painter();
    p.rect_filled(rect, 2.0, if selected { ACCENT.gamma_multiply(0.35) } else { WELL });
    p.text(rect.center(), Align2::CENTER_CENTER, text, FontId::proportional(10.5), TEXT);
    resp
}

pub fn readout(ui: &mut Ui, text: String, w: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(w, 16.0), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 2.0, WELL);
    p.text(rect.center(), Align2::CENTER_CENTER, text, FontId::monospace(10.0), BLUE);
}

pub fn section(ui: &mut Ui, title: &str, add: impl FnOnce(&mut Ui)) {
    egui::Frame::none()
        .fill(PANEL)
        .rounding(6.0)
        .stroke(Stroke::new(1.0_f32, PANEL_HI))
        .inner_margin(8.0)
        .show(ui, |ui| {
            ui.vertical(|ui| {
                ui.label(egui::RichText::new(title).color(ACCENT).size(12.0).strong());
                ui.add_space(4.0);
                add(ui);
            });
        });
}
