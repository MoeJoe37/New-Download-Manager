use crate::app::{dlu, egui, explore, style, tr, App, Dialog, Relaxed};
use crate::appearance::themes::{self, Base};

impl App {
    pub(in crate::app) fn themes_menu(&mut self, ui: &mut egui::Ui) {
        let (selected, ndm) = {
            let s = self.core.settings();
            (s.theme.clone(), s.ndm_style)
        };
        for (label, n) in [("Modern style", false), ("NDM Style", true)] {
            if ui
                .radio(selected.is_empty() && ndm == n, tr(label))
                .clicked()
            {
                themes::activate(None);
                {
                    let mut s = self.core.settings();
                    s.theme.clear();
                    s.ndm_style = n;
                }
                self.appearance_changed(ui.ctx());
                ui.close_menu();
            }
        }
        let installed = themes::list(&self.core.config_dir().join("themes"));
        if !installed.is_empty() {
            ui.separator();
        }
        for (folder, name) in installed {
            if ui.radio(selected == folder, name).clicked() {
                match themes::load_selected(&self.core.config_dir().join("themes"), &folder) {
                    Ok(theme) => self.use_theme(ui.ctx(), folder, theme),
                    Err(error) => self.theme_error(error),
                }
                ui.close_menu();
            }
        }
        ui.separator();
        if ui.button(tr("Install theme...")).clicked() {
            ui.close_menu();
            if let Some(file) = rfd::FileDialog::new()
                .set_title(crate::i18n::tr_os("Install theme"))
                .add_filter("JSON", &["json"])
                .pick_file()
            {
                match themes::install(&file, &self.core.config_dir().join("themes")) {
                    Ok((folder, theme)) => self.use_theme(ui.ctx(), folder, theme),
                    Err(error) => self.theme_error(error),
                }
            }
        }
        if ui
            .add_enabled(!selected.is_empty(), egui::Button::new(tr("Reload theme")))
            .clicked()
        {
            match themes::load_selected(&self.core.config_dir().join("themes"), &selected) {
                Ok(theme) => self.use_theme(ui.ctx(), selected, theme),
                Err(error) => self.theme_error(error),
            }
            ui.close_menu();
        }
        if ui.button(tr("Create theme...")).clicked() {
            let light = ui.ctx().style_of(egui::Theme::Light);
            let dark = ui.ctx().style_of(egui::Theme::Dark);
            match themes::create(
                &self.core.config_dir().join("themes"),
                if ndm { Base::Ndm } else { Base::Modern },
                [&light, &dark],
            ) {
                Ok(path) => explore(path),
                Err(error) => self.theme_error(error),
            }
            ui.close_menu();
        }
        if ui.button(tr("Open themes folder")).clicked() {
            match std::fs::create_dir_all(self.core.config_dir().join("themes")) {
                Ok(()) => explore(self.core.config_dir().join("themes")),
                Err(e) => self.theme_error(e.to_string()),
            }
            ui.close_menu();
        }
    }

    fn theme_error(&mut self, error: String) {
        self.open(Dialog::Info {
            title: "Could not load theme".into(),
            text: error,
        });
    }

    fn use_theme(&mut self, ctx: &egui::Context, folder: String, theme: themes::Loaded) {
        {
            let mut s = self.core.settings();
            s.theme = folder;
            s.ndm_style = theme.file.base == Base::Ndm;
        }
        themes::activate(Some(theme));
        self.appearance_changed(ctx);
    }

    fn appearance_changed(&mut self, ctx: &egui::Context) {
        dlu::NDM_STYLE.store(self.core.settings().ndm_style, Relaxed);
        themes::clear_cache(ctx);
        self.toolbar = None;
        self.cat_pics.clear();
        self.core.save();
        style(ctx, self.dark);
        ctx.request_repaint();
    }
}
