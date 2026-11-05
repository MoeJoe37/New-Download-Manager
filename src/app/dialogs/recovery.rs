use crate::app::{App, dlu, egui, explore, i18n};
use crate::engine::{Status, recovery::RecoveryAction};

impl App {
    pub(super) fn dialog_recovery(&mut self, ctx: &egui::Context, ui: &mut egui::Ui, id: u64) -> bool {
        let Some(job) = self.core.find(id) else { return false };
        let info = job.info().clone();
        let Status::Error(error) = &info.status else { return false };
        if job.running() { return false }
        let action = RecoveryAction::for_download(&info);
        let mut d = dlu::D::new(ui);
        d.at(8.0, 8.0, 319.0, 47.0, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| { ui.label(i18n::line(&i18n::tr_msg_os(error))); });
        });
        d.text(8.0, 61.0, 319.0, 42.0, action.guidance());
        let primary = d.button(8.0, 116.0, 151.0, 16.0, action.label(), true);
        let retry = !info.fed && !matches!(action, RecoveryAction::Retry | RecoveryAction::Restart)
            && d.button(167.0, 116.0, 70.0, 16.0, "Retry", true);
        let cancel = d.button(245.0, 116.0, 78.0, 16.0, "Cancel", true);
        if retry { self.resume(&[id]); }
        if primary {
            match action {
                RecoveryAction::Retry => self.resume(&[id]),
                RecoveryAction::Restart => self.redownload(&[id]),
                RecoveryAction::RefreshAddress => self.refresh_address(id),
                RecoveryAction::Credentials => self.file_properties(id),
                RecoveryAction::ProxySettings => self.options(5),
                RecoveryAction::MediaTools => self.set_ytdlp(ctx),
                RecoveryAction::OpenFolder => explore(&info.folder),
            }
        }
        !(primary || retry || cancel)
    }
}
