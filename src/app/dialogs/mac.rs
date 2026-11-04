use super::*;

impl App {
    pub(super) fn dialog_mac_capture(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        services: &[(String, String)],
        selected: &mut Option<usize>,
        pending: &mut Option<Receiver<Result<(), String>>>,
        message: &mut String,
    ) -> bool {
        if let Some(rx) = pending {
            match rx.try_recv() {
                Ok(result) => {
                    *message = result.err().unwrap_or_else(|| {
                        tr_os(if mac_capture::has_journal() { "HTTP capture is enabled for this session." } else { "Web proxy settings restored." })
                    });
                    *pending = None;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    *message = tr_os("The network preferences operation stopped unexpectedly.");
                    *pending = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => ctx.request_repaint_after(Duration::from_millis(150)),
            }
        }
        ui.label(tr("Capture plain HTTP on one network service until NDM quits. The browser extension still handles HTTPS."));
        ui.label(tr("macOS may require an administrator to enable or restore this setting."));
        let recovery = mac_capture::has_journal();
        let busy = pending.is_some();
        let label = selected.and_then(|index| services.get(index)).map(|(_, name)| i18n::shown(name))
            .unwrap_or_else(|| tr("Choose a network service"));
        ui.add_enabled_ui(!busy && !recovery, |ui| {
            egui::ComboBox::from_id_salt("mac-capture-service").selected_text(label).show_ui(ui, |ui| {
                for (index, (_, name)) in services.iter().enumerate() {
                    ui.selectable_value(selected, Some(index), i18n::shown(name));
                }
            });
        });
        let enable = ui.add_enabled(!busy && !recovery && selected.is_some(),
            egui::Button::new(tr("Enable for this session"))).clicked();
        let restore = ui.add_enabled(!busy && recovery, egui::Button::new(tr("Restore web proxy"))).clicked();
        if enable || restore {
            let id = selected.and_then(|index| services.get(index)).map(|(id, _)| id.clone());
            let core = self.core.clone();
            let (tx, rx) = channel();
            std::thread::spawn(move || {
                let result = if enable {
                    id.ok_or_else(|| "Choose a network service.".to_string())
                        .and_then(|id| mac_capture::enable(&id, &core.proxy_ready))
                } else {
                    mac_capture::restore()
                };
                let _ = tx.send(result);
            });
            message.clear();
            *pending = Some(rx);
        }
        if busy { ui.spinner(); }
        if !message.is_empty() { ui.label(tr(message)); }
        !ui.button(tr("Close")).clicked()
    }
}
