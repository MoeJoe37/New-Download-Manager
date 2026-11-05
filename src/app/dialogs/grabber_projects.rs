use crate::app::dialogs::Dialog;
use crate::app::{core, dlu, egui, i18n, tr, App};

impl App {
    pub(super) fn dialog_grab_save(
        &mut self,
        ui: &mut egui::Ui,
        name: &mut String,
        orig: &mut Option<String>,
        quiet: &mut bool,
        proj: &mut Option<Box<core::settings::GrabProject>>,
        sched: &mut bool,
    ) -> bool {
        let mut d = dlu::D::new(ui);
        d.label(
            7.0,
            6.0,
            204.0,
            9.0,
            "Save this project with the following name",
        );
        d.edit(7.0, 19.0, 204.0, 14.0, name, true);
        if orig.is_none() {
            d.check(
                18.0,
                35.0,
                185.0,
                10.0,
                quiet,
                "Don't show this dialog if the name is unique",
            );
        }
        let ok = d.button(53.0, 54.0, 50.0, 14.0, "OK", !name.trim().is_empty());
        if ok {
            if orig.is_none() {
                self.core.settings().grab_quiet = *quiet;
            }
            let p = proj.take().map(|p| *p);
            let n = name.trim().to_string();
            let taken = orig.is_none()
                && p.as_ref().is_some_and(|p| p.opts.name.trim() != n)
                && self
                    .core
                    .settings()
                    .grab_projects
                    .iter()
                    .any(|x| x.opts.name == n);
            if taken {
                // 20615: another project already has this name
                *proj = p.map(Box::new);
                self.open(Dialog::Info {
                    title: "Saving the project of NDM Grabber".into(),
                    text: i18n::trf_os(
                        "The project with \"%s\" name already exists. Please select another name.",
                        &[&n],
                    ),
                });
                return true;
            }
            if p.is_some() {
                self.grab_saved_as = Some(n.clone());
            }
            self.save_project(&n, orig.as_deref(), p);
            if *sched {
                self.project_cmd(1, &n);
            }
        }
        !(ok || d.button(113.0, 54.0, 50.0, 14.0, "Cancel", true))
    }

    pub(super) fn dialog_grab_tpl(
        &mut self,
        ui: &mut egui::Ui,
        name: &mut String,
        proj: &mut Box<core::settings::GrabProject>,
        replace: &mut bool,
    ) -> bool {
        // template 268; built-in names are reserved, an existing one is replaced on Yes
        let mut d = dlu::D::new(ui);
        if *replace {
            d.text(
                7.0,
                7.0,
                172.0,
                18.0,
                "The template with such name already exists. Do you want to replace the template?",
            );
        } else {
            d.text(
                7.0,
                7.0,
                172.0,
                18.0,
                "Please enter a name for the grabber template",
            );
        }
        d.edit(6.0, 29.0, 173.0, 14.0, name, !*replace);
        let n = name.trim().to_string();
        let ok = d.button(
            34.0,
            49.0,
            50.0,
            14.0,
            if *replace { "Yes" } else { "OK" },
            !n.is_empty(),
        );
        let cancel = d.button(
            102.0,
            49.0,
            50.0,
            14.0,
            if *replace { "No" } else { "Cancel" },
            true,
        );
        if ok
            && crate::grabber::TEMPLATES
                .iter()
                .any(|t| t.eq_ignore_ascii_case(&n) || tr(t).eq_ignore_ascii_case(&n))
        {
            self.open(Dialog::Info {
                title: "Template Name".into(),
                text: "This name is reserved. Please select another name for your template.".into(),
            });
            return true;
        }
        let exists = self
            .core
            .settings()
            .grab_templates
            .iter()
            .any(|t| t.opts.name == n);
        if ok && exists && !*replace {
            *replace = true;
            return true;
        }
        if ok {
            let mut t = (**proj).clone();
            (
                t.opts.name,
                t.opts.start,
                t.opts.login,
                t.opts.pass,
                t.opts.cookies,
                t.opts.template,
                t.files,
                t.pages,
            ) = (
                n.clone(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                vec![],
                0,
            );
            let mut s = self.core.settings();
            s.grab_templates.retain(|x| x.opts.name != n);
            s.grab_templates.push(t);
            drop(s);
            self.core.save();
        }
        if cancel && *replace {
            *replace = false;
            return true;
        }
        !(ok || cancel)
    }

    pub(super) fn dialog_grab_load(&mut self, ui: &mut egui::Ui, sel: &mut Option<usize>) -> bool {
        // template 270: the saved projects; OK or a double click opens one in the grabber
        let mut d = dlu::D::new(ui);
        let names: Vec<String> = self
            .core
            .settings()
            .grab_projects
            .iter()
            .map(|p| p.opts.name.clone())
            .collect();
        let lr = d.listview(7.0, 7.0, 186.0, 124.0, &[]);
        let mut open = false;
        d.ui.scope_builder(egui::UiBuilder::new().max_rect(lr.shrink(2.0)), |ui| {
            egui::ScrollArea::vertical()
                .id_salt("gload")
                .auto_shrink(false)
                .show(ui, |ui| {
                    for (k, n) in names.iter().enumerate() {
                        let r = ui.selectable_label(*sel == Some(k), n.as_str());
                        if r.clicked() {
                            *sel = Some(k);
                        }
                        if r.double_clicked() {
                            (*sel, open) = (Some(k), true);
                        }
                    }
                });
        });
        open |= d.button(200.0, 7.0, 50.0, 14.0, "OK", true);
        let cancel = d.button(200.0, 24.0, 50.0, 14.0, "Cancel", true);
        if open {
            match sel.and_then(|k| names.get(k)) {
                Some(n) => self.grab_load = Some(n.clone()),
                None => {
                    self.open(Dialog::Info {
                        title: "Load project".into(),
                        text: "Please select the project name!".into(),
                    });
                    return true;
                }
            }
        }
        !(open || cancel)
    }
}
