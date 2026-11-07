use crate::app::dialogs::Dialog;
use crate::app::{
    core, dlu, egui, explore, extension_dir, extension_page, i18n, integration, tr, tr_os, win,
    App, RichText, Settings, Vec2, GUIDES,
};

impl App {
    pub(super) fn dialog_extension_export(
        &mut self,
        ui: &mut egui::Ui,
        firefox: &mut bool,
    ) -> bool {
        let mut d = dlu::D::new(ui);
        d.text(
            10.0,
            8.0,
            310.0,
            18.0,
            "Choose the browser that will use the extension:",
        );
        if d.radio(
            10.0,
            31.0,
            310.0,
            12.0,
            !*firefox,
            "Chromium-based browsers",
        ) {
            *firefox = false;
        }
        d.label(
            23.0,
            47.0,
            297.0,
            10.0,
            "Chrome, Edge, Brave, Opera, Vivaldi and other Chromium browsers",
        );
        if d.radio(
            10.0,
            68.0,
            310.0,
            12.0,
            *firefox,
            "Firefox / Mozilla-based browsers",
        ) {
            *firefox = true;
        }
        d.label(
            23.0,
            84.0,
            297.0,
            10.0,
            "Firefox, Zen, Waterfox, LibreWolf, Floorp and other Firefox browsers",
        );
        d.text(10.0, 104.0, 310.0, 24.0, "NDM exports a ready-to-load folder with installation steps. Firefox removes temporary add-ons when the browser closes.");
        let export = d.button(194.0, 137.0, 59.0, 14.0, "Export...", true);
        let cancel = d.button(260.0, 137.0, 59.0, 14.0, "Cancel", true);
        if export {
            self.export_extension_for(*firefox);
        }
        !(export || cancel)
    }

    pub(super) fn dialog_cancelled(
        &mut self,
        ui: &mut egui::Ui,
        site: &mut String,
        url: &mut String,
        by_site: &mut bool,
        by_addr: &mut bool,
        dont_show: &mut bool,
    ) -> bool {
        let mut d = dlu::D::new(ui);
        d.text(6.0, 5.0, 271.0, 33.0, "You have cancelled this download twice.\n\nIf New Download Manager always takes over this download by mistake, you may add this address or this site to New Download Manager list of exceptions:");
        d.check(
            15.0,
            46.0,
            261.0,
            10.0,
            by_site,
            "Don't start downloading automatically from this site:",
        );
        d.label(27.0, 59.0, 248.0, 8.0, site.as_str());
        d.check(
            15.0,
            72.0,
            261.0,
            17.0,
            by_addr,
            "Don't start downloading automatically from this address:",
        );
        d.label(27.0, 89.0, 248.0, 8.0, url.as_str());
        d.text(7.0, 104.0, 269.0, 17.0, "Also you may edit exception lists on \"Options->File Types\" tab of the options dialog");
        let ok = d.button(82.0, 124.0, 50.0, 14.0, "OK", true);
        let cancel = d.button(151.0, 124.0, 50.0, 14.0, "Cancel", true);
        d.check(
            15.0,
            142.0,
            227.0,
            10.0,
            dont_show,
            "Don't show this dialog again",
        );
        if ok {
            let mut s = self.core.settings();
            if *by_site
                && !site.is_empty()
                && !s
                    .skip_sites
                    .split_whitespace()
                    .any(|x| x.eq_ignore_ascii_case(site))
            {
                s.skip_sites = format!("{} {site}", s.skip_sites).trim().to_string();
            }
            if *by_addr && !s.skip_urls.contains(url) {
                s.skip_urls.push(url.clone());
            }
            s.ask_exception = !*dont_show;
        }
        !(ok || cancel)
    }

