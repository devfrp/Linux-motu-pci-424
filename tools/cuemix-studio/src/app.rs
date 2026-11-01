// SPDX-License-Identifier: GPL-2.0-or-later
use crate::eqcurve;
use crate::hw::HwLink;
use crate::mech::*;
use crate::model::*;
use crate::sim::Sim;
use crate::widgets::*;
use egui::{pos2, vec2, Align2, Color32, FontId, RichText, Stroke};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Mixes,
    Inputs,
    Outputs,
    ChannelDyn,
    Eq,
    Reverb,
    Clock,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DeviceChoice {
    Pci424,
    Mk3,
}

pub struct App {
    pub state: State,
    committed: State,
    undo: Vec<State>,
    redo: Vec<State>,
    pub sim: Sim,
    hw: HwLink,
    pub tab: Tab,
    pub mix: usize,
    sel: usize,
    sel_output: bool,
    clipboard: Option<Mix>,
    configs: BTreeMap<String, State>,
    config_name: String,
    io_path: String,
    show_save: bool,
    show_io: bool,
    toast: Option<(String, f64)>,
    pub frame: u64,
    rename: Option<(usize, String)>,
}

fn config_path() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("cuemix-studio").join("CueMixConfigurations.json")
}

impl App {
    pub fn new(hardware: bool) -> Self {
        let state = State::pci424(State::default_rig(), 48000).with_demo_mix();
        let sim = Sim::new(&state);
        let configs = std::fs::read_to_string(config_path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let home = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).unwrap_or_else(|_| ".".into());
        App {
            committed: state.clone(),
            state,
            undo: vec![],
            redo: vec![],
            sim,
            hw: HwLink::start(hardware),
            tab: Tab::Mixes,
            mix: 0,
            sel: 0,
            sel_output: false,
            clipboard: None,
            configs,
            config_name: "My setup".into(),
            io_path: format!("{}/cuemix-configuration.json", home),
            show_save: false,
            show_io: false,
            toast: None,
            frame: 0,
            rename: None,
        }
    }

    fn set_state(&mut self, s: State) {
        self.undo.push(self.state.clone());
        self.state = s;
        self.committed = self.state.clone();
        self.sim = Sim::new(&self.state);
        self.mix = self.mix.min(self.state.mixes.len().saturating_sub(1));
        self.sel = 0;
        if !self.state.has_effects() && matches!(self.tab, Tab::ChannelDyn | Tab::Eq | Tab::Reverb) {
            self.tab = Tab::Mixes;
        }
        if self.state.has_effects() && self.tab == Tab::Clock {
            self.tab = Tab::Mixes;
        }
    }

    fn save_configs(&mut self) {
        let p = config_path();
        let _ = std::fs::create_dir_all(p.parent().unwrap());
        match serde_json::to_string_pretty(&self.configs) {
            Ok(s) if std::fs::write(&p, &s).is_ok() => {}
            _ => self.notify("Could not write the configurations file"),
        }
    }

    fn notify(&mut self, s: &str) {
        self.toast = Some((s.to_string(), 0.0));
    }

