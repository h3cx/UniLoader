//! Native desktop dialogs. Runs in the binary's isolated `__dialog` process.
use crate::preview::{Preview, fit_scale};
use anyhow::{Context, Result, ensure};
use eframe::egui;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex, mpsc},
};

#[derive(Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase")]
enum Request {
    Destination {
        component: String,
        projects: Vec<PathBuf>,
    },
    Compare {
        incoming: String,
        candidates: Vec<Candidate>,
    },
    File {
        title: String,
        paths: Vec<PathBuf>,
    },
}

#[derive(Deserialize)]
struct Candidate {
    label: String,
    path: PathBuf,
    raw: String,
    differences: Vec<String>,
}

#[derive(Clone, PartialEq)]
enum Destination {
    Global,
    Project(PathBuf),
}

enum Dialog {
    Destination {
        component: String,
        projects: Vec<PathBuf>,
        selected: Destination,
        remember: bool,
        browse: Option<mpsc::Receiver<Option<PathBuf>>>,
        browse_message: Option<String>,
    },
    Compare {
        incoming: Preview,
        candidates: Vec<Candidate>,
        previews: Vec<Preview>,
        selected: usize,
    },
    File {
        title: String,
        paths: Vec<PathBuf>,
        selected: Option<usize>,
    },
}

impl Dialog {
    fn from_request(request: Value) -> Result<Self> {
        Ok(
            match serde_json::from_value(request).context("invalid desktop dialog request")? {
                Request::Destination {
                    component,
                    projects,
                } => {
                    let selected = projects
                        .first()
                        .cloned()
                        .map(Destination::Project)
                        .unwrap_or(Destination::Global);
                    Self::Destination {
                        component,
                        projects,
                        selected,
                        remember: false,
                        browse: None,
                        browse_message: None,
                    }
                }
                Request::Compare {
                    incoming,
                    candidates,
                } => {
                    ensure!(!candidates.is_empty(), "no footprint candidates to compare");
                    let previews = candidates
                        .iter()
                        .map(|c| Preview::from_raw(&c.raw))
                        .collect();
                    Self::Compare {
                        incoming: Preview::from_raw(&incoming),
                        candidates,
                        previews,
                        selected: 0,
                    }
                }
                Request::File { title, paths } => {
                    ensure!(!paths.is_empty(), "no model files to choose from");
                    Self::File {
                        title,
                        paths,
                        selected: None,
                    }
                }
            },
        )
    }

    fn file_response(paths: &[PathBuf], selected: Option<usize>) -> Option<Value> {
        selected
            .and_then(|index| paths.get(index))
            .map(|path| json!({"path": path}))
    }

