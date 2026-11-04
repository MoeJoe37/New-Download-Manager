use super::*;

impl App {
    pub(super) fn dialog_problem_report(ui: &mut egui::Ui, text: &mut String) -> bool {
        ui.label(tr("Review and edit the report before copying it. Paste it into the issue form when you are ready."));
        egui::ScrollArea::vertical().max_height((ui.available_height() - 44.0).max(80.0)).show(ui, |ui| {
            ui.add(egui::TextEdit::multiline(text).desired_rows(13).desired_width(f32::INFINITY));
        });
        let mut close = false;
        ui.horizontal(|ui| {
            if ui.button(tr("Copy report")).clicked() { ui.ctx().copy_text(text.clone()); }
            if ui.button(tr("Open issue form")).clicked() { App::report_problem(); }
            close = ui.button(tr("Close")).clicked();
        });
        !close
    }
}
