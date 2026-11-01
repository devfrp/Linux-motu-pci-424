// SPDX-License-Identifier: GPL-2.0-or-later
mod app;
mod eqcurve;
mod hw;
mod mech;
mod model;
mod sim;
mod widgets;

use app::{App, Tab};

struct Runner {
    app: App,
    screenshot: Option<String>,
    requested: bool,
}

impl eframe::App for Runner {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.app.ui(ctx);
        if let Some(path) = self.screenshot.clone() {
            if self.app.frame >= 120 && !self.requested {
                self.requested = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot);
            }
            let shot = ctx.input(|i| {
                i.raw.events.iter().find_map(|e| match e {
                    egui::Event::Screenshot { image, .. } => Some(image.clone()),
                    _ => None,
                })
            });
            if let Some(img) = shot {
                let [w, h] = img.size;
                let mut buf = Vec::with_capacity(w * h * 4);
                for p in &img.pixels {
                    buf.extend_from_slice(&[p.r(), p.g(), p.b(), 255]);
                }
                if let Some(dir) = std::path::Path::new(&path).parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                match image::save_buffer(&path, &buf, w as u32, h as u32, image::ExtendedColorType::Rgba8) {
                    Ok(()) => eprintln!("saved screenshot to {}", path),
                    Err(e) => eprintln!("could not save screenshot: {}", e),
                }
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }
}

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let mut screenshot = None;
    let mut tab = Tab::Mixes;
    let mut mk3 = false;
    let mut hardware = true;
    let mut k = 1;
    while k < args.len() {
        match args[k].as_str() {
            "--screenshot" if k + 1 < args.len() => {
                screenshot = Some(args[k + 1].clone());
                k += 1;
            }
            "--tab" if k + 1 < args.len() => {
                tab = match args[k + 1].as_str() {
                    "inputs" => Tab::Inputs,
                    "outputs" => Tab::Outputs,
                    "channel" => Tab::ChannelDyn,
                    "eq" => Tab::Eq,
                    "reverb" => Tab::Reverb,
                    "clock" => Tab::Clock,
                    _ => Tab::Mixes,
                };
                k += 1;
            }
            "--828mk3" => mk3 = true,
            "--simulate" => hardware = false,
            _ => {}
        }
        k += 1;
    }
    // screenshots are taken of the simulated console so they don't depend on a card
    if screenshot.is_some() {
        hardware = false;
    }
    let mut app = App::new(hardware);
    if mk3 {
        app.state = model::State::mk3().with_demo_mix();
        app.sim = sim::Sim::new(&app.state);
    }
    app.tab = tab;
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("CueMix Studio")
            .with_inner_size([1280.0, 760.0])
            .with_min_inner_size([900.0, 560.0]),
        ..Default::default()
    };
    eframe::run_native(
        "CueMix Studio",
        options,
        Box::new(move |cc| {
            let mut v = egui::Visuals::dark();
            v.panel_fill = widgets::BG;
            v.window_fill = widgets::PANEL;
            v.extreme_bg_color = widgets::WELL;
            v.selection.bg_fill = widgets::ACCENT.gamma_multiply(0.6);
            cc.egui_ctx.set_visuals(v);
            Ok(Box::new(Runner { app, screenshot, requested: false }))
        }),
    )
}