    /// Returns a response only after an explicit action. The outer Option is
    /// completion; an inner None means Later, leaving the import pending.
    fn show(&mut self, ui: &mut egui::Ui) -> Option<Option<Value>> {
        ui.heading("UniLoader");
        ui.add_space(8.0);
        let mut result = None;
        match self {
            Self::Destination {
                component,
                projects,
                selected,
                remember,
                browse,
                browse_message,
            } => {
                ui.label(format!("Import: {component}"));
                ui.label("Choose the component's destination.");
                ui.weak(
                    "Detected KiCad projects are suggestions; check the path before importing.",
                );
                ui.add_space(8.0);
                egui::ScrollArea::vertical()
                    .max_height(230.0)
                    .id_salt("projects")
                    .show(ui, |ui| {
                        for project in projects.iter() {
                            ui.radio_value(
                                selected,
                                Destination::Project(project.clone()),
                                project.display().to_string(),
                            );
                        }
                        ui.radio_value(selected, Destination::Global, "Global libraries");
                    });
                if let Some(receiver) = browse {
                    match receiver.try_recv() {
                        Ok(path) => {
                            if let Some(path) = path {
                                if path.extension().is_some_and(|ext| ext == "kicad_pro")
                                    && path.is_file()
                                {
                                    if !projects.contains(&path) {
                                        projects.push(path.clone());
                                    }
                                    *selected = Destination::Project(path);
                                    *browse_message = None;
                                } else {
                                    *browse_message =
                                        Some("Select an existing .kicad_pro file.".into());
                                }
                            } else {
                                *browse_message = Some("No project selected. You can retry Browse or select a destination above.".into());
                            }
                            *browse = None;
                        }
                        Err(mpsc::TryRecvError::Disconnected) => {
                            *browse_message = Some("The desktop file picker is unavailable. Select a detected project or global libraries.".into());
                            *browse = None;
                        }
                        Err(mpsc::TryRecvError::Empty) => {}
                    }
                }
                if ui
                    .add_enabled(browse.is_none(), egui::Button::new("Browse for project…"))
                    .clicked()
                {
                    let (sender, receiver) = mpsc::channel();
                    let ctx = ui.ctx().clone();
                    std::thread::spawn(move || {
                        let path = rfd::FileDialog::new()
                            .set_title("Select KiCad project")
                            .add_filter("KiCad project", &["kicad_pro"])
                            .pick_file();
                        let _ = sender.send(path);
                        ctx.request_repaint();
                    });
                    *browse = Some(receiver);
                    *browse_message = None;
                }
                if browse.is_some() {
                    ui.weak("Waiting for the desktop file picker…");
                }
                if let Some(message) = browse_message {
                    ui.label(message.as_str());
                }
                ui.add_space(8.0);
                ui.checkbox(
                    remember,
                    "Remember for this session while this project remains open",
                );
                ui.separator();
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .add_enabled(browse.is_none(), egui::Button::new("Import"))
                        .clicked()
                    {
                        let project = match selected {
                            Destination::Global => None,
                            Destination::Project(path) => Some(path.clone()),
                        };
                        result = Some(Some(
                            json!({"project":project,"remember":remember,"skip":false}),
                        ));
                    }
                    if ui.button("Skip component").clicked() {
                        result = Some(Some(json!({"project":null,"remember":false,"skip":true})));
                    }
                });
            }
            Self::Compare {
                incoming,
                candidates,
                previews,
                selected,
            } => {
                ui.label("Compare footprint candidates");
                egui::ComboBox::from_id_salt("candidate")
                    .width(ui.available_width().min(800.0))
                    .selected_text(&candidates[*selected].label)
                    .show_ui(ui, |ui| {
                        for (index, candidate) in candidates.iter().enumerate() {
                            ui.selectable_value(selected, index, &candidate.label);
                        }
                    });
                ui.weak(candidates[*selected].path.display().to_string());
                let right = &previews[*selected];
                let pane_width = (ui.available_width() - ui.spacing().item_spacing.x) / 2.0;
                let scale = fit_scale(incoming, right, pane_width, 280.0);
                ui.columns(2, |columns| {
                    columns[0].label("Incoming footprint");
                    incoming.paint(&mut columns[0], scale);
                    columns[1].label("Existing candidate");
                    right.paint(&mut columns[1], scale);
                });
                ui.weak("Both previews use the same scale. Drill holes are simplified; text and 3D geometry are omitted. Unsupported shapes are listed below.");
                egui::ScrollArea::vertical()
                    .max_height(170.0)
                    .id_salt("differences")
                    .show(ui, |ui| {
                        for difference in &candidates[*selected].differences {
                            ui.label(format!("• {difference}"));
                        }
                        for warning in incoming.warnings.iter().chain(right.warnings.iter()) {
                            ui.colored_label(egui::Color32::YELLOW, warning);
                        }
                    });
                ui.separator();
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Use selected existing").clicked() {
                        result = Some(Some(json!({"action":"reuse","index":selected})));
                    }
                    if ui.button("Import separately").clicked() {
                        result = Some(Some(json!({"action":"separate"})));
                    }
                    if ui.button("Skip component").clicked() {
                        result = Some(Some(json!({"action":"skip"})));
                    }
                });
            }
            Self::File {
                title,
                paths,
                selected,
            } => {
                ui.label(title.as_str());
                ui.weak("Multiple model files match. Select the file to use.");
                egui::ScrollArea::vertical()
                    .max_height(350.0)
                    .id_salt("models")
                    .show(ui, |ui| {
                        for (index, path) in paths.iter().enumerate() {
                            ui.selectable_value(selected, Some(index), path.display().to_string());
                        }
                    });
                ui.separator();
                if ui
                    .add_enabled(selected.is_some(), egui::Button::new("Use selected file"))
                    .clicked()
                {
                    result = Self::file_response(paths, *selected).map(Some);
                }
            }
        }
        ui.add_space(6.0);
        if ui.button("Later").clicked() {
            result = Some(None);
        }
        result
    }
}

struct App {
    dialog: Dialog,
    response: Arc<Mutex<Option<Value>>>,
}
impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                if let Some(response) = self.dialog.show(ui) {
                    if let Ok(mut output) = self.response.lock() {
                        *output = response;
                    }
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
        });
    }
}