    fn undo(&mut self) {
        if let Some(s) = self.undo.pop() {
            self.redo.push(self.state.clone());
            self.committed = s.clone();
            self.state = s;
            self.sim = Sim::new(&self.state);
        }
    }
    fn redo(&mut self) {
        if let Some(s) = self.redo.pop() {
            self.undo.push(self.state.clone());
            self.committed = s.clone();
            self.state = s;
            self.sim = Sim::new(&self.state);
        }
    }

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        egui::menu::bar(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.button("Save Configuration…").clicked() {
                    self.show_save = true;
                    ui.close_menu();
                }
                ui.menu_button("Load Configuration", |ui| {
                    if self.configs.is_empty() {
                        ui.label(RichText::new("No saved configurations").color(DIM));
                    }
                    let names: Vec<String> = self.configs.keys().cloned().collect();
                    for n in names {
                        if ui.button(&n).clicked() {
                            let s = self.configs[&n].clone();
                            self.set_state(s);
                            self.notify(&format!("Loaded “{}”", n));
                            ui.close_menu();
                        }
                    }
                });
                ui.menu_button("Delete Configuration", |ui| {
                    let names: Vec<String> = self.configs.keys().cloned().collect();
                    for n in names {
                        if ui.button(&n).clicked() {
                            self.configs.remove(&n);
                            self.save_configs();
                            ui.close_menu();
                        }
                    }
                });
                if ui.button("Factory Defaults").clicked() {
                    let s = match self.state.family {
                        Family::Pci424 => State::pci424(self.state.rig.clone(), self.state.sample_rate),
                        Family::Mk3 => State::mk3(),
                    };
                    self.set_state(s);
                    ui.close_menu();
                }
                if ui.button("Import / Export…").clicked() {
                    self.show_io = true;
                    ui.close_menu();
                }
                ui.separator();
                if ui.button("Quit").clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            ui.menu_button("Edit", |ui| {
                if ui.add_enabled(!self.undo.is_empty(), egui::Button::new("Undo")).clicked() {
                    self.undo();
                    ui.close_menu();
                }
                if ui.add_enabled(!self.redo.is_empty(), egui::Button::new("Redo")).clicked() {
                    self.redo();
                    ui.close_menu();
                }
                ui.separator();
                if ui.button("Copy Mix Bus").clicked() {
                    self.clipboard = Some(self.state.mixes[self.mix].clone());
                    ui.close_menu();
                }
                if ui.add_enabled(self.clipboard.is_some(), egui::Button::new("Paste Mix Bus")).clicked() {
                    if let Some(c) = self.clipboard.clone() {
                        let m = &mut self.state.mixes[self.mix];
                        if c.sends.len() == m.sends.len() {
                            m.sends = c.sends;
                            m.master = c.master;
                            m.mute = c.mute;
                            m.rev_send = c.rev_send;
                            m.rev_return = c.rev_return;
                        }
                    }
                    ui.close_menu();
                }
                if ui.button("Edit Bus Name").clicked() {
                    self.rename = Some((self.mix, self.state.mixes[self.mix].name.clone()));
                    ui.close_menu();
                }
                ui.separator();
                if ui.button("Clear Peaks").clicked() {
                    self.sim.clear_peaks();
                    ui.close_menu();
                }
            });
            ui.menu_button("Devices", |ui| {
                let cur = if self.state.family == Family::Pci424 { DeviceChoice::Pci424 } else { DeviceChoice::Mk3 };
                if ui.radio(cur == DeviceChoice::Pci424, "PCI-424 core system").clicked() && cur != DeviceChoice::Pci424 {
                    let s = if self.hw.connected {
                        self.hw.build_state(State::default_rig())
                    } else {
                        State::pci424(State::default_rig(), 48000).with_demo_mix()
                    };
                    self.set_state(s);
                    ui.close_menu();
                }
                if ui.radio(cur == DeviceChoice::Mk3, "828mk3 (CueMix FX with effects)").clicked() && cur != DeviceChoice::Mk3 {
                    self.set_state(State::mk3().with_demo_mix());
                    ui.close_menu();
                }
                ui.separator();
                ui.label(RichText::new(format!("Card link: {}", self.hw.status)).color(DIM).size(11.0));
            });
            ui.menu_button("View", |ui| {
                ui.menu_button("Peak Hold Time", |ui| {
                    for (k, (name, _)) in PEAK_HOLD.iter().enumerate() {
                        if ui.radio(self.sim.hold_idx == k, *name).clicked() {
                            self.sim.hold_idx = k;
                            ui.close_menu();
                        }
                    }
                });
            });
        });
    }

    fn header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("CueMix").color(TEXT).size(20.0).strong());
            ui.label(RichText::new("Studio").color(ACCENT).size(20.0));
            ui.add_space(10.0);
            let live = self.hw.connected && self.state.family == Family::Pci424;
            let (dot, txt) = if live { (ACCENT, self.state.device.clone()) } else { (SOLO, self.state.device.clone()) };
            let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), egui::Sense::hover());
            ui.painter().circle_filled(r.center(), 4.5, dot);
            ui.label(RichText::new(txt).color(TEXT).size(13.0))
                .on_hover_text(format!("Card link: {}", self.hw.status));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let mut lv = self.state.listen;
                if toggle(ui, &mut lv, "LISTEN", BLUE, 58.0).changed() {
                    self.state.listen = lv;
                }
                let mut tk = self.state.talk;
                if toggle(ui, &mut tk, "TALK", SOLO, 50.0).changed() {
                    self.state.talk = tk;
                }
                let any_solo = self.state.mixes[self.mix].sends.iter().any(|s| s.solo);
                if any_solo {
                    if push(ui, "SOLO ✕", 62.0).on_hover_text("Clear every solo in this mix").clicked() {
                        for s in self.state.mixes[self.mix].sends.iter_mut() {
                            s.solo = false;
                        }
                    }
                }
                if self.state.has_effects() {
                    let v = self.state.monitor_level;
                    let mut m = v;
                    let r = ui.add(egui::Slider::new(&mut m, 0.0..=1.0).show_value(false).text("MONITOR"));
                    if r.changed() {
                        self.state.monitor_level = m;
                    }
                }
            });
        });
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            let mut tabs = vec![(Tab::Mixes, "mixes"), (Tab::Inputs, "inputs"), (Tab::Outputs, "outputs")];
            if self.state.has_effects() {
                tabs.extend([(Tab::ChannelDyn, "channel / dynamics"), (Tab::Eq, "eq"), (Tab::Reverb, "reverb")]);
            } else {
                tabs.push((Tab::Clock, "clock & format"));
            }
            for (t, name) in tabs {
                let on = self.tab == t;
                let b = egui::Button::new(RichText::new(name).color(if on { Color32::BLACK } else { TEXT }).size(13.0))
                    .fill(if on { ACCENT } else { PANEL })
                    .rounding(4.0)
                    .min_size(vec2(0.0, 26.0));
                if ui.add(b).clicked() {
                    self.tab = t;
                }
            }
        });
    }

    // ------------------------------------------------------------ Mixes tab
    fn mixes_tab(&mut self, ui: &mut egui::Ui) {
        let h = ui.available_height();
        egui::SidePanel::right("bus")
            .resizable(false)
            .exact_width(250.0)
            .frame(egui::Frame::none().fill(MIXCOL.gamma_multiply(0.45)).inner_margin(10.0).rounding(6.0))
            .show_inside(ui, |ui| self.bus_section(ui, h));
        egui::ScrollArea::horizontal().id_salt("mixstrips").show(ui, |ui| {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                let n = self.state.inputs.len();
                let mut i = 0;
                while i < n {
                    let g = self.state.inputs[i].group.clone();
                    let mut j = i;
                    while j < n && self.state.inputs[j].group == g {
                        j += 1;
                    }
                    group_frame(ui, &g, |ui| {
                        for k in i..j {
                            self.mix_strip(ui, k, h - 40.0);
                        }
                    });
                    i = j;
                }
            });
        });
    }

    fn mix_strip(&mut self, ui: &mut egui::Ui, i: usize, h: f32) {
        let w = 58.0;
        let mixc = self.mix;
        egui::Frame::none().fill(PANEL).rounding(4.0).inner_margin(3.0).show(ui, |ui| {
            ui.set_width(w);
            ui.vertical_centered(|ui| {
                ui.spacing_mut().item_spacing.y = 3.0;
                let name = self.state.inputs[i].name.clone();
                let stereo_partner = self.state.inputs[i].stereo;
                let sel = self.sel == i && !self.sel_output;
                if name_label(ui, &name, w, sel).clicked() {
                    self.sel = i;
                    self.sel_output = false;
                }
                let send = &mut self.state.mixes[mixc].sends[i];
                let ro = fmt_pan(send.pan);
                let mut pan = send.pan;
                let pr = knob(ui, &mut pan, -1.0, 1.0, 0.0, Curve::Lin, "pan", ro, 30.0, ACCENT);
                if pr.changed() {
                    send.pan = pan;
                }
                pr.on_hover_text(format!("hardware word {}", pan_hw_word(send.pan)));
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    let mut s = send.solo;
                    if toggle(ui, &mut s, "S", SOLO, 27.0).changed() {
                        send.solo = s;
                    }
                    let mut m = send.mute;
                    if toggle(ui, &mut m, "M", MUTE, 27.0).changed() {
                        send.mute = m;
                    }
                });
                let fh = (h - 175.0).clamp(140.0, 420.0);
                let mut f = send.fader;
                let changed = ui
                    .horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        let c = fader(ui, &mut f, fh, 40.0, ACCENT).changed();
                        meter(ui, &self.sim.send_meters[i], vec2(9.0, fh));
                        c
                    })
                    .inner;
                if changed {
                    let delta = f - self.state.mixes[mixc].sends[i].fader;
                    self.state.mixes[mixc].sends[i].fader = f;
                    // stereo-paired inputs move together
                    let partner = if stereo_partner { Some(i + 1) } else if i > 0 && self.state.inputs[i - 1].stereo { Some(i - 1) } else { None };
                    if let Some(p) = partner {
                        if p < self.state.inputs.len() {
                            let pf = &mut self.state.mixes[mixc].sends[p].fader;
                            *pf = (*pf + delta).clamp(0.0, FADER_TOP);
                        }
                    }
                }
                readout(ui, fmt_fader(self.state.mixes[mixc].sends[i].fader), w);
            });
        });
    }

    fn bus_section(&mut self, ui: &mut egui::Ui, h: f32) {
        ui.label(RichText::new("MIX BUS").color(DIM).size(11.0));
        egui::Grid::new("busgrid").spacing(vec2(4.0, 4.0)).show(ui, |ui| {
            for k in 0..self.state.mixes.len() {
                let on = k == self.mix;
                let b = egui::Button::new(RichText::new(format!("{}", k + 1)).color(if on { Color32::BLACK } else { TEXT }))
                    .fill(if on { ACCENT } else { WELL })
                    .min_size(vec2(50.0, 22.0));
                if ui.add(b).on_hover_text(self.state.mixes[k].name.clone()).clicked() {
                    self.mix = k;
                }
                if k % 4 == 3 {
                    ui.end_row();
                }
            }
        });
        ui.add_space(6.0);
        let m = self.mix;
        let r = ui.add(
            egui::Label::new(RichText::new(&self.state.mixes[m].name).size(16.0).color(TEXT).strong()).sense(egui::Sense::click()),
        );
        if r.on_hover_text("Double-click to rename").double_clicked() {
            self.rename = Some((m, self.state.mixes[m].name.clone()));
        }
        ui.horizontal(|ui| {
            ui.label(RichText::new("mix assign").color(DIM).size(11.0));
            let pairs = self.state.output_pairs();
            let cur = self.state.mixes[m].output;
            let mut sel = cur;
            egui::ComboBox::from_id_salt("assign")
                .selected_text(if cur == usize::MAX { "—".to_string() } else { self.state.pair_name(cur) })
                .width(120.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut sel, usize::MAX, "—");
                    for p in pairs {
                        ui.selectable_value(&mut sel, p, self.state.pair_name(p));
                    }
                });
            if sel != cur {
                // an output pair carries one mix at a time
                for (k, mx) in self.state.mixes.iter_mut().enumerate() {
                    if k != m && mx.output == sel && sel != usize::MAX {
                        mx.output = usize::MAX;
                    }
                }
                self.state.mixes[m].output = sel;
                if sel != usize::MAX && !self.state.patchbay {
                    self.state.patchbay = true;
                    self.notify("Patchbay switched on so the mix reaches its outputs");
                }
            }
        });
        ui.add_space(6.0);
        ui.horizontal_top(|ui| {
            let fh = (h - 260.0).clamp(140.0, 420.0);
            ui.vertical(|ui| {
                ui.label(RichText::new("MASTER").color(DIM).size(11.0));
                let mut f = self.state.mixes[m].master;
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    if fader(ui, &mut f, fh, 56.0, Color32::from_rgb(180, 120, 200)).changed() {
                        self.state.mixes[m].master = f;
                    }
                    meter(ui, &self.sim.mix_l[m], vec2(10.0, fh));
                    meter(ui, &self.sim.mix_r[m], vec2(10.0, fh));
                });
                readout(ui, fmt_fader(self.state.mixes[m].master), 86.0);
                let mut mu = self.state.mixes[m].mute;
                if toggle(ui, &mut mu, "MUTE", MUTE, 86.0).changed() {
                    self.state.mixes[m].mute = mu;
                }
            });
            if self.state.has_effects() {
                ui.vertical(|ui| {
                    let mx = &mut self.state.mixes[m];
                    let ro = fmt_fader(mx.rev_send);
                    let mut v = mx.rev_send;
                    if knob(ui, &mut v, 0.0, 1.0, 0.0, Curve::Lin, "rev send", ro, 40.0, ACCENT).changed() {
                        mx.rev_send = v;
                    }
                    let ro = fmt_fader(mx.rev_return);
                    let mut v = mx.rev_return;
                    if knob(ui, &mut v, 0.0, 1.0, 0.0, Curve::Lin, "rev return", ro, 40.0, ACCENT).changed() {
                        mx.rev_return = v;
                    }
                });
            }
        });
    }

    // ------------------------------------------------------------ Inputs tab
    fn inputs_tab(&mut self, ui: &mut egui::Ui) {
        let fx = self.state.has_effects();
        egui::ScrollArea::horizontal().id_salt("instrips").show(ui, |ui| {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                let n = self.state.inputs.len();
                let mut i = 0;
                while i < n {
                    let g = self.state.inputs[i].group.clone();
                    let mut j = i;
                    while j < n && self.state.inputs[j].group == g {
                        j += 1;
                    }
                    group_frame(ui, &g, |ui| {
                        for k in i..j {
                            self.input_strip(ui, k, fx);
                        }
                    });
                    i = j;
                }
            });
        });
    }

    fn input_strip(&mut self, ui: &mut egui::Ui, i: usize, fx: bool) {
        let w = 64.0;
        egui::Frame::none().fill(PANEL).rounding(4.0).inner_margin(3.0).show(ui, |ui| {
            ui.set_width(w);
            ui.vertical_centered(|ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                let sel = self.sel == i && !self.sel_output;
                let name = self.state.inputs[i].name.clone();
                if name_label(ui, &name, w, sel).clicked() {
                    self.sel = i;
                    self.sel_output = false;
                }
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    ui.add_space(8.0);
                    meter(ui, &self.sim.in_pre[i], vec2(9.0, 110.0));
                    meter(ui, &self.sim.in_post[i], vec2(9.0, 110.0));
                    ui.add_space(4.0);
                    ui.vertical(|ui| {
                        ui.label(RichText::new("pre").size(9.0).color(DIM));
                        ui.add_space(80.0);
                        ui.label(RichText::new("post").size(9.0).color(DIM));
                    });
                });
                let v = &mut self.state.inputs[i];
                let (lo, hi) = v.trim_range;
                let ro = format!("{:+.0} dB", v.trim);
                let mut t = v.trim;
                let kr = knob(ui, &mut t, lo, hi, lo.max(0.0).min(hi), Curve::Lin, "trim", ro, 34.0, ACCENT);
                if kr.changed() {
                    v.trim = t.round();
                }
                kr.on_hover_text(format!("hardware word {}", trim_hw_word(v.trim, lo, hi)));
                let b = |ui: &mut egui::Ui, val: &mut bool, txt: &str, c: Color32| {
                    let mut x = *val;
                    if toggle(ui, &mut x, txt, c, w).changed() {
                        *val = x;
                    }
                };
                b(ui, &mut v.pad, "pad -20", BLUE);
                b(ui, &mut v.phase, "phase Ø", BLUE);
                b(ui, &mut v.stereo, "stereo", ACCENT);
                b(ui, &mut v.mute, "mute", MUTE);
                if fx {
                    b(ui, &mut v.eq.on, "eq", ACCENT);
                    b(ui, &mut v.dynamics.on, "dynamics", ACCENT);
                    let ro = fmt_fader(v.rev_send);
                    let mut r = v.rev_send;
                    if knob(ui, &mut r, 0.0, 1.0, 0.0, Curve::Lin, "rev send", ro, 30.0, ACCENT).changed() {
                        v.rev_send = r;
                    }
                    b(ui, &mut v.talk, "talk", SOLO);
                    b(ui, &mut v.listen, "listen", BLUE);
                }
            });
        });
    }

    // ------------------------------------------------------------ Outputs tab
    fn outputs_tab(&mut self, ui: &mut egui::Ui) {
        let fx = self.state.has_effects();
        let h = ui.available_height();
        if !fx {
            ui.horizontal(|ui| {
                let mut p = self.state.patchbay;
                if toggle(ui, &mut p, "PATCHBAY", ACCENT, 90.0)
                    .on_hover_text("Off: every output carries its normal (Main = computer playback). On: mixes reach the outputs they're assigned to.")
                    .changed()
                {
                    self.state.patchbay = p;
                }
                ui.label(
                    RichText::new(if p { "mixes are patched to their assigned outputs" } else { "bypassed — outputs sit at their normals (Main carries the computer's playback)" })
                        .color(DIM)
                        .size(11.0),
                );
            });
            ui.add_space(4.0);
        }
        egui::ScrollArea::horizontal().id_salt("outstrips").show(ui, |ui| {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                let n = self.state.outputs.len();
                let mut i = 0;
                while i < n {
                    let g = self.state.outputs[i].group.clone();
                    let mut j = i;
                    while j < n && self.state.outputs[j].group == g {
                        j += 1;
                    }
                    group_frame(ui, &g, |ui| {
                        for k in i..j {
                            self.output_strip(ui, k, fx, h - 60.0);
                        }
                    });
                    i = j;
                }
            });
        });
    }

    fn source_of(&self, o: usize) -> String {
        for m in &self.state.mixes {
            if self.state.patchbay && m.output == o {
                return format!("{} L", m.name);
            }
            if self.state.patchbay && m.output != usize::MAX && m.output + 1 == o {
                return format!("{} R", m.name);
            }
        }
        if self.state.outputs[o].token.contains("Main") {
            "computer".into()
        } else {
            "direct".into()
        }
    }

    fn output_strip(&mut self, ui: &mut egui::Ui, o: usize, fx: bool, h: f32) {
        let w = 58.0;
        let src = self.source_of(o);
        egui::Frame::none().fill(PANEL).rounding(4.0).inner_margin(3.0).show(ui, |ui| {
            ui.set_width(w);
            ui.vertical_centered(|ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                let sel = self.sel == o && self.sel_output;
                let name = self.state.outputs[o].name.clone();
                if name_label(ui, &name, w, sel).clicked() {
                    self.sel = o;
                    self.sel_output = true;
                }
                ui.label(RichText::new(src).size(9.5).color(ACCENT));
                let fh = if fx { (h - 260.0).clamp(120.0, 360.0) } else { (h - 140.0).clamp(140.0, 420.0) };
                let mut f = self.state.outputs[o].fader;
                let ch = ui
                    .horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        let c = fader(ui, &mut f, fh, 40.0, BLUE).changed();
                        meter(ui, &self.sim.out_l[o], vec2(9.0, fh));
                        c
                    })
                    .inner;
                if ch {
                    let delta = f - self.state.outputs[o].fader;
                    self.state.outputs[o].fader = f;
                    let partner = if self.state.outputs[o].stereo { Some(o + 1) } else if o > 0 && self.state.outputs[o - 1].stereo { Some(o - 1) } else { None };
                    if let Some(p) = partner {
                        if p < self.state.outputs.len() {
                            let pf = &mut self.state.outputs[p].fader;
                            *pf = (*pf + delta).clamp(0.0, FADER_TOP);
                        }
                    }
                }
                readout(ui, fmt_fader(self.state.outputs[o].fader), w);
                let v = &mut self.state.outputs[o];
                let b = |ui: &mut egui::Ui, val: &mut bool, txt: &str, c: Color32| {
                    let mut x = *val;
                    if toggle(ui, &mut x, txt, c, w).changed() {
                        *val = x;
                    }
                };
                b(ui, &mut v.mute, "mute", MUTE);
                b(ui, &mut v.stereo, "ST", ACCENT);
                if fx {
                    b(ui, &mut v.eq.on, "eq", ACCENT);
                    b(ui, &mut v.dynamics.on, "dynamics", ACCENT);
                    let ro = fmt_fader(v.rev_return);
                    let mut r = v.rev_return;
                    if knob(ui, &mut r, 0.0, 1.0, 0.0, Curve::Lin, "rev rtn", ro, 28.0, ACCENT).changed() {
                        v.rev_return = r;
                    }
                    b(ui, &mut v.monitor, "monitor", BLUE);
                    b(ui, &mut v.talk, "talk", SOLO);
                    b(ui, &mut v.listen, "listen", BLUE);
                }
            });
        });
    }

    // ------------------------------------------------------------ channel picker
    fn channel_picker(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let len = if self.sel_output { self.state.outputs.len() } else { self.state.inputs.len() };
            self.sel = self.sel.min(len.saturating_sub(1));
            if push(ui, "<<", 36.0).clicked() && self.sel > 0 {
                self.sel -= 1;
            }
            let name = if self.sel_output { self.state.outputs[self.sel].name.clone() } else { self.state.inputs[self.sel].name.clone() };
            ui.label(RichText::new(format!("{}  {}", if self.sel_output { "OUTPUT" } else { "INPUT" }, name)).size(16.0).color(TEXT).strong());
            if push(ui, ">>", 36.0).clicked() && self.sel + 1 < len {
                self.sel += 1;
            }
            let mut f = self.sel_output;
            if toggle(ui, &mut f, "FLIP", ACCENT, 50.0).on_hover_text("Switch between inputs and outputs").changed() {
                self.sel_output = f;
                self.sel = 0;
            }
        });
        ui.add_space(6.0);
    }

    fn dyn_mut(&mut self) -> &mut Dyn {
        if self.sel_output {
            &mut self.state.outputs[self.sel].dynamics
        } else {
            &mut self.state.inputs[self.sel].dynamics
        }
    }

    fn channel_tab(&mut self, ui: &mut egui::Ui) {
        self.channel_picker(ui);
        ui.horizontal_top(|ui| {
            if !self.sel_output {
                let v = &mut self.state.inputs[self.sel];
                section(ui, "channel", |ui| {
                    ui.horizontal(|ui| {
                        toggle(ui, &mut v.pad, "pad", BLUE, 60.0);
                        let mut p4 = v.ref_plus4;
                        if toggle(ui, &mut p4, "+4 dBu", BLUE, 60.0).clicked() {
                            v.ref_plus4 = true;
                        }
                        let mut m10 = !v.ref_plus4;
                        if toggle(ui, &mut m10, "-10 dBV", BLUE, 60.0).clicked() {
                            v.ref_plus4 = false;
                        }
                    });
                    ui.add_space(6.0);
                    ui.label(RichText::new("stereo").color(DIM));
                    ui.horizontal(|ui| {
                        let mut n = !v.ms;
                        if toggle(ui, &mut n, "normal", ACCENT, 60.0).clicked() {
                            v.ms = false;
                        }
                        let mut ms = v.ms;
                        if toggle(ui, &mut ms, "M/S", ACCENT, 60.0).clicked() {
                            v.ms = true;
                        }
                        toggle(ui, &mut v.swap, "swap L/R", ACCENT, 70.0);
                    });
                    let ro = format!("{:.0}%", v.width * 100.0);
                    knob(ui, &mut v.width, -1.0, 1.0, 1.0, Curve::Lin, "width", ro, 40.0, ACCENT);
                    ui.add_space(6.0);
                    ui.label(RichText::new("overload protection").color(DIM));
                    ui.horizontal(|ui| {
                        toggle(ui, &mut v.vlimit, "vlimit", ACCENT, 56.0);
                        toggle(ui, &mut v.lookahead, "lookahead", ACCENT, 70.0);
                        toggle(ui, &mut v.softclip, "soft clip", ACCENT, 64.0);
                    });
                });
            }
            let gr = if self.sel_output { self.sim.out_gr[self.sel] } else { self.sim.in_gr[self.sel] };
            let d = self.dyn_mut();
            section(ui, "dynamics", |ui| {
                toggle(ui, &mut d.on, "enable", ACCENT, 80.0);
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    toggle(ui, &mut d.comp_on, "compressor", ACCENT, 90.0);
                    let mut pk = !d.rms;
                    if toggle(ui, &mut pk, "peak", BLUE, 44.0).clicked() {
                        d.rms = false;
                    }
                    let mut rm = d.rms;
                    if toggle(ui, &mut rm, "RMS", BLUE, 44.0).clicked() {
                        d.rms = true;
                    }
                });
                ui.horizontal(|ui| {
                    let ro = format!("{:.1} dB", d.threshold);
                    knob(ui, &mut d.threshold, COMP_THRESHOLD.0, COMP_THRESHOLD.1, -20.0, Curve::Lin, "threshold", ro, 44.0, ACCENT);
                    let ro = format!("{:.1}:1", d.ratio);
                    knob(ui, &mut d.ratio, COMP_RATIO.0, COMP_RATIO.1, 2.0, Curve::Lin, "ratio", ro, 44.0, ACCENT);
                    let ro = fmt_ms(d.attack);
                    knob(ui, &mut d.attack, COMP_ATTACK.0, COMP_ATTACK.1, 10.0, Curve::Lin, "attack", ro, 44.0, ACCENT);
                    let ro = fmt_ms(d.release);
                    knob(ui, &mut d.release, COMP_RELEASE.0, COMP_RELEASE.1, 100.0, Curve::Log, "release", ro, 44.0, ACCENT);
                    let ro = format!("{:.1} dB", d.trim);
                    knob(ui, &mut d.trim, COMP_TRIM.0, COMP_TRIM.1, 0.0, Curve::Lin, "trim", ro, 44.0, ACCENT);
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    toggle(ui, &mut d.lev_on, "leveler", ACCENT, 90.0);
                    let mut c = !d.lev_limit;
                    if toggle(ui, &mut c, "comp", BLUE, 44.0).clicked() {
                        d.lev_limit = false;
                    }
                    let mut l = d.lev_limit;
                    if toggle(ui, &mut l, "limit", BLUE, 44.0).clicked() {
                        d.lev_limit = true;
                    }
                });
                ui.horizontal(|ui| {
                    let ro = format!("{:.0}", d.lev_reduction);
                    knob(ui, &mut d.lev_reduction, LEVELER.0, LEVELER.1, 0.0, Curve::Lin, "gain reduction", ro, 44.0, ACCENT);
                    let ro = format!("{:.0}", d.lev_makeup);
                    knob(ui, &mut d.lev_makeup, LEVELER.0, LEVELER.1, 0.0, Curve::Lin, "makeup gain", ro, 44.0, ACCENT);
                });
                ui.add_space(6.0);
                ui.label(RichText::new(format!("gain reduction  {:.1} dB", gr)).color(DIM));
                let (r, _) = ui.allocate_exact_size(vec2(260.0, 10.0), egui::Sense::hover());
                ui.painter().rect_filled(r, 2.0, WELL);
                let f = (gr / 24.0).clamp(0.0, 1.0);
                ui.painter().rect_filled(egui::Rect::from_min_max(pos2(r.right() - r.width() * f, r.top()), r.max), 2.0, SOLO);
            });
        });
    }

    // ------------------------------------------------------------ EQ tab
    fn eq_tab(&mut self, ui: &mut egui::Ui) {
        self.channel_picker(ui);
        let eq = if self.sel_output { &mut self.state.outputs[self.sel].eq } else { &mut self.state.inputs[self.sel].eq };
        ui.horizontal(|ui| {
            toggle(ui, &mut eq.on, "EQ enable", ACCENT, 90.0);
        });
        ui.add_space(4.0);
        // response graph
        let w = ui.available_width().min(1100.0);
        let (rect, _) = ui.allocate_exact_size(vec2(w, 200.0), egui::Sense::hover());
        let p = ui.painter();
        p.rect_filled(rect, 6.0, WELL);
        let fx = |f: f32| rect.left() + to_norm(f, EQ_FREQ.0, EQ_FREQ.1, Curve::Log) * rect.width();
        let gy = |db: f32| rect.center().y - db / 24.0 * rect.height() * 0.5;
        for f in [50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0] {
            p.line_segment([pos2(fx(f), rect.top()), pos2(fx(f), rect.bottom())], Stroke::new(1.0_f32, PANEL_HI));
            let t = if f >= 1000.0 { format!("{}k", f / 1000.0) } else { format!("{}", f) };
            p.text(pos2(fx(f) + 2.0, rect.bottom() - 2.0), Align2::LEFT_BOTTOM, t, FontId::proportional(9.0), DIM);
        }
        for db in [-20.0, -10.0, 0.0, 10.0, 20.0] {
            p.line_segment([pos2(rect.left(), gy(db)), pos2(rect.right(), gy(db))], Stroke::new(if db == 0.0 { 1.5_f32 } else { 1.0 }, PANEL_HI));
            p.text(pos2(rect.left() + 3.0, gy(db)), Align2::LEFT_BOTTOM, format!("{:+}", db), FontId::proportional(9.0), DIM);
        }
        let pts: Vec<egui::Pos2> = (0..=240)
            .map(|k| {
                let f = from_norm(k as f32 / 240.0, EQ_FREQ.0, EQ_FREQ.1, Curve::Log);
                pos2(fx(f), gy(eqcurve::response_db(eq, f).clamp(-24.0, 24.0)))
            })
            .collect();
        p.add(egui::Shape::line(pts, Stroke::new(2.0_f32, if eq.on { ACCENT } else { DIM })));
        let colors = [Color32::from_rgb(230, 90, 80), Color32::from_rgb(240, 170, 60), Color32::from_rgb(120, 210, 90), Color32::from_rgb(80, 170, 240), Color32::from_rgb(190, 110, 230)];
        for (b, c) in eq.bands.iter().zip(colors) {
            if b.on {
                p.circle_filled(pos2(fx(b.freq), gy(b.gain)), 5.0, c);
            }
        }
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            section(ui, "hpf", |ui| {
                toggle(ui, &mut eq.hpf_on, "enable", ACCENT, 60.0);
                ui.horizontal(|ui| {
                    let mut a = !eq.hpf_slope12;
                    if toggle(ui, &mut a, "6", BLUE, 28.0).clicked() {
                        eq.hpf_slope12 = false;
                    }
                    let mut b = eq.hpf_slope12;
                    if toggle(ui, &mut b, "12", BLUE, 28.0).clicked() {
                        eq.hpf_slope12 = true;
                    }
                });
                let ro = fmt_freq(eq.hpf_freq);
                knob(ui, &mut eq.hpf_freq, EQ_FREQ.0, EQ_FREQ.1, EQ_FREQ.0, Curve::Log, "freq", ro, 40.0, ACCENT);
            });
            for (k, b) in eq.bands.iter_mut().enumerate() {
                section(ui, BAND_NAMES[k], |ui| {
                    ui.horizontal(|ui| {
                        toggle(ui, &mut b.on, "on", colors[k], 30.0);
                        let types: &[(BandType, &str)] = if k == 0 || k == 4 {
                            &[(BandType::One, "I"), (BandType::Two, "II"), (BandType::Shelf, "shelf")]
                        } else {
                            &[(BandType::One, "I"), (BandType::Two, "II")]
                        };
                        for (t, name) in types {
                            let mut on = b.kind == *t;
                            if toggle(ui, &mut on, name, BLUE, if *name == "shelf" { 36.0 } else { 22.0 }).clicked() {
                                b.kind = *t;
                            }
                        }
                    });
                    ui.horizontal(|ui| {
                        let ro = fmt_freq(b.freq);
                        let def = [60.0, 250.0, 1000.0, 4000.0, 12000.0][k];
                        knob(ui, &mut b.freq, EQ_FREQ.0, EQ_FREQ.1, def, Curve::Log, "freq", ro, 38.0, colors[k]);
                        let ro = format!("{:.2} dB", b.gain);
                        knob(ui, &mut b.gain, EQ_GAIN.0, EQ_GAIN.1, 0.0, Curve::Lin, "gain", ro, 38.0, colors[k]);
                        let ro = format!("{:.2}", b.q);
                        knob(ui, &mut b.q, EQ_Q.0, EQ_Q.1, 1.0, Curve::Lin, "Q", ro, 38.0, colors[k]);
                    });
                });
            }
            section(ui, "lpf", |ui| {
                toggle(ui, &mut eq.lpf_on, "enable", ACCENT, 60.0);
                ui.horizontal(|ui| {
                    let mut a = !eq.lpf_slope12;
                    if toggle(ui, &mut a, "6", BLUE, 28.0).clicked() {
                        eq.lpf_slope12 = false;
                    }
                    let mut b = eq.lpf_slope12;
                    if toggle(ui, &mut b, "12", BLUE, 28.0).clicked() {
                        eq.lpf_slope12 = true;
                    }
                });
                let ro = fmt_freq(eq.lpf_freq);
                knob(ui, &mut eq.lpf_freq, EQ_FREQ.0, EQ_FREQ.1, EQ_FREQ.1, Curve::Log, "freq", ro, 40.0, ACCENT);
            });
        });
    }

    // ------------------------------------------------------------ Reverb tab
    fn reverb_tab(&mut self, ui: &mut egui::Ui) {
        let r = &mut self.state.reverb;
        ui.horizontal(|ui| {
            toggle(ui, &mut r.on, "reverb enable", ACCENT, 110.0);
            ui.add_space(20.0);
            ui.label(RichText::new("split point").color(DIM));
            let mut m = !r.split_outputs;
            if toggle(ui, &mut m, "mixes", BLUE, 60.0).clicked() {
                r.split_outputs = false;
            }
            let mut o = r.split_outputs;
            if toggle(ui, &mut o, "outputs", BLUE, 60.0).clicked() {
                r.split_outputs = true;
            }
        });
        ui.add_space(8.0);
        ui.horizontal_top(|ui| {
            section(ui, "primary controls", |ui| {
                ui.horizontal(|ui| {
                    // reverb time follows the original's 100 ms … 60 s curve centred on 2 s
                    let ro = fmt_ms(r.time);
                    knob(ui, &mut r.time, REV_TIME.0, REV_TIME.1, REV_TIME_CENTRE, Curve::Log, "reverb time", ro, 56.0, ACCENT);
                    let ro = fmt_ms(r.predelay);
                    knob(ui, &mut r.predelay, REV_PREDELAY.0, REV_PREDELAY.1, REV_PREDELAY.0, Curve::Lin, "pre-delay", ro, 56.0, ACCENT);
                });
                ui.label(RichText::new("shelf filter").color(DIM));
                ui.horizontal(|ui| {
                    let ro = fmt_freq(r.shelf_freq);
                    knob(ui, &mut r.shelf_freq, REV_SHELF_FREQ.0, REV_SHELF_FREQ.1, REV_SHELF_FREQ_DEFAULT, Curve::Log, "freq", ro, 44.0, ACCENT);
                    let ro = format!("{:.1} dB", r.shelf_cut);
                    knob(ui, &mut r.shelf_cut, REV_SHELF_CUT.0, REV_SHELF_CUT.1, 0.0, Curve::Lin, "cut", ro, 44.0, ACCENT);
                });
            });
            section(ui, "early reflections", |ui| {
                ui.horizontal(|ui| {
                    for (k, n) in ["A", "B", "C", "D", "E"].iter().enumerate() {
                        let mut on = r.refl_set == k;
                        if toggle(ui, &mut on, n, BLUE, 26.0).clicked() {
                            r.refl_set = k;
                        }
                    }
                });
                ui.horizontal(|ui| {
                    let ro = format!("{:.0}%", r.refl_size);
                    knob(ui, &mut r.refl_size, REV_REFL_SIZE.0, REV_REFL_SIZE.1, REV_REFL_SIZE_DEFAULT, Curve::Lin, "size", ro, 44.0, ACCENT);
                    let ro = fmt_db(db_from_gain(r.refl_level));
                    knob(ui, &mut r.refl_level, 0.0, 1.0, 1.0, Curve::Lin, "level", ro, 44.0, ACCENT);
                });
            });
            section(ui, "reverb design", |ui| {
                ui.label(RichText::new("reverb time").color(DIM));
                ui.horizontal(|ui| {
                    for (v, n, d) in [(&mut r.time_low, "low", REV_TIME_LOW_DEFAULT), (&mut r.time_mid, "mid", REV_TIME_MID_DEFAULT), (&mut r.time_high, "high", REV_TIME_HIGH_DEFAULT)] {
                        let ro = format!("{:.0}%", *v);
                        knob(ui, v, REV_TIME_PCT.0, REV_TIME_PCT.1, d, Curve::Lin, n, ro, 40.0, ACCENT);
                    }
                });
                ui.label(RichText::new("crossover").color(DIM));
                ui.horizontal(|ui| {
                    let ro = fmt_freq(r.xover_low);
                    knob(ui, &mut r.xover_low, REV_XOVER_LOW.0, REV_XOVER_LOW.1, REV_XOVER_LOW_DEFAULT, Curve::Log, "low", ro, 40.0, ACCENT);
                    let ro = fmt_freq(r.xover_high);
                    knob(ui, &mut r.xover_high, REV_XOVER_HIGH.0, REV_XOVER_HIGH.1, REV_XOVER_HIGH_DEFAULT, Curve::Log, "high", ro, 40.0, ACCENT);
                    let ro = format!("{:.0}%", r.width * 100.0);
                    knob(ui, &mut r.width, REV_WIDTH.0, REV_WIDTH.1, REV_WIDTH.1, Curve::Lin, "width", ro, 40.0, ACCENT);
                });
            });
        });
    }

    // ------------------------------------------------------------ Clock & format tab (PCI-424)
    fn clock_tab(&mut self, ui: &mut egui::Ui) {
        let live = self.hw.connected;
        ui.horizontal_top(|ui| {
            section(ui, "clock & format", |ui| {
                egui::Grid::new("clock").num_columns(2).spacing(vec2(12.0, 8.0)).show(ui, |ui| {
                    ui.label(RichText::new("Clock source").color(DIM));
                    let mut cs = self.state.clock_source.clone();
                    let items: Vec<String> = self
                        .hw
                        .controls
                        .get("Clock Source")
                        .map(|c| c.items.clone())
                        .filter(|v| !v.is_empty())
                        .unwrap_or_else(|| CLOCK_SOURCES.iter().map(|s| s.to_string()).collect());
                    egui::ComboBox::from_id_salt("clk").selected_text(&cs).width(150.0).show_ui(ui, |ui| {
                        for it in &items {
                            ui.selectable_value(&mut cs, it.clone(), it);
                        }
                    });
                    self.state.clock_source = cs;
                    ui.end_row();
                    ui.label(RichText::new("Sample rate").color(DIM));
                    let mut sr = self.state.sample_rate;
                    egui::ComboBox::from_id_salt("sr").selected_text(format!("{} Hz", sr)).width(150.0).show_ui(ui, |ui| {
                        for (fam, rates) in [("1x", &SAMPLE_RATES[0..2]), ("2x", &SAMPLE_RATES[2..4]), ("4x", &SAMPLE_RATES[4..6])] {
                            ui.label(RichText::new(fam).color(DIM).size(10.0));
                            for r in rates {
                                ui.selectable_value(&mut sr, *r, format!("{} Hz", r));
                            }
                        }
                    });
                    if sr != self.state.sample_rate {
                        if live {
                            self.state.sample_rate = sr;
                        } else {
                            // simulated rig: channel counts shrink with the rate family
                            let mut s = State::pci424(self.state.rig.clone(), sr).with_demo_mix();
                            s.clock_source = self.state.clock_source.clone();
                            self.set_state(s);
                        }
                    }
                    ui.end_row();
                    ui.label(RichText::new("Locked rate").color(DIM));
                    let locked = self.hw.controls.get("Clock Rate").map(|c| c.value.clone()).unwrap_or_else(|| self.state.sample_rate.to_string());
                    ui.label(RichText::new(format!("{} Hz", locked)).color(BLUE).monospace());
                    ui.end_row();
                    ui.label(RichText::new("Meters").color(DIM));
                    let mut m = self.state.meters;
                    let label = if m { "on" } else { "off" };
                    if toggle(ui, &mut m, label, ACCENT, 60.0).changed() {
                        self.state.meters = m;
                    }
                    ui.end_row();
                    ui.label(RichText::new("Talkback dim").color(DIM));
                    let mut a = self.state.talk_atten;
                    if ui.add(egui::Slider::new(&mut a, -60.0..=0.0).suffix(" dB").step_by(1.0)).changed() {
                        self.state.talk_atten = a;
                    }
                    ui.end_row();
                });
            });
            section(ui, "AudioWire interfaces", |ui| {
                if live {
                    for s in 0..4 {
                        let slot = (b'A' + s as u8) as char;
                        let m = self.hw.controls.get(&format!("Slot {} Interface", slot)).map(|c| c.value.clone()).unwrap_or_else(|| "—".into());
                        ui.label(format!("Slot {}   {}", slot, m));
                    }
                    ui.label(RichText::new("Reported by the card").color(DIM).size(11.0));
                } else {
                    let mut rig = self.state.rig.clone();
                    for (s, cur) in rig.iter_mut().enumerate() {
                        let slot = (b'A' + s as u8) as char;
                        ui.horizontal(|ui| {
                            ui.label(format!("Slot {}", slot));
                            egui::ComboBox::from_id_salt(("slot", s))
                                .selected_text(rig_model(cur).unwrap_or("empty"))
                                .width(110.0)
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(cur, String::new(), "empty");
                                    for (k, m, _) in RIG_IFACES {
                                        ui.selectable_value(cur, k.to_string(), m);
                                    }
                                });
                        });
                    }
                    if rig != self.state.rig {
                        if rig.iter().all(|r| r.is_empty()) {
                            self.notify("At least one interface is needed");
                        } else {
                            let s = State::pci424(rig, self.state.sample_rate).with_demo_mix();
                            self.set_state(s);
                        }
                    }
                    ui.label(RichText::new("Simulated rig — on Linux with the motu424 driver\nthe card reports its own interfaces.").color(DIM).size(11.0));
                }
            });
            section(ui, "card link", |ui| {
                ui.label(RichText::new(&self.hw.status).color(if live { ACCENT } else { SOLO }));
                ui.label(
                    RichText::new(match &self.hw.tool {
                        Some(t) => format!("tool: {}", t),
                        None => "tool: motu424-ctl (Linux, from the motu424 driver project)".into(),
                    })
                    .color(DIM)
                    .size(11.0),
                );
                ui.label(RichText::new("Controls follow docs/cuemix-control-map.md:\nInput NN Trim/Pad/Phase/Stereo/Mute,\nMix KK Input NN Volume/Pan/Mute/Solo,\nMix KK Master, Output NN Volume/Source…").color(DIM).size(11.0));
            });
        });
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        if self.show_save {
            let mut open = true;
            egui::Window::new("Save Configuration").collapsible(false).resizable(false).open(&mut open).show(ctx, |ui| {
                ui.text_edit_singleline(&mut self.config_name);
                if ui.button("Save").clicked() && !self.config_name.trim().is_empty() {
                    self.configs.insert(self.config_name.trim().to_string(), self.state.clone());
                    self.save_configs();
                    self.show_save = false;
                    let n = self.config_name.clone();
                    self.notify(&format!("Saved “{}”", n));
                }
            });
            if !open {
                self.show_save = false;
            }
        }
        if self.show_io {
            let mut open = true;
            egui::Window::new("Import / Export").collapsible(false).resizable(false).open(&mut open).show(ctx, |ui| {
                ui.label("File");
                ui.add(egui::TextEdit::singleline(&mut self.io_path).desired_width(360.0));
                ui.horizontal(|ui| {
                    if ui.button("Export current setup").clicked() {
                        let ok = serde_json::to_string_pretty(&self.state).ok().and_then(|s| std::fs::write(&self.io_path, s).ok()).is_some();
                        self.notify(if ok { "Exported" } else { "Could not write that file" });
                    }
                    if ui.button("Import").clicked() {
                        match std::fs::read_to_string(&self.io_path).ok().and_then(|s| serde_json::from_str::<State>(&s).ok()) {
                            Some(s) => {
                                self.set_state(s);
                                self.notify("Imported");
                            }
                            None => self.notify("That file isn't a saved setup"),
                        }
                    }
                });
            });
            if !open {
                self.show_io = false;
            }
        }
        if let Some((m, mut name)) = self.rename.take() {
            let mut open = true;
            let mut done = false;
            egui::Window::new("Edit Bus Name").collapsible(false).resizable(false).open(&mut open).show(ctx, |ui| {
                let r = ui.text_edit_singleline(&mut name);
                if ui.button("OK").clicked() || (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))) {
                    done = true;
                }
            });
            if done {
                self.state.mixes[m].name = name;
            } else if open {
                self.rename = Some((m, name));
            }
        }
        if let Some((msg, t)) = &mut self.toast {
            *t += ctx.input(|i| i.stable_dt) as f64;
            let alpha = if *t > 2.5 { (3.0 - *t).max(0.0) as f32 * 2.0 } else { 1.0 };
            egui::Area::new(egui::Id::new("toast")).anchor(Align2::CENTER_BOTTOM, vec2(0.0, -20.0)).show(ctx, |ui| {
                egui::Frame::none().fill(PANEL_HI.gamma_multiply(alpha)).rounding(6.0).inner_margin(10.0).show(ui, |ui| {
                    ui.label(RichText::new(msg.as_str()).color(TEXT.gamma_multiply(alpha)));
                });
            });
            if *t > 3.0 {
                self.toast = None;
            }
        }
    }

    pub fn ui(&mut self, ctx: &egui::Context) {
        self.frame += 1;
        let dt = ctx.input(|i| i.stable_dt).clamp(0.001, 0.1);

        // hardware: pull what the card says
        let (rebuild, values) = self.hw.poll();
        if self.state.family == Family::Pci424 {
            if rebuild && self.hw.connected {
                let s = self.hw.build_state(self.state.rig.clone());
                self.set_state(s);
                self.undo.clear();
            } else if values && self.hw.connected {
                let before = self.state.clone();
                self.hw.apply(&mut self.state);
                if before != self.state {
                    self.committed = self.state.clone();
                }
            }
        }

        let (undo, redo) = ctx.input(|i| {
            let cmd = i.modifiers.command;
            (cmd && !i.modifiers.shift && i.key_pressed(egui::Key::Z), cmd && (i.key_pressed(egui::Key::Y) || (i.modifiers.shift && i.key_pressed(egui::Key::Z))))
        });
        if undo {
            self.undo();
        }
        if redo {
            self.redo();
        }

        egui::TopBottomPanel::top("menu").frame(egui::Frame::none().fill(PANEL).inner_margin(egui::Margin::symmetric(8.0, 2.0))).show(ctx, |ui| {
            self.menu_bar(ui);
        });
        egui::TopBottomPanel::top("header").frame(egui::Frame::none().fill(BG).inner_margin(egui::Margin::symmetric(12.0, 8.0))).show(ctx, |ui| {
            self.header(ui);
        });
        egui::CentralPanel::default().frame(egui::Frame::none().fill(BG).inner_margin(egui::Margin::symmetric(12.0, 6.0))).show(ctx, |ui| match self.tab {
            Tab::Mixes => self.mixes_tab(ui),
            Tab::Inputs => self.inputs_tab(ui),
            Tab::Outputs => self.outputs_tab(ui),
            Tab::ChannelDyn => self.channel_tab(ui),
            Tab::Eq => self.eq_tab(ui),
            Tab::Reverb => self.reverb_tab(ui),
            Tab::Clock => self.clock_tab(ui),
        });
        self.dialogs(ctx);

        // one undo step per gesture: commit once the pointer is released
        let pointer_down = ctx.input(|i| i.pointer.any_down());
        if !pointer_down && self.state != self.committed {
            self.undo.push(std::mem::replace(&mut self.committed, self.state.clone()));
            if self.undo.len() > 200 {
                self.undo.remove(0);
            }
            self.redo.clear();
        }
        if self.state.family == Family::Pci424 {
            self.hw.push(&self.state);
        }
        if self.state.meters {
            self.sim.step(&self.state, self.mix, dt);
        }
        ctx.request_repaint();
    }
}

fn group_frame(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::none().fill(Color32::from_rgb(22, 22, 27)).rounding(6.0).inner_margin(4.0).show(ui, |ui| {
        ui.vertical(|ui| {
            ui.label(RichText::new(title).size(10.5).color(ACCENT));
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 3.0;
                add(ui);
            });
        });
    });
}
