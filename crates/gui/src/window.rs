use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use eframe::egui::{self, Color32, RichText};
use usbridge::ipc::DeviceState;
use usbridge::paths::Paths;

use crate::model::{self, Action, Model};

const GREEN: Color32 = Color32::from_rgb(0x2e, 0xa0, 0x43);
const AMBER: Color32 = Color32::from_rgb(0xd2, 0x99, 0x22);

pub fn run() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("usbridge")
            .with_inner_size([560.0, 420.0])
            .with_min_inner_size([420.0, 240.0]),
        ..Default::default()
    };
    eframe::run_native(
        "usbridge",
        options,
        Box::new(|cc| {
            let ctx = cc.egui_ctx.clone();
            let (model, actions) = model::spawn(Paths::default(), move || ctx.request_repaint());
            Ok(Box::new(Window {
                model,
                actions,
                new_server: String::new(),
            }))
        }),
    )
}

struct Window {
    model: Arc<Mutex<Model>>,
    actions: Sender<Action>,
    new_server: String,
}

impl eframe::App for Window {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let model = self.model.clone();
        let m = model.lock().unwrap();

        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| match (&m.agent, &m.last_error) {
                (_, Some(e)) => {
                    ui.colored_label(Color32::RED, e);
                }
                (Some(Err(e)), _) => {
                    ui.colored_label(Color32::RED, e);
                }
                (None, _) => {
                    ui.label("Connecting to the agent…");
                }
                (Some(Ok(_)), _) => {
                    dot(ui, GREEN, true);
                    ui.label(
                        "Agent running. Attached devices stay attached when this window closes.",
                    );
                }
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                let edit = ui.add(
                    egui::TextEdit::singleline(&mut self.new_server)
                        .hint_text("server, e.g. usbgw.local")
                        .desired_width(240.0),
                );
                let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if (ui.button("Add server").clicked() || enter)
                    && !self.new_server.trim().is_empty()
                {
                    let _ = self
                        .actions
                        .send(Action::AddServer(self.new_server.trim().to_owned()));
                    self.new_server.clear();
                }
            });
            ui.separator();

            let servers = m.servers();
            if servers.is_empty() && matches!(m.agent, Some(Ok(_))) {
                ui.label("Add the server that exports your USB devices.");
            }
            egui::ScrollArea::vertical().show(ui, |ui| {
                for server in &servers {
                    self.server_section(ui, &m, server);
                    ui.add_space(8.0);
                }
            });
        });

        // The model thread repaints on changes; this keeps "retrying" fresh.
        ctx.request_repaint_after(Duration::from_secs(2));
    }
}

impl Window {
    fn server_section(&self, ui: &mut egui::Ui, m: &Model, server: &str) {
        ui.horizontal(|ui| {
            ui.heading(server);
            if ui
                .small_button("Remove")
                .on_hover_text("Forget this server and detach its devices")
                .clicked()
            {
                let _ = self.actions.send(Action::RemoveServer(server.to_owned()));
            }
        });
        match m.exported.get(server) {
            Some(Err(e)) => {
                ui.colored_label(AMBER, format!("Cannot reach server: {e}"));
            }
            None => {
                ui.label("Loading…");
            }
            Some(Ok(_)) => {}
        }

        let rows = m.rows(server);
        if rows.is_empty() {
            if let Some(Ok(_)) = m.exported.get(server) {
                ui.label("This server exports no devices right now.");
            }
            return;
        }
        egui::Grid::new(server)
            .striped(true)
            .num_columns(5)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                for row in rows {
                    let (filled, color, note) = match row.status {
                        Some(s) if s.state == DeviceState::Attached => {
                            (true, GREEN, "attached".to_owned())
                        }
                        Some(s) => (
                            true,
                            AMBER,
                            s.error.clone().unwrap_or_else(|| "connecting".into()),
                        ),
                        None => (false, ui.visuals().weak_text_color(), String::new()),
                    };
                    dot(ui, color, filled);
                    ui.label(RichText::new(row.id).strong());
                    match row.exported {
                        Some(d) => ui.label(format!(
                            "{:04x}:{:04x}  {}",
                            d.device.vendor_id, d.device.product_id, d.device.speed
                        )),
                        None => ui.weak("not exported now"),
                    };
                    ui.label(note);
                    let (label, action) = match row.status {
                        Some(_) => (
                            "Detach",
                            Action::Detach {
                                server: server.to_owned(),
                                id: row.id.to_owned(),
                            },
                        ),
                        None => (
                            "Attach",
                            Action::Attach {
                                server: server.to_owned(),
                                id: row.id.to_owned(),
                            },
                        ),
                    };
                    if ui.button(label).clicked() {
                        let _ = self.actions.send(action);
                    }
                    ui.end_row();
                }
            });
    }
}

fn dot(ui: &mut egui::Ui, color: Color32, filled: bool) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
    if filled {
        ui.painter().circle_filled(rect.center(), 4.5, color);
    } else {
        ui.painter()
            .circle_stroke(rect.center(), 4.0, egui::Stroke::new(1.5_f32, color));
    }
}