pub fn run(request: Value) -> Result<Option<Value>> {
    let dialog = Dialog::from_request(request)?;
    let compare = matches!(dialog, Dialog::Compare { .. });
    let response = Arc::new(Mutex::new(None));
    let app_response = response.clone();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_app_id("uniloader")
            .with_inner_size(if compare {
                [700.0, 700.0]
            } else {
                [700.0, 490.0]
            })
            .with_min_inner_size([660.0, 400.0]),
        ..Default::default()
    };
    eframe::run_native(
        "UniLoader — component import",
        options,
        Box::new(move |_cc| {
            Ok(Box::new(App {
                dialog,
                response: app_response,
            }))
        }),
    )
    .map_err(|error| anyhow::anyhow!("native desktop dialog failed: {error}"))?;
    let result = response
        .lock()
        .map_err(|_| anyhow::anyhow!("dialog response lock failed"))?
        .clone();
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn models_require_explicit_selection() {
        let dialog = Dialog::from_request(
            json!({"mode":"file","title":"Choose model","paths":["/a.step","/b.step"]}),
        )
        .unwrap();
        let Dialog::File {
            paths, selected, ..
        } = dialog
        else {
            panic!("wrong dialog")
        };
        assert!(selected.is_none());
        assert!(Dialog::file_response(&paths, selected).is_none());
        assert_eq!(
            Dialog::file_response(&paths, Some(1)),
            Some(json!({"path":"/b.step"}))
        );
        assert!(Dialog::file_response(&paths, Some(2)).is_none());
    }

    #[test]
    fn rejects_invalid_requests() {
        for request in [
            json!({"mode":"other"}),
            json!({"mode":"file","title":"Choose","paths":[]}),
            json!({"mode":"compare","incoming":"","candidates":[]}),
        ] {
            assert!(Dialog::from_request(request).is_err());
        }
    }

    fn frame(
        dialog: &mut Dialog,
        ctx: &egui::Context,
        events: Vec<egui::Event>,
    ) -> (egui::FullOutput, Option<Option<Value>>) {
        let mut answer = None;
        let output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(900.0, 900.0),
                )),
                events,
                ..Default::default()
            },
            |ui| {
                egui::CentralPanel::default().show(ui, |ui| {
                    answer = dialog.show(ui);
                });
            },
        );
        (output, answer)
    }

    fn text_center(shape: &egui::epaint::Shape, label: &str) -> Option<egui::Pos2> {
        match shape {
            egui::epaint::Shape::Text(text) if text.galley.text() == label => {
                Some(text.pos + text.galley.rect.center().to_vec2())
            }
            egui::epaint::Shape::Vec(shapes) => {
                shapes.iter().find_map(|shape| text_center(shape, label))
            }
            _ => None,
        }
    }

    fn click(dialog: &mut Dialog, ctx: &egui::Context, label: &str) -> Option<Option<Value>> {
        let (mut output, _) = frame(dialog, ctx, vec![]);
        let pos = output
            .shapes
            .iter()
            .find_map(|shape| text_center(&shape.shape, label))
            .unwrap_or_else(|| panic!("missing UI label: {label}"));
        output.textures_delta.clear();
        let mut answer = None;
        for pressed in [true, false] {
            let (mut output, response) = frame(
                dialog,
                ctx,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: Default::default(),
                    },
                ],
            );
            output.textures_delta.clear();
            if response.is_some() {
                answer = response;
            }
        }
        answer
    }

    #[test]
    fn actual_buttons_produce_expected_responses() {
        let destination = json!({"mode":"destination","component":"Example","projects":[]});
        let compare = json!({"mode":"compare","incoming":"(footprint a)","candidates":[{"label":"Existing","path":"/x.kicad_mod","raw":"(footprint b)","differences":[]}]});
        for (request, label, expected) in [
            (
                destination.clone(),
                "Import",
                Some(json!({"project":null,"remember":false,"skip":false})),
            ),
            (
                destination.clone(),
                "Skip component",
                Some(json!({"project":null,"remember":false,"skip":true})),
            ),
            (destination, "Later", None),
            (
                compare.clone(),
                "Use selected existing",
                Some(json!({"action":"reuse","index":0})),
            ),
            (
                compare,
                "Import separately",
                Some(json!({"action":"separate"})),
            ),
        ] {
            let mut dialog = Dialog::from_request(request).unwrap();
            assert_eq!(
                click(&mut dialog, &egui::Context::default(), label),
                Some(expected)
            );
        }
    }

    #[test]
    fn model_confirmation_stays_disabled_until_selection() {
        let mut dialog = Dialog::from_request(
            json!({"mode":"file","title":"Choose model","paths":["/a.step","/b.step"]}),
        )
        .unwrap();
        let ctx = egui::Context::default();
        assert_eq!(click(&mut dialog, &ctx, "Use selected file"), None);
        assert_eq!(click(&mut dialog, &ctx, "/b.step"), None);
        assert_eq!(
            click(&mut dialog, &ctx, "Use selected file"),
            Some(Some(json!({"path":"/b.step"})))
        );
    }

    #[test]
    fn idle_dialogs_do_not_consent() {
        let requests = [
            json!({"mode":"destination","component":"Example Ω","projects":["/tmp/board.kicad_pro"]}),
            json!({"mode":"file","title":"Choose model","paths":["/a.step","/b.step"]}),
            json!({"mode":"compare","incoming":"(footprint a)","candidates":[{"label":"Existing","path":"/x.kicad_mod","raw":"(footprint b)","differences":["Pad locations differ"]}]}),
        ];
        for request in requests {
            let mut dialog = Dialog::from_request(request).unwrap();
            let ctx = egui::Context::default();
            let mut output = ctx.run_ui(Default::default(), |ui| {
                egui::CentralPanel::default().show(ui, |ui| {
                    assert!(dialog.show(ui).is_none());
                });
            });
            output.textures_delta.clear();
        }
    }
}
