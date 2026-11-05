use crate::app::dialogs::Dialog;
#[cfg(target_os = "macos")]
use crate::app::mac_capture;
use crate::app::{
    core, dlu, egui, extension_page, i18n, integration, net, tr, tr_os, win, App, Path, RichText,
    Settings, Vec2, DUPL, GENERAL, PAGE_DX,
};

impl App {
    pub(super) fn options_general(
        &mut self,
        ui: &mut egui::Ui,
        s: &mut Settings,
        origin: (f32, f32),
        ctx: &egui::Context,
        icon: Option<egui::TextureId>,
        open: &mut Option<Dialog>,
        integrate: &mut Vec<core::settings::Browser>,
        has_extension: bool,
    ) {
        let mut d2 = dlu::D::new_offset(ui, origin.0, origin.1).with_dx(PAGE_DX);
        d2.page_header(icon, 48.0, "Browser/System Integration");
        d2.check(
            13.0,
            25.0,
            240.0,
            10.0,
            &mut s.autostart,
            "Launch New Download Manager on startup",
        );
        d2.check(
            13.0,
            39.0,
            241.0,
            10.0,
            &mut s.clip_watch,
            "Automatically start downloading of URLs placed to clipboard",
        );
        #[cfg(not(target_os = "macos"))]
        d2.check(
            13.0,
            53.0,
            240.0,
            10.0,
            &mut s.adv_integration,
            "Use advanced browser integration",
        );
        #[cfg(target_os = "macos")]
        if d2.button(13.0, 52.0, 128.0, 14.0, "macOS HTTP capture...", true) {
            let (services, message) = match mac_capture::services() {
                Ok(services) => (services, String::new()),
                Err(error) => (vec![], error),
            };
            *open = Some(Dialog::MacCapture {
                services,
                selected: None,
                pending: None,
                message,
            });
        }
        d2.group(
            4.0,
            73.0,
            261.0,
            144.0,
            "Capture downloads from the following browsers:",
        );
        let list = d2.listview(
            13.0,
            84.0,
            241.0,
            84.0,
            &[("Browser", 120.0), ("Type of integration", 119.0)],
        );
        let sel_id = egui::Id::new("browser_sel");
        let mut sel: Option<usize> = d2
            .ui
            .data(|m| m.get_temp(sel_id))
            .flatten()
            .filter(|k| *k < s.browsers.len());
        let icons: Vec<Option<egui::TextureId>> = s
            .browsers
            .iter()
            .map(|b| self.exe_icon(ctx, &b.exe))
            .collect();
        d2.rows(
            list.shrink2(Vec2::new(4.0, 0.0))
                .with_min_y(list.min.y + 20.0),
            |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for (k, b) in s.browsers.iter_mut().enumerate() {
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut b.on, tr(""));
                            if let Some(t) = icons[k] {
                                ui.image((t, Vec2::splat(16.0)));
                            }
                            let w = 120.0 * PAGE_DX - 48.0;
                            if ui
                                .add_sized(
                                    [w, 16.0],
                                    egui::SelectableLabel::new(sel == Some(k), b.name.as_str()),
                                )
                                .clicked()
                            {
                                sel = Some(k);
                            }
                            ui.label(tr(integration(b)));
                        });
                    }
                });
            },
        );
        if d2.button(13.0, 173.0, 72.0, 14.0, "Reintegrate All", has_extension) {
            for b in s
                .browsers
                .iter()
                .filter(|b| b.on && extension_page(b).is_some())
            {
                integrate.push(b.clone());
            }
        }
        if d2.button(97.0, 173.0, 72.0, 14.0, "Add browser...", true) {
            if let Some(p) = rfd::FileDialog::new()
                .add_filter(tr_os("Programs"), &["exe"])
                .pick_file()
            {
                let name = p
                    .file_stem()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                s.browsers.push(core::settings::Browser {
                    name,
                    exe: p.display().to_string(),
                    on: true,
                });
                sel = Some(s.browsers.len() - 1);
            }
        }
        if d2.button(182.0, 173.0, 72.0, 14.0, "Details", sel.is_some()) {
            *open = Some(Dialog::BrowserDetails {
                name: s.browsers[sel.unwrap()].name.clone(),
            });
        }
        d2.ui.data_mut(|m| m.insert_temp(sel_id, sel));
        d2.hline(16.0, 192.0, 238.0);
        d2.label(
            10.0,
            202.0,
            190.0,
            9.0,
            "Customize keys to prevent or force downloading with New Download Manager",
        );
        if d2.button(204.0, 198.0, 50.0, 14.0, "Keys...", true) {
            *open = Some(Dialog::Keys { k: s.keys.clone() });
        }
        d2.label(
            10.0,
            224.0,
            190.0,
            9.0,
            "Customize menu items in context menu of browsers",
        );
        if d2.button(204.0, 221.0, 50.0, 14.0, "Edit...", true) {
            *open = Some(Dialog::MenuItems {
                dl: s.menu_dl,
                all: s.menu_all,
            });
        }
        d2.label(
            12.0,
            243.0,
            188.0,
            9.0,
            "Customize download panels in browsers",
        );
        if d2.button(204.0, 240.0, 50.0, 14.0, "...", true) {
            *open = Some(Dialog::Panel {
                tab: 0,
                types: s.panel_types.clone(),
                min: s.panel_min.clone(),
                mini: s.panel_mini,
                sel: None,
                nocap: s.panel_nocap,
                drm: s.panel_drm,
                smode: s.sel_mode,
                sites: s.sel_sites.clone(),
                smini: s.sel_mini,
                ssel: None,
            });
        }
    }

    pub(super) fn options_file_types(
        ui: &mut egui::Ui,
        s: &mut Settings,
        origin: (f32, f32),
        icon: Option<egui::TextureId>,
        open: &mut Option<Dialog>,
    ) {
        let mut d2 = dlu::D::new_offset(ui, origin.0, origin.1).with_dx(PAGE_DX);
        d2.page_header(icon, 45.0, "Downloaded file types");
        d2.label(
            7.0,
            27.0,
            237.0,
            10.0,
            "Automatically start downloading the following file types:",
        );
        d2.edit(7.0, 39.0, 254.0, 36.0, &mut s.file_types, true);
        if d2.button(203.0, 78.0, 50.0, 14.0, "Default", true) {
            s.file_types = Settings::default().file_types;
        }
        d2.label(
            7.0,
            100.0,
            235.0,
            10.0,
            "Don't start downloading automatically from the following sites:",
        );
        d2.edit(7.0, 112.0, 254.0, 30.0, &mut s.skip_sites, true);
        d2.label(7.0, 144.0, 189.0, 10.0, "(separate names by spaces)");
        if d2.button(203.0, 145.0, 50.0, 14.0, "Default", true) {
            s.skip_sites.clear();
        }
        d2.label(
            7.0,
            164.0,
            254.0,
            10.0,
            "Don't start downloading automatically from the following addresses:",
        );
        if d2.button(7.0, 176.0, 91.0, 14.0, "Edit list ...", true) {
            *open = Some(Dialog::Exceptions {
                list: s.skip_urls.clone(),
                sel: None,
                ask: s.ask_exception,
            });
        }
    }

    pub(super) fn options_save_to(
        &mut self,
        ui: &mut egui::Ui,
        s: &mut Settings,
        origin: (f32, f32),
        icon: Option<egui::TextureId>,
        new_cat: &mut Option<Option<String>>,
    ) {
        let mut d2 = dlu::D::new_offset(ui, origin.0, origin.1).with_dx(PAGE_DX);
        d2.page_header(icon, 48.0, "Categories, file types, folders");
        d2.group(7.0, 25.0, 253.0, 128.0, "Save To...");
        d2.label(13.0, 42.0, 121.0, 10.0, "Category");
        let mut cat = s
            .cat_dirs
            .keys()
            .next()
            .cloned()
            .filter(|_| false)
            .unwrap_or_else(|| {
                d2.ui.data_mut(|m| {
                    m.get_temp::<String>(egui::Id::new("optcat"))
                        .unwrap_or_else(|| GENERAL.to_string())
                })
            });
        let cat_names = self.core.settings().category_names();
        let cats: Vec<&str> = cat_names.iter().map(String::as_str).collect();
        if d2.combo(13.0, 55.0, 141.0, 14.0, "optcat", &mut cat, &cats, true) {
            d2.ui
                .data_mut(|m| m.insert_temp(egui::Id::new("optcat"), cat.clone()));
        }
        if d2.button(205.0, 35.0, 50.0, 14.0, "New", true) {
            *new_cat = Some(None);
        }
        if d2.button(205.0, 55.0, 50.0, 14.0, "Edit...", cat != GENERAL) {
            *new_cat = Some(Some(cat.clone()));
        }
        let exts = s
            .categories
            .iter()
            .find(|c| c.name == cat)
            .map_or(String::new(), |c| c.exts.clone());
        let exts = exts.as_str();
        d2.label(
            14.0,
            74.0,
            238.0,
            10.0,
            i18n::trf_os(
                "Automatically put in \"%s\" category the following file types:",
                &[&tr_os(&cat)],
            ),
        );
        d2.readonly(14.0, 85.0, 241.0, 14.0, exts);
        d2.label(
            13.0,
            106.0,
            242.0,
            10.0,
            i18n::trf_os(
                "Default download directory for \"%s\" category",
                &[&tr_os(&cat)],
            ),
        );
        let mut dir = s.folder_for(&cat);
        if d2.edit(14.0, 118.0, 187.0, 14.0, &mut dir, true).changed() {
            if cat == GENERAL {
                s.base_dir = dir.clone();
            } else {
                s.cat_dirs.insert(cat.clone(), dir.clone());
            }
        }
        if d2.button(205.0, 118.0, 50.0, 14.0, "Browse", true) {
            if let Some(p) = rfd::FileDialog::new().set_directory(&dir).pick_folder() {
                let p = p.display().to_string();
                if cat == GENERAL {
                    s.base_dir = p
                } else {
                    s.cat_dirs.insert(cat.clone(), p);
                }
            }
        }
        // = the category's "Remember last save path" (template 204)
        let label = i18n::trf_os(
            "Change folder for \"%s\" category on last selected",
            &[&tr_os(&cat)],
        );
        match s.categories.iter_mut().find(|c| c.name == cat) {
            Some(c) => {
                d2.at(15.0, 137.0, 240.0, 10.0, |ui| {
                    ui.add(dlu::checkbox(&mut c.remember, &label))
                });
            }
            None => {
                d2.at(15.0, 137.0, 240.0, 10.0, |ui| {
                    ui.add(dlu::checkbox(&mut s.remember_general, &label))
                });
            }
        }
        d2.check(
            15.0,
            159.0,
            240.0,
            10.0,
            &mut s.set_date,
            "Set file creation date as provided by the server",
        );
        d2.group(7.0, 173.0, 253.0, 77.0, "Temporary directory");
        d2.edit(13.0, 186.0, 187.0, 14.0, &mut s.temp_dir, true);
        if d2.button(204.0, 186.0, 50.0, 14.0, "Browse", true) {
            if let Some(p) = rfd::FileDialog::new()
                .set_directory(&s.temp_dir)
                .pick_folder()
            {
                s.temp_dir = p.display().to_string();
            }
        }
        d2.text(13.0, 203.0, 242.0, 42.0, "Temporary directory is required for storing file parts during download.\n If you have several physical drives on your computer, you should select different physical drives for temporary directory and \"Save To\" folders for faster assembling of downloaded files.");
    }

    pub(super) fn options_downloads(
        ui: &mut egui::Ui,
        s: &mut Settings,
        origin: (f32, f32),
        icon: Option<egui::TextureId>,
        open: &mut Option<Dialog>,
    ) {
        let mut d2 = dlu::D::new_offset(ui, origin.0, origin.1).with_dx(PAGE_DX);
        d2.page_header(icon, 48.0, "Default download settings");
        d2.label(
            7.0,
            28.0,
            191.0,
            10.0,
            "Customize \"Download progress\" dialog",
        );
        if d2.button(205.0, 25.0, 50.0, 14.0, "Edit...", true) {
            *open = Some(Dialog::ProgCustom {
                view: s.prog_view,
                sl: s.prog_sl_tab,
                c: s.prog_c_tab,
                btns: s.prog_tab_btns,
            });
        }
        d2.check(
            9.0,
            44.0,
            241.0,
            10.0,
            &mut s.show_progress,
            "Show start download dialog",
        );
        d2.check(
            26.0,
            57.0,
            235.0,
            10.0,
            &mut s.only_queue,
            "Do not start downloading, only add files to the queue",
        );
        d2.check(
            9.0,
            69.0,
            243.0,
            10.0,
            &mut s.show_complete,
            "Show download complete dialog",
        );
        d2.label(
            14.0,
            82.0,
            237.0,
            10.0,
            "Note: These settings don't relate to queue processing",
        );
        d2.hline(7.0, 93.0, 254.0);
        d2.check(
            9.0,
            96.0,
            251.0,
            19.0,
            &mut s.start_at_once,
            "Start downloading immediately while displaying \"Download File Info\" dialog",
        );
        d2.check(
            9.0,
            119.0,
            251.0,
            10.0,
            &mut s.ask_queue_later,
            "Show queue selection panel on pressing \"Download Later\"",
        );
        d2.check(
            9.0,
            134.0,
            252.0,
            10.0,
            &mut s.ask_queue_batch,
            "Show queue selection panel on adding batch downloads",
        );
        d2.check(
            9.0,
            149.0,
            251.0,
            10.0,
            &mut s.ignore_date,
            "Ignore file modification time changes when resuming a download",
        );
        d2.hline(7.0, 164.0, 254.0);
        d2.label(
            7.0,
            170.0,
            246.0,
            10.0,
            "If a duplicate download link is added:",
        );
        let k = if s.dupl_ask {
            0
        } else {
            [3, 1, 2][s.dupl_action.min(2) as usize]
        };
        let mut dup = DUPL[k].to_string();
        if d2.combo(7.0, 180.0, 253.0, 14.0, "dup", &mut dup, &DUPL, true) {
            match DUPL.iter().position(|x| *x == dup) {
                Some(0) => s.dupl_ask = true,
                Some(k) => (s.dupl_ask, s.dupl_action) = (false, [0, 1, 2, 0][k]),
                None => {}
            }
        }
        d2.hline(7.0, 197.0, 254.0);
        d2.label(
            7.0,
            203.0,
            254.0,
            9.0,
            "User-Agent for manually added downloads:",
        );
        d2.edit(7.0, 216.0, 254.0, 14.0, &mut s.user_agent, true);
        d2.hline(7.0, 233.0, 254.0);
        d2.label(7.0, 242.0, 188.0, 8.0, "Virus checking settings");
        if d2.button(205.0, 239.0, 50.0, 14.0, "Edit...", true) {
            *open = Some(Dialog::Virus {
                exe: s.virus_exe.clone(),
                args: s.virus_args.clone(),
            });
        }
    }

    pub(super) fn options_connection(
        &mut self,
        ui: &mut egui::Ui,
        s: &mut Settings,
        origin: (f32, f32),
        icon: Option<egui::TextureId>,
        open: &mut Option<Dialog>,
    ) {
        let mut d2 = dlu::D::new_offset(ui, origin.0, origin.1).with_dx(PAGE_DX);
        d2.page_header(icon, 47.0, "Connections and Limits");
        d2.group(7.0, 30.0, 253.0, 124.0, "Max. connections number");
        d2.label_r(20.0, 42.0, 148.0, 10.0, "Default max. conn. number");
        let mut conns = s.conns.to_string();
        if d2.combo(
            172.0,
            40.0,
            67.0,
            14.0,
            "conns",
            &mut conns,
            &["1", "2", "4", "8", "16", "24", "32"],
            true,
        ) {
            s.conns = conns.parse().unwrap_or(8);
        }
        d2.hline(13.0, 58.0, 240.0);
        d2.label(15.0, 61.0, 239.0, 10.0, "Exceptions:");
        let list = d2.listview(
            13.0,
            71.0,
            187.0,
            75.0,
            &[("Server", 120.0), ("Max. connections number", 66.0)],
        );
        let sel_id = egui::Id::new("srvconn_sel");
        let mut sel: Option<usize> = d2
            .ui
            .data(|m| m.get_temp(sel_id))
            .flatten()
            .filter(|k| *k < s.server_conns.len());
        let mut edit = None;
        d2.rows(
            list.shrink2(Vec2::new(2.0, 0.0))
                .with_min_y(list.min.y + 18.0),
            |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for (k, (m, n)) in s.server_conns.iter().enumerate() {
                    ui.horizontal(|ui| {
                        let a = ui.add_sized(
                            [120.0 * PAGE_DX - 4.0, 16.0],
                            egui::SelectableLabel::new(sel == Some(k), m.as_str()),
                        );
                        let b = ui.add_sized(
                            [66.0 * PAGE_DX - 8.0, 16.0],
                            egui::SelectableLabel::new(sel == Some(k), n.to_string()),
                        );
                        if a.clicked() || b.clicked() {
                            sel = Some(k);
                        }
                        if a.double_clicked() || b.double_clicked() {
                            edit = Some(k);
                        }
                    });
                }
            },
        );
        if d2.button(205.0, 77.0, 50.0, 14.0, "New", true) {
            self.open(Dialog::ServerConn {
                orig: None,
                server: String::new(),
                n: s.conns.to_string(),
            });
        }
        if d2.button(205.0, 95.0, 50.0, 14.0, "Delete", sel.is_some()) {
            s.server_conns.remove(sel.unwrap());
            sel = None;
        }
        if d2.button(205.0, 113.0, 50.0, 14.0, "Edit", sel.is_some()) {
            edit = sel;
        }
        if d2.button(205.0, 132.0, 50.0, 14.0, "HTTP/1.1...", true) {
            *open = Some(Dialog::Http1Hosts {
                hosts: s.http1_hosts.clone(),
            });
        }
        if let Some(k) = edit {
            let (server, n) = s.server_conns[k].clone();
            self.open(Dialog::ServerConn {
                orig: Some(k),
                server,
                n: n.to_string(),
            });
        }
        d2.ui.data_mut(|m| m.insert_temp(sel_id, sel));
        d2.group(7.0, 161.0, 253.0, 55.0, "");
        // NDM's quota (template 141 group at [7,161]); the speed limit has its own dialog (337)
        d2.check(18.0, 158.0, 64.0, 12.0, &mut s.quota_on, "Download limits");
        let on = s.quota_on;
        d2.label_r(14.0, 177.0, 93.0, 10.0, "Download no more than");
        d2.drag(112.0, 174.0, 24.0, 12.0, &mut s.quota_mb, 1, 999_999);
        d2.label(141.0, 177.0, 72.0, 10.0, "MBytes");
        d2.label_r(55.0, 189.0, 52.0, 10.0, "every");
        d2.drag(112.0, 187.0, 24.0, 12.0, &mut s.quota_hours, 1, 23);
        d2.label(143.0, 189.0, 69.0, 10.0, "hours");
        if on {
            d2.check(
                18.0,
                202.0,
                209.0,
                10.0,
                &mut s.quota_warn,
                "Show warning before stopping downloads",
            );
        } else {
            d2.check_off(
                18.0,
                202.0,
                209.0,
                10.0,
                "Show warning before stopping downloads",
            );
        }
        if net::nic_supported() {
            d2.check(
                11.0,
                224.0,
                190.0,
                10.0,
                &mut s.multi_net,
                "Download over several network connections at once",
            );
            d2.label(
                23.0,
                237.0,
                178.0,
                10.0,
                "Each network needs its own internet connection",
            );
        } else {
            s.multi_net = false;
            d2.check_off(
                11.0,
                224.0,
                190.0,
                10.0,
                "Download over several network connections at once",
            );
            d2.label(
                23.0,
                237.0,
                178.0,
                10.0,
                "Not available: Linux 5.7 or later is needed to choose the network",
            );
        }
        if d2.button(205.0, 222.0, 50.0, 14.0, "Networks...", s.multi_net) {
            self.open(Dialog::Networks {
                nics: win::nics(),
                off: s.nets_off.clone(),
            });
        }
    }

    pub(super) fn options_site_logins(
        &mut self,
        ui: &mut egui::Ui,
        s: &mut Settings,
        origin: (f32, f32),
    ) {
        // Site Logins (template 145): credentials applied to matching download addresses.
        let icon = self.res.icon(272);
        let mut d2 = dlu::D::new_offset(ui, origin.0, origin.1).with_dx(PAGE_DX);
        d2.page_header(icon, 42.0, "User names and passwords for servers/sites");
        let list = d2.listview(
            7.0,
            28.0,
            255.0,
            172.0,
            &[
                ("Server/path", 110.0),
                ("User name", 75.0),
                ("Password", 70.0),
            ],
        );
        let sel_id = egui::Id::new("login_sel");
        let mut sel: Option<usize> = d2.ui.data(|m| m.get_temp(sel_id)).flatten();
        d2.rows(
            list.shrink2(Vec2::new(2.0, 0.0))
                .with_min_y(list.min.y + 18.0),
            |ui| {
                for (k, l) in s.logins.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        let w = |n: f32| n * PAGE_DX - 6.0;
                        let a = ui.add(
                            egui::TextEdit::singleline(&mut l.site)
                                .desired_width(w(110.0))
                                .hint_text("example.com"),
                        );
                        let b =
                            ui.add(egui::TextEdit::singleline(&mut l.user).desired_width(w(75.0)));
                        let c = ui.add(
                            egui::TextEdit::singleline(&mut l.pass)
                                .password(true)
                                .desired_width(w(70.0)),
                        );
                        if a.gained_focus() || b.gained_focus() || c.gained_focus() {
                            sel = Some(k);
                        }
                        if ui.ctx().memory(|m| m.has_focus(a.id))
                            && ui
                                .data(|m| m.get_temp::<bool>(sel_id.with("edit")))
                                .unwrap_or(false)
                        {
                            ui.data_mut(|m| m.remove::<bool>(sel_id.with("edit")));
                        }
                        if sel == Some(k)
                            && ui
                                .data(|m| m.get_temp::<bool>(sel_id.with("edit")))
                                .unwrap_or(false)
                        {
                            a.request_focus();
                        }
                    });
                }
            },
        );
        if d2.button(13.0, 205.0, 50.0, 14.0, "New", true) {
            s.logins.push(Default::default());
            sel = Some(s.logins.len() - 1);
            d2.ui.data_mut(|m| m.insert_temp(sel_id.with("edit"), true));
        }
        let has = sel.is_some_and(|k| k < s.logins.len());
        if d2.button(76.0, 205.0, 50.0, 14.0, "Edit", has) {
            d2.ui.data_mut(|m| m.insert_temp(sel_id.with("edit"), true));
        }
        if d2.button(139.0, 205.0, 50.0, 14.0, "Remove", has) {
            s.logins.remove(sel.unwrap());
            sel = None;
        }
        d2.ui.data_mut(|m| m.insert_temp(sel_id, sel));
    }

    pub(super) fn options_dial_up(
        &mut self,
        ui: &mut egui::Ui,
        s: &mut Settings,
        origin: (f32, f32),
    ) {
        // Dial Up / VPN (template 185, COptDialUp): Windows' connections; Apply stores the user
        // name (and with "Save password" the password) in Windows, as NDM does (RasSetEntryDialParams).
        let icon = self.res.icon(275);
        let mut d2 = dlu::D::new_offset(ui, origin.0, origin.1).with_dx(PAGE_DX);
        d2.page_header(icon, 47.0, "Dial up / VPN settings");
        d2.check(
            8.0,
            30.0,
            247.0,
            10.0,
            &mut s.dial_on,
            "Use Windows Dial Up / VPN Networking",
        );
        let on = s.dial_on;
        d2.group(7.0, 44.0, 254.0, 110.0, "Connection options");
        for (y, t) in [
            (62.0, "Connection:"),
            (84.0, "User name:"),
            (106.0, "Password:"),
        ] {
            d2.label_r(
                16.0,
                y,
                59.0,
                10.0,
                if on {
                    RichText::new(tr(t))
                } else {
                    RichText::new(tr(t)).weak()
                },
            );
        }
        let id = egui::Id::new("dialup_form");
        let (mut user, mut pass, mut save) = d2
            .ui
            .data_mut(|m| m.get_temp::<(String, String, bool)>(id))
            .unwrap_or_default();
        let entries = win::ras_entries();
        if s.dial_entry.is_empty() {
            s.dial_entry = entries.first().cloned().unwrap_or_default();
        }
        let list: Vec<&str> = entries.iter().map(String::as_str).collect();
        d2.combo(
            80.0,
            59.0,
            156.0,
            14.0,
            "dialup",
            &mut s.dial_entry,
            &list,
            on,
        );
        d2.edit(80.0, 82.0, 117.0, 14.0, &mut user, on);
        d2.password(80.0, 103.0, 117.0, 14.0, &mut pass, on);
        d2.check(80.0, 123.0, 115.0, 10.0, &mut save, "Save password");
        if d2.button(
            201.0,
            134.0,
            50.0,
            14.0,
            "Apply",
            on && !s.dial_entry.is_empty(),
        ) {
            if let Err(e) = win::ras_save(&s.dial_entry, &user, &pass, save) {
                self.open(Dialog::Info {
                    title: "Dial Up / VPN".into(),
                    text: e,
                });
            }
        }
        d2.ui.data_mut(|m| m.insert_temp(id, (user, pass, save)));
        d2.group(7.0, 158.0, 254.0, 54.0, "Redial options");
        let weak = |t: &str| {
            if on {
                RichText::new(tr(t))
            } else {
                RichText::new(tr(t)).weak()
            }
        };
        d2.label_r(
            13.0,
            172.0,
            132.0,
            10.0,
            weak("Redial attempts (zero if endlessly):"),
        );
        d2.drag(151.0, 169.0, 28.0, 14.0, &mut s.dial_redial, 0, 999);
        d2.label(184.0, 172.0, 68.0, 10.0, weak("times"));
        d2.label_r(
            14.0,
            192.0,
            131.0,
            10.0,
            weak("Time between redial attempts:"),
        );
        d2.drag(151.0, 189.0, 28.0, 14.0, &mut s.dial_wait, 1, 3600);
        d2.label(184.0, 192.0, 69.0, 10.0, weak("seconds"));
    }

    pub(super) fn options_sounds(
        &mut self,
        ui: &mut egui::Ui,
        s: &mut Settings,
        origin: (f32, f32),
    ) {
        // Sounds (template 195, CSoundsSettings 0x5120c0): a tick and a .wav per event; Browse and
        // Play stay disabled until a row is picked. Like NDM the picked file is
        // copied next to our state, so moving or deleting the original doesn't silence the event.
        let icon = self.res.icon(274);
        let mut d2 = dlu::D::new_offset(ui, origin.0, origin.1).with_dx(PAGE_DX);
        d2.page_header(icon, 48.0, "Sound settings");
        d2.group(
            10.0,
            30.0,
            255.0,
            188.0,
            "Select sounds for New Download Manager events",
        );
        let list = d2.listview(
            15.0,
            40.0,
            244.0,
            154.0,
            &[("Event", 120.0), ("Sound file", 124.0)],
        );
        let sel_id = egui::Id::new("sound_sel");
        let mut sel: Option<usize> = d2.ui.data(|m| m.get_temp(sel_id)).flatten();
        d2.rows(
            list.shrink2(Vec2::new(4.0, 0.0))
                .with_min_y(list.min.y + 18.0),
            |ui| {
                for (k, (on, path)) in s.sounds.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        ui.add_sized(
                            [120.0 * PAGE_DX - 6.0, 18.0],
                            dlu::checkbox(on, core::settings::SOUND_EVENTS[k]),
                        );
                        let name = Path::new(path.as_str())
                            .file_name()
                            .map_or(String::new(), |n| n.to_string_lossy().into_owned());
                        if ui
                            .add_sized(
                                [124.0 * PAGE_DX - 10.0, 18.0],
                                egui::SelectableLabel::new(sel == Some(k), name),
                            )
                            .clicked()
                        {
                            sel = Some(k);
                        }
                    });
                }
            },
        );
        if d2.button(63.0, 199.0, 50.0, 14.0, "Browse...", sel.is_some()) {
            let k = sel.unwrap();
            if let Some(p) = rfd::FileDialog::new()
                .add_filter(format!("{} (*.wav)", tr_os("Sound Files")), &["wav"])
                .pick_file()
            {
                let dir = core::state::store_path().with_file_name("Sounds");
                let copy = dir.join(p.file_name().unwrap_or_default());
                let kept = copy == p
                    || (std::fs::create_dir_all(&dir).is_ok() && std::fs::copy(&p, &copy).is_ok());
                s.sounds[k] = (true, if kept { copy } else { p }.display().to_string());
            }
        }
        if d2.button(
            153.0,
            199.0,
            50.0,
            14.0,
            "Play",
            sel.is_some_and(|k| !s.sounds[k].1.is_empty()),
        ) {
            win::play_sound(&s.sounds[sel.unwrap()].1);
        }
        d2.ui.data_mut(|m| m.insert_temp(sel_id, sel));
    }

    pub(super) fn options_proxy(
        ui: &mut egui::Ui,
        s: &mut Settings,
        origin: (f32, f32),
        icon: Option<egui::TextureId>,
        adv: &mut Option<core::routing::AdvProxy>,
    ) {
        let mut d2 = dlu::D::new_offset(ui, origin.0, origin.1).with_dx(PAGE_DX);
        d2.page_header(icon, 36.0, "Proxy / socks configuration");
        // below the page, off the header (whose title ends at 255 DLU)
        if d2.button(197.0, 241.0, 63.0, 14.0, "Get System", true) {
            // Internet Options' proxy into the manual fields ("h:p" or "http=h:p;https=h:p;...")
            let (_, server, bypass) = win::system_proxy();
            let hp = server
                .split(';')
                .find_map(|p| p.strip_prefix("http="))
                .or_else(|| Some(server.as_str()).filter(|x| !x.contains('=')))
                .unwrap_or("");
            let (h, p) = hp.rsplit_once(':').unwrap_or((hp, ""));
            (s.proxy_host, s.proxy_port, s.proxy_mode) =
                (h.trim().to_string(), p.trim().to_string(), 3);
            (s.proxy_bypass_on, s.proxy_bypass) = (
                !bypass.is_empty(),
                bypass
                    .replace(';', " ")
                    .replace("<local>", "")
                    .trim()
                    .to_string(),
            );
        }
        d2.check(13.0, 27.0, 243.0, 19.0, &mut s.proxy_browser, "Use proxy/socks from a browser's request if there are download errors for the downloads intercepted from the browser");
        d2.hline(8.0, 50.0, 254.0);
        if d2.radio(7.0, 55.0, 248.0, 10.0, s.proxy_mode == 0, "No proxy/socks") {
            s.proxy_mode = 0;
        }
        d2.hline(8.0, 69.0, 254.0);
        if d2.radio(
            7.0,
            73.0,
            249.0,
            10.0,
            s.proxy_mode == 1,
            "Use system settings",
        ) {
            s.proxy_mode = 1;
        }
        d2.hline(7.0, 86.0, 254.0);
        if d2.radio(
            7.0,
            90.0,
            234.0,
            10.0,
            s.proxy_mode == 2,
            "Use automatic configuration script",
        ) {
            s.proxy_mode = 2;
        }
        d2.label(13.0, 105.0, 35.0, 9.0, "Address");
        d2.edit(53.0, 102.0, 203.0, 14.0, &mut s.pac_url, s.proxy_mode == 2);
        d2.hline(7.0, 120.0, 254.0);
        if d2.radio(
            8.0,
            124.0,
            229.0,
            10.0,
            s.proxy_mode == 3,
            "Manual proxy/socks configuration",
        ) {
            s.proxy_mode = 3;
        }
        let m = s.proxy_mode == 3;
        d2.label(13.0, 136.0, 84.0, 10.0, "Proxy server address");
        d2.label(108.0, 136.0, 34.0, 10.0, "Port");
        d2.label(146.0, 136.0, 52.0, 10.0, "UserName");
        d2.label(202.0, 136.0, 54.0, 10.0, "Password");
        d2.edit(13.0, 147.0, 91.0, 15.0, &mut s.proxy_host, m);
        d2.edit(108.0, 147.0, 34.0, 15.0, &mut s.proxy_port, m);
        d2.edit(146.0, 147.0, 52.0, 15.0, &mut s.proxy_user, m);
        d2.password(201.0, 147.0, 55.0, 15.0, &mut s.proxy_pass, m);
        d2.label(
            13.0,
            164.0,
            235.0,
            10.0,
            "Use this proxy for the following protocols:",
        );
        d2.at(13.0, 176.0, 28.0, 10.0, |ui| {
            ui.add_enabled(m, dlu::checkbox(&mut s.proxy_http, "http"))
        });
        d2.at(58.0, 176.0, 31.0, 10.0, |ui| {
            ui.add_enabled(m, dlu::checkbox(&mut s.proxy_https, "https"))
        });
        d2.at(109.0, 176.0, 24.0, 10.0, |ui| {
            ui.add_enabled(m, dlu::checkbox(&mut s.proxy_ftp, "ftp"))
        });
        if d2.button(149.0, 179.0, 107.0, 14.0, "Advanced...", m) {
            *adv = Some(s.adv.clone());
        }
        d2.group(13.0, 197.0, 248.0, 41.0, "");
        d2.at(18.0, 194.0, 238.0, 12.0, |ui| {
            ui.add_enabled(
                m,
                dlu::checkbox(
                    &mut s.proxy_bypass_on,
                    "Do not use proxy server for addresses beginning with:",
                ),
            )
        });
        d2.edit(
            22.0,
            208.0,
            234.0,
            17.0,
            &mut s.proxy_bypass,
            m && s.proxy_bypass_on,
        );
        d2.label(20.0, 227.0, 228.0, 10.0, "Separate names by spaces");
        d2.hline(7.0, 241.0, 254.0);
        d2.check(
            11.0,
            244.0,
            180.0,
            10.0,
            &mut s.ftp_pasv,
            "Use FTP in PASV mode",
        );
    }
}