    pub(super) fn dialog_exceptions(
        &mut self,
        ui: &mut egui::Ui,
        list: &mut Vec<String>,
        sel: &mut Option<usize>,
        ask: &mut bool,
    ) -> bool {
        if let Some(a) = self.exc_add.take() {
            list.push(a);
            *sel = Some(list.len() - 1);
        }
        let mut d = dlu::D::new(ui);
        d.check(7.0, 6.0, 267.0, 17.0, ask, "Show the dialog to add an address to the list of exceptions for a twice-cancelled download");
        d.label(
            7.0,
            26.0,
            267.0,
            9.0,
            "Don't start downloading automatically from the following addresses:",
        );
        let r = d.listview(7.0, 37.0, 267.0, 107.0, &[]);
        d.ui.scope_builder(egui::UiBuilder::new().max_rect(r.shrink(2.0)), |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                for (k, a) in list.iter().enumerate() {
                    if ui
                        .add(egui::SelectableLabel::new(*sel == Some(k), a.as_str()))
                        .clicked()
                    {
                        *sel = Some(k);
                    }
                }
            });
        });
        d.label(
            7.0,
            146.0,
            267.0,
            9.0,
            "You may use the asterisk wildcard for the address pattern.",
        );
        if d.button(16.0, 161.0, 50.0, 14.0, "Add", true) {
            self.open(Dialog::ExcAdd {
                addr: String::new(),
            });
        }
        if d.button(
            79.0,
            161.0,
            50.0,
            14.0,
            "Delete",
            sel.is_some_and(|k| k < list.len()),
        ) {
            list.remove(sel.take().unwrap());
        }
        let ok = d.button(215.0, 161.0, 50.0, 14.0, "OK", true);
        if ok {
            let (list, ask) = (list.clone(), *ask);
            self.opt_patch.push(Box::new(move |s: &mut Settings| {
                (s.skip_urls, s.ask_exception) = (list, ask)
            }));
        }
        !ok
    }

    pub(super) fn dialog_exc_add(&mut self, ui: &mut egui::Ui, addr: &mut String) -> bool {
        let mut d = dlu::D::new(ui);
        d.label(7.0, 10.0, 35.0, 9.0, "Address");
        d.edit(45.0, 7.0, 253.0, 14.0, addr, true);
        let ok = d.button(191.0, 25.0, 50.0, 14.0, "OK", !addr.trim().is_empty());
        if ok {
            self.exc_add = Some(addr.trim().to_string());
        }
        !(ok || d.button(248.0, 25.0, 50.0, 14.0, "Cancel", true))
    }

    pub(super) fn dialog_browser_details(&mut self, ui: &mut egui::Ui, name: &mut String) -> bool {
        let mut d = dlu::D::new(ui);
        let list = d.listview(
            7.0,
            7.0,
            416.0,
            59.0,
            &[
                ("Browser", 110.0),
                ("Path", 230.0),
                ("Type of integration", 74.0),
            ],
        );
        let rows: Vec<core::settings::Browser> = self
            .dialogs
            .iter()
            .find_map(|(_, x)| {
                if let Dialog::Options { s, .. } = x {
                    Some(s.browsers.clone())
                } else {
                    None
                }
            })
            .unwrap_or_else(|| self.core.settings().browsers.clone())
            .into_iter()
            .filter(|b| b.name == *name)
            .collect();
        d.rows(
            list.shrink2(Vec2::new(4.0, 0.0))
                .with_min_y(list.min.y + 20.0),
            |ui| {
                for b in &rows {
                    ui.horizontal(|ui| {
                        ui.add_sized(
                            [110.0 * dlu::DX - 8.0, 16.0],
                            egui::Label::new(b.name.as_str()).truncate(),
                        );
                        ui.add_sized(
                            [230.0 * dlu::DX - 8.0, 16.0],
                            egui::Label::new(b.exe.as_str()).truncate(),
                        );
                        ui.label(tr(integration(b)));
                    });
                }
            },
        );
        if d.button(135.0, 73.0, 67.0, 14.0, "Add version...", true) {
            if let Some(p) = rfd::FileDialog::new()
                .add_filter(tr_os("Programs"), &["exe"])
                .pick_file()
            {
                self.browser_add.push(core::settings::Browser {
                    name: name.clone(),
                    exe: p.display().to_string(),
                    on: true,
                });
            }
        }
        !d.button(227.0, 73.0, 67.0, 14.0, "Close", true)
    }

    pub(super) fn dialog_integrate(ui: &mut egui::Ui, b: &mut core::settings::Browser) -> bool {
        let page = extension_page(b).unwrap_or_default();
        let dir = extension_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let mut d = dlu::D::new(ui);
        d.label(
            8.0,
            3.0,
            466.0,
            12.0,
            RichText::new(b.name.as_str()).strong(),
        );
        d.group(6.0, 18.0, 473.0, 138.0, "");
        // ponytail: unsigned add-ons only load temporarily in release Firefox; signing on AMO makes it permanent
        d.text(
            14.0,
            28.0,
            458.0,
            60.0,
            GUIDES[!page.starts_with("about:") as usize],
        );
        d.readonly(14.0, 92.0, 458.0, 14.0, &dir);
        if d.button(
            14.0,
            162.0,
            90.0,
            14.0,
            "Open extensions page",
            !page.is_empty(),
        ) {
            let mut c = std::process::Command::new(&b.exe);
            win::clean_env(&mut c);
            let _ = c.arg(&page).spawn();
        }
        if d.button(110.0, 162.0, 70.0, 14.0, "Open folder", !dir.is_empty()) {
            explore(&dir);
        }
        !d.button(428.0, 180.0, 50.0, 14.0, "Close", true)
    }

    pub(super) fn dialog_keys(&mut self, ui: &mut egui::Ui, k: &mut core::settings::Keys) -> bool {
        // Template 229; the extension reads these through /config.
        let mut d = dlu::D::new(ui);
        d.hline(6.0, 6.0, 306.0);
        d.check(7.0, 13.0, 304.0, 10.0, &mut k.prevent, "Use the following key(s) to prevent downloading with New Download Manager for any links:");
        let p = k.prevent;
        d.at(89.0, 26.0, 34.0, 10.0, |ui| {
            ui.add_enabled(p, dlu::checkbox(&mut k.alt_p, "Alt"))
        });
        d.at(127.0, 26.0, 35.0, 10.0, |ui| {
            ui.add_enabled(p, dlu::checkbox(&mut k.shift_p, "Shift"))
        });
        d.at(165.0, 26.0, 35.0, 10.0, |ui| {
            ui.add_enabled(p, dlu::checkbox(&mut k.ctrl_p, "Ctrl"))
        });
        d.at(203.0, 25.0, 41.0, 10.0, |ui| {
            ui.add_enabled(p, dlu::checkbox(&mut k.del_p, "Del"))
        });
        d.hline(6.0, 41.0, 306.0);
        d.check(7.0, 50.0, 304.0, 10.0, &mut k.force, "Use the following key(s) to force downloading with New Download Manager for any links:");
        let f = k.force;
        d.at(89.0, 64.0, 34.0, 10.0, |ui| {
            ui.add_enabled(f, dlu::checkbox(&mut k.alt_f, "Alt"))
        });
        d.at(127.0, 64.0, 35.0, 10.0, |ui| {
            ui.add_enabled(f, dlu::checkbox(&mut k.shift_f, "Shift"))
        });
        d.at(165.0, 64.0, 35.0, 10.0, |ui| {
            ui.add_enabled(f, dlu::checkbox(&mut k.ctrl_f, "Ctrl"))
        });
        d.at(203.0, 64.0, 41.0, 10.0, |ui| {
            ui.add_enabled(f, dlu::checkbox(&mut k.ins_f, "Ins"))
        });
        d.text(7.0, 76.0, 304.0, 27.0, "Note: Sometimes after you click on a download link, a web page will load telling that the download starts in X seconds. In this case you will need to uncheck the following option to force downloading.");
        d.at(45.0, 104.0, 235.0, 17.0, |ui| ui.add_enabled(f, dlu::checkbox(&mut k.check_mouse, "Check for left mouse button clicked on a link along with the special key(s) pressed to force downloading the link")));
        d.text(7.0, 130.0, 304.0, 50.0, "Note 2: Sometimes when you click on a link, your browser requests other files AT THE SAME TIME like Java scripts, web pages, pictures, etc. When you press and hold down a special key, New Download Manager intercepts these files, and the browser may not request the necessary file because it will not run a Javascript, which has been intercepted by New Download Manager erroneously. If you want to intercept and download only the necessary files with New Download Manager, please turn on the option below:");
        d.check(45.0, 183.0, 266.0, 17.0, &mut k.skip_html, "While holding down a special key DO NOT take over the downloads which are web-pages, pictures, scripts and etc.");
        d.hline(6.0, 205.0, 306.0);
        let ok = d.button(89.0, 213.0, 50.0, 14.0, "OK", true);
        if ok {
            let k = k.clone();
            self.opt_patch
                .push(Box::new(move |s: &mut Settings| s.keys = k));
        }
        !(ok || d.button(178.0, 213.0, 50.0, 14.0, "Cancel", true))
    }

    pub(super) fn dialog_menu_items(
        &mut self,
        ui: &mut egui::Ui,
        dl: &mut bool,
        all: &mut bool,
    ) -> bool {
        // Template 334; its two FLV video items are hidden in NDM too (no WS_VISIBLE).
        let mut d = dlu::D::new(ui);
        d.label(
            7.0,
            11.0,
            261.0,
            9.0,
            "Selected menu items will appear in browser context menu:",
        );
        d.check(
            7.0,
            27.0,
            261.0,
            10.0,
            dl,
            "Download with New Download Manager",
        );
        d.check(
            7.0,
            42.0,
            261.0,
            10.0,
            all,
            "Download all links with New Download Manager",
        );
        let ok = d.button(152.0, 54.0, 50.0, 14.0, "OK", true);
        if ok {
            let (dl, all) = (*dl, *all);
            self.opt_patch.push(Box::new(move |s: &mut Settings| {
                (s.menu_dl, s.menu_all) = (dl, all)
            }));
        }
        !(ok || d.button(210.0, 54.0, 50.0, 14.0, "Cancel", true))
    }

    pub(super) fn dialog_panel(
        &mut self,
        ui: &mut egui::Ui,
        tab: &mut u8,
        types: &mut Vec<(String, bool)>,
        min: &mut std::collections::BTreeMap<String, u64>,
        mini: &mut bool,
        sel: &mut Option<usize>,
        nocap: &mut bool,
        drm: &mut bool,
        smode: &mut u8,
        sites: &mut Vec<String>,
        smini: &mut bool,
        ssel: &mut Option<usize>,
    ) -> bool {
        if let Some(s) = self.panel_site.take() {
            if !sites.contains(&s) {
                sites.push(s);
            }
        }
        if let Some(t) = self.panel_add.take() {
            match types.iter().position(|x| x.0.eq_ignore_ascii_case(&t)) {
                Some(k) => types[k].1 = true,
                None => types.push((t, true)),
            }
        }
        if let Some((t, kb)) = self.panel_min.take() {
            if kb == 0 {
                min.remove(&t)
            } else {
                min.insert(t, kb)
            };
        }
        let mut d = dlu::D::new(ui);
        let (px, py, _, _) = d.tabs(
            7.0,
            7.0,
            322.0,
            262.0,
            tab,
            &["For web-players", "For selected files"],
        );
        let mut d2 = dlu::D::new_offset(d.ui, px - 2.0, py - 2.0);
        if *tab == 0 {
            d2.text(7.0, 4.0, 308.0, 18.0, "New Download Manager can show its Download panel on a web-player in a browser when New Download Manager detects a multimedia request from the web-player");
            d2.hline(7.0, 26.0, 308.0);
            d2.label(7.0, 30.0, 79.0, 9.0, "Panel view:");
            if d2.radio(22.0, 43.0, 65.0, 10.0, !*mini, "Full mode") {
                *mini = false;
            }
            if d2.radio(22.0, 59.0, 64.0, 10.0, *mini, "Mini mode") {
                *mini = true;
            }
            d2.hline(7.0, 75.0, 308.0);
            d2.label(
                7.0,
                80.0,
                308.0,
                9.0,
                "Show download panel for the following file types:",
            );
            let list = d2.listview(28.0, 92.0, 148.0, 88.0, &[]);
            d2.ui
                .scope_builder(egui::UiBuilder::new().max_rect(list.shrink(3.0)), |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        for (k, (t, on)) in types.iter_mut().enumerate() {
                            ui.horizontal(|ui| {
                                ui.checkbox(on, tr(""));
                                let size = min
                                    .get(t.as_str())
                                    .map(|kb| {
                                        format!("  ({} {kb} {})", tr_os("Min. size"), tr_os("KB"))
                                    })
                                    .unwrap_or_default();
                                if ui
                                    .add(egui::SelectableLabel::new(
                                        *sel == Some(k),
                                        i18n::line(&format!("{}{size}", i18n::ltr(t))),
                                    ))
                                    .clicked()
                                {
                                    *sel = Some(k);
                                }
                            });
                        }
                    });
                });
            if d2.button(190.0, 93.0, 99.0, 14.0, "Add", true) {
                self.open(Dialog::PanelType {
                    name: String::new(),
                });
            }
            let picked = sel.and_then(|k| types.get(k)).map(|t| t.0.clone());
            if d2.button(190.0, 111.0, 99.0, 14.0, "Minimum size", picked.is_some()) {
                let ext = picked.unwrap();
                let kb = min.get(&ext).copied().unwrap_or(0).to_string();
                self.open(Dialog::PanelMin { ext, kb });
            }
            if d2.button(190.0, 129.0, 99.0, 14.0, "Check All", true) {
                types.iter_mut().for_each(|t| t.1 = true);
            }
            if d2.button(190.0, 147.0, 99.0, 14.0, "Clear All", true) {
                types.iter_mut().for_each(|t| t.1 = false);
            }
            if d2.button(190.0, 165.0, 99.0, 14.0, "Exceptions", true) {
                let text = self
                    .dialogs
                    .iter()
                    .find_map(|(_, x)| {
                        if let Dialog::Options { s, .. } = x {
                            Some(s.panel_skip.clone())
                        } else {
                            None
                        }
                    })
                    .unwrap_or_default();
                self.open(Dialog::PanelExc { text });
            }
            d2.check(
                50.0,
                221.0,
                251.0,
                9.0,
                nocap,
                "Don't capture downloads from web-players automatically",
            );
            d2.hline(7.0, 234.0, 308.0);
            d2.check(22.0, 238.0, 279.0, 16.0, drm, "Show download panel for protected content which New Download Manager may not download");
        } else {
            // Template 348: a panel over selected text that holds links (extension's content.js)
            d2.text(7.0, 5.0, 309.0, 32.0, "New Download Manager can show Download panel on a web-page when you select a text that contains download links.");
            d2.hline(7.0, 39.0, 309.0);
            d2.label(7.0, 44.0, 79.0, 9.0, "Panel view:");
            if d2.radio(22.0, 58.0, 65.0, 10.0, !*smini, "Full mode") {
                *smini = false;
            }
            if d2.radio(22.0, 74.0, 64.0, 10.0, *smini, "Mini mode") {
                *smini = true;
            }
            d2.hline(7.0, 92.0, 309.0);
            for (y, h, v, t) in [
                (101.0, 10.0, 0, "Show Download panel for any selected links"),
                (
                    115.0,
                    10.0,
                    1,
                    "Don't show Download panel for selected links",
                ),
                (
                    127.0,
                    14.0,
                    2,
                    "Show Download panel for selected links for the following sites only:",
                ),
            ] {
                if d2.radio(22.0, y, 288.0, h, *smode == v, t) {
                    *smode = v;
                }
            }
            let on = *smode == 2;
            let list = d2.listview(38.0, 148.0, 188.0, 74.0, &[]);
            d2.ui
                .scope_builder(egui::UiBuilder::new().max_rect(list.shrink(3.0)), |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        for (k, s) in sites.iter().enumerate() {
                            if ui
                                .add_enabled(
                                    on,
                                    egui::SelectableLabel::new(*ssel == Some(k), s.as_str()),
                                )
                                .clicked()
                            {
                                *ssel = Some(k);
                            }
                        }
                    });
                });
            if d2.button(235.0, 152.0, 56.0, 14.0, "Add", on) {
                self.open(Dialog::PanelSite {
                    name: String::new(),
                });
            }
            if d2.button(
                235.0,
                171.0,
                56.0,
                14.0,
                "Delete",
                on && ssel.is_some_and(|k| k < sites.len()),
            ) {
                sites.remove(ssel.take().unwrap());
            }
        }
        let ok = d.button(218.0, 280.0, 50.0, 14.0, "OK", true);
        if ok {
            let (t, m, v) = (types.clone(), min.clone(), *mini);
            let sel_panel = (*nocap, *drm, *smode, sites.clone(), *smini);
            self.opt_patch.push(Box::new(move |s: &mut Settings| {
                (s.panel_types, s.panel_min, s.panel_mini) = (t, m, v);
                (
                    s.panel_nocap,
                    s.panel_drm,
                    s.sel_mode,
                    s.sel_sites,
                    s.sel_mini,
                ) = sel_panel;
            }));
        }
        !(ok || d.button(276.0, 280.0, 50.0, 14.0, "Cancel", true))
    }

    pub(super) fn dialog_panel_site(&mut self, ui: &mut egui::Ui, name: &mut String) -> bool {
        let mut d = dlu::D::new(ui);
        d.edit(7.0, 7.0, 172.0, 14.0, name, true);
        let site = name
            .trim()
            .trim_start_matches("http://")
            .trim_start_matches("https://")
            .trim_end_matches('/')
            .to_ascii_lowercase();
        let ok = d.button(
            60.0,
            31.0,
            50.0,
            14.0,
            "OK",
            !site.is_empty() && !site.contains(' '),
        );
        if ok {
            self.panel_site = Some(site);
        }
        !(ok || d.button(120.0, 31.0, 50.0, 14.0, "Cancel", true))
    }

    pub(super) fn dialog_panel_type(&mut self, ui: &mut egui::Ui, name: &mut String) -> bool {
        let mut d = dlu::D::new(ui);
        d.edit(7.0, 7.0, 112.0, 14.0, name, true);
        let t = name.trim().trim_start_matches('.').to_ascii_uppercase();
        let ok = d.button(
            9.0,
            28.0,
            50.0,
            14.0,
            "OK",
            !t.is_empty() && !t.contains([' ', '.', '/', '\\']),
        );
        if ok {
            self.panel_add = Some(t);
        }
        !(ok || d.button(66.0, 28.0, 50.0, 14.0, "Cancel", true))
    }

    pub(super) fn dialog_panel_min(
        &mut self,
        ui: &mut egui::Ui,
        ext: &str,
        kb: &mut String,
    ) -> bool {
        let mut d = dlu::D::new(ui);
        // NDM's own wording for this setting (21149); the type is shown on its own line
        d.text(7.0, 7.0, 225.0, 32.0, "Do not show \"Download this video\" panel if media file size is below the following minimum size in kilobytes");
        d.label_r(40.0, 46.0, 58.0, 8.0, ext);
        d.edit(103.0, 44.0, 48.0, 14.0, kb, true);
        d.label(155.0, 46.0, 43.0, 8.0, "KB");
        let v = kb.trim().parse::<u64>();
        let ok = d.button(59.0, 116.0, 50.0, 14.0, "OK", v.is_ok());
        if ok {
            self.panel_min = Some((ext.to_owned(), v.unwrap_or(0)));
        }
        !(ok || d.button(130.0, 116.0, 50.0, 14.0, "Cancel", true))
    }

    pub(super) fn dialog_panel_exc(&mut self, ui: &mut egui::Ui, text: &mut String) -> bool {
        let mut d = dlu::D::new(ui);
        d.text(
            7.0,
            6.0,
            240.0,
            18.0,
            "Don't show download panel for the following sites:",
        );
        d.edit(7.0, 27.0, 240.0, 51.0, text, true);
        d.text(
            7.0,
            81.0,
            240.0,
            18.0,
            "Separate names by spaces. You may use asterisk as a wildcard pattern",
        );
        let ok = d.button(67.0, 102.0, 50.0, 14.0, "OK", true);
        if ok {
            let t = text.split_whitespace().collect::<Vec<_>>().join(" ");
            self.opt_patch
                .push(Box::new(move |s: &mut Settings| s.panel_skip = t));
        }
        !(ok || d.button(136.0, 102.0, 50.0, 14.0, "Cancel", true))
    }
}
