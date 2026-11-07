use crate::app::{egui, tr, win, App, Dialog, ViewportCommand};
use crate::appearance::icons;

impl App {
    pub(in crate::app) fn icon_menu(&mut self, ui: &mut egui::Ui, app: bool) {
        let (custom, mode) = {
            let s = self.core.settings();
            (
                if app {
                    s.app_icon.clone()
                } else {
                    s.tray_custom_icon.clone()
                },
                s.tray_icon,
            )
        };
        let default = if app {
            custom.is_empty()
        } else {
            mode != 3 && mode != 4
        };
        if ui.radio(default, tr("Default")).clicked() {
            if app {
                self.core.settings().app_icon.clear();
            } else {
                self.core.settings().tray_icon = 2;
            }
            self.icons_changed(ui.ctx(), app);
            ui.close_menu();
        }
        if !app && !custom.is_empty() && ui.radio(mode == 4, tr("Custom icon")).clicked() {
            self.core.settings().tray_icon = 4;
            self.icons_changed(ui.ctx(), false);
            ui.close_menu();
        }
        if app && !custom.is_empty() {
            let _ = ui.radio(true, tr("Custom icon"));
        }
        if ui.button(tr("Choose icon...")).clicked() {
            ui.close_menu();
            if let Some(file) = rfd::FileDialog::new()
                .set_title(crate::i18n::tr_os("Choose icon"))
                .add_filter(
                    crate::i18n::tr_os("Images"),
                    &["png", "jpg", "jpeg", "webp", "bmp", "gif", "ico", "svg"],
                )
                .add_filter(crate::i18n::tr_os("All files"), &["*"])
                .pick_file()
            {
                match icons::import(self.core.config_dir(), &file, app) {
                    Ok(name) => {
                        {
                            let mut s = self.core.settings();
                            if app {
                                s.app_icon = name;
                            } else {
                                s.tray_custom_icon = name;
                                s.tray_icon = 4;
                            }
                        }
                        self.icons_changed(ui.ctx(), app);
                    }
                    Err(error) => self.open(Dialog::Info {
                        title: "Could not use this icon".into(),
                        text: error,
                    }),
                }
            }
        }
        if !app {
            ui.separator();
            if ui.radio(mode == 3, tr("Don't show")).clicked() {
                self.core.settings().tray_icon = 3;
                self.icons_changed(ui.ctx(), false);
                ui.close_menu();
            }
        }
    }

    fn icons_changed(&mut self, ctx: &egui::Context, app: bool) {
        let (a, t) = {
            let s = self.core.settings();
            (s.app_icon.clone(), s.tray_custom_icon.clone())
        };
        icons::initialize(self.core.config_dir(), &a, &t);
        self.core.save();
        if app {
            self.apply_app_icon(ctx, true);
        } else {
            self.refresh_tray(ctx);
        }
        ctx.request_repaint();
    }

    pub(in crate::app) fn apply_app_icon(&mut self, ctx: &egui::Context, launchers: bool) {
        let icon = icons::viewport();
        let ids = ctx.input(|i| i.raw.viewports.keys().copied().collect::<Vec<_>>());
        for id in ids {
            ctx.send_viewport_cmd_to(id, ViewportCommand::Icon(Some(icon.clone())));
        }
        let px = icons::app();
        if self.drop_shown {
            win::drop_target(false, &px);
            win::drop_target(true, &px);
        }
        let name = self.core.settings().app_icon.clone();
        if launchers || cfg!(target_os = "macos") {
            match icons::launcher_icon(self.core.config_dir(), &name) {
                Ok(path) => win::set_application_icon(&px, &path, !name.is_empty()),
                Err(error) => self.open(Dialog::Info {
                    title: "Could not update app icon".into(),
                    text: error,
                }),
            }
        }
    }
}
