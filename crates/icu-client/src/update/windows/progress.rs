use super::*;
#[derive(Default)]
struct State {
    message: String,
    done: bool,
    error: Option<String>,
    handed_off: bool,
}
#[derive(Clone, Default)]
pub(super) struct Status(Arc<Mutex<State>>);
impl Status {
    pub fn set(&self, message: &str) {
        self.0.lock().unwrap().message = message.into();
    }
    pub fn handed_off(&self) {
        self.0.lock().unwrap().handed_off = true;
    }
    fn finish(&self, result: &Result<(), String>) {
        let mut state = self.0.lock().unwrap();
        state.done = true;
        state.error = result.as_ref().err().cloned();
        drop(state);
        #[cfg(feature = "gui")]
        crate::ui::native::dispatch(result.is_err());
    }
}
fn work(path: PathBuf, status: Status) -> Result<(), String> {
    let result = broker(&path, &status);
    if let Err(error) = &result {
        if !status.0.lock().unwrap().handed_off {
            reply(Reply::Error(error.clone()));
        }
    }
    if let Some(dir) = path.parent() {
        let _ = fs::write(dir.join("finished"), b"finished\n");
    }
    status.finish(&result);
    result
}
#[cfg(feature = "gui")]
pub(super) fn run(path: PathBuf) -> Result<(), String> {
    struct Progress {
        status: Status,
        focused: bool,
    }
    impl eframe::App for Progress {
        fn ui(&mut self, ui: &mut egui::Ui, _: &mut eframe::Frame) {
            let state = self.status.0.lock().unwrap();
            ui.ctx().request_repaint_after(Duration::from_millis(100));
            if !state.done && ui.ctx().input(|i| i.viewport().close_requested()) {
                ui.ctx()
                    .send_viewport_cmd(egui::ViewportCommand::CancelClose);
            }
            egui::CentralPanel::default().show(ui, |ui| {
                ui.add_space(14.);
                ui.heading("ICU aktualisieren");
                ui.add_space(16.);
                if let Some(error) = &state.error {
                    ui.label(error);
                    ui.add_space(16.);
                    if crate::ui::design::button(
                        ui,
                        crate::ui::widgets::ButtonKind::Neutral,
                        "",
                        "Schließen",
                    )
                    .clicked()
                    {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    if !self.focused {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Focus);
                        self.focused = true;
                    }
                } else {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(&state.message);
                    });
                }
            });
        }
    }
    let status = Status::default();
    let returned = status.clone();
    crate::ui::native::run_native(
        "ICU Update",
        crate::ui::native_options(
            egui::ViewportBuilder::default()
                .with_inner_size([470., 210.])
                .with_resizable(false),
        ),
        Box::new(move |cc| {
            crate::ui::fonts::install(&cc.egui_ctx);
            crate::ui::theme::apply(&cc.egui_ctx, "system");
            let worker = status.clone();
            std::thread::spawn(move || {
                let _ = work(path, worker);
            });
            Ok(Box::new(Progress {
                status,
                focused: false,
            }))
        }),
    )
    .map_err(|e| e.to_string())?;
    let state = returned.0.lock().unwrap();
    if !state.done {
        Err("Das Update-Fenster wurde vorzeitig beendet.".into())
    } else if let Some(error) = &state.error {
        Err(error.clone())
    } else {
        Ok(())
    }
}
#[cfg(not(feature = "gui"))]
pub(super) fn run(path: PathBuf) -> Result<(), String> {
    work(path, Status::default())
}
