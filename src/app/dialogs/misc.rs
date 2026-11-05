use crate::app::{Align2, App, Arc, BAR, COLS, Cat, Color32, Duration, FontId, GENERAL, Mutex, Relaxed, Release, RichText, Sense, Settings, TB_BUTTONS, TB_ICONS, TITLE, Vec2, ViewportCommand, ViewportId, Which, app_icon, core, dlu, egui, help, help_logo, human, i18n, install_update, open_url, texture, tr, tr_os, win};
use crate::app::dialogs::{Dialog};

impl App {
    pub(super) fn dialog_category(
        &mut self,
        ui: &mut egui::Ui,
        orig: &mut Option<String>,
        cat: &mut core::settings::Category,
        folder: &mut String,
        error: &mut String,
    ) -> bool {
        let mut d = dlu::D::new(ui);
        d.label(9.0, 3.0, 235.0, 10.0, "Category name");
        d.edit(9.0, 13.0, 238.0, 14.0, &mut cat.name, true);
        d.label(9.0, 33.0, 238.0, 10.0, "Automatically put in this category the following file types:");
        d.edit(9.0, 43.0, 238.0, 14.0, &mut cat.exts, true);
        d.label(9.0, 58.0, 302.0, 10.0, "Note: type file extensions separated by space (e.g. avi mpg mpeg)");
        d.hline(9.0, 72.0, 302.0);
        d.check(9.0, 78.0, 302.0, 10.0, &mut cat.sites_on, "Automatically put in this category the files from the following sites only:");
        d.edit(9.0, 90.0, 302.0, 14.0, &mut cat.sites, cat.sites_on);
        d.label(9.0, 106.0, 302.0, 10.0, "Separate sites by spaces. You may use asterisk as a wildcard pattern");
        d.hline(9.0, 121.0, 302.0);
        d.label(9.0, 127.0, 302.0, 10.0, "Save future downloads of this category to the following folder:");
        d.edit(9.0, 138.0, 302.0, 14.0, folder, true);
        d.check(9.0, 155.0, 228.0, 10.0, &mut cat.remember, "Remember last save path");
        if d.button(261.0, 155.0, 50.0, 14.0, "Browse...", true) {
            if let Some(p) = rfd::FileDialog::new().set_directory(folder.as_str()).pick_folder() {
                *folder = p.display().to_string();
            }
        }
        if !error.is_empty() {
            d.label(9.0, 166.0, 240.0, 9.0, RichText::new(error.as_str()).color(Color32::RED));
        }
        let ok = d.button(261.0, 13.0, 50.0, 14.0, "OK", true);
        let cancel = d.button(261.0, 34.0, 50.0, 14.0, "Cancel", true);
        if ok {
            cat.name = cat.name.trim().to_string();
            let taken = self.core.settings().category_names().iter().any(|n| n.eq_ignore_ascii_case(&cat.name) && Some(n) != orig.as_ref());
            *error = if cat.name.is_empty() {
                "Please fill in the \"Category name\" field.".into()
            } else if taken {
                i18n::trf_os("The \"%s\" category already exists.", &[&cat.name])
            } else {
                String::new()
            };
            if error.is_empty() {
                self.save_category(orig.as_deref(), cat.clone(), folder.trim());
            }
        }
        !(ok && error.is_empty() || cancel)
    }

    pub(super) fn dialog_cat_delete(&mut self, ui: &mut egui::Ui, name: &mut String) -> bool {
        let mut d = dlu::D::new(ui);
        d.text(7.0, 6.0, 246.0, 20.0, i18n::trf_os("Do you really want to delete \"%s\" category from the categories list?", &[&name]));
        let yes = d.button(70.0, 38.0, 50.0, 14.0, "Yes", true);
        if yes {
            let mut s = self.core.settings();
            s.categories.retain(|c| c.name != *name);
            s.cat_dirs.remove(name.as_str());
            drop(s);
            // its downloads fall back to General (the files stay where they are)
            for j in self.core.jobs() {
                let mut i = j.info();
                if i.category == *name {
                    i.category = GENERAL.into();
                }
            }
            if matches!(&self.cat, Cat::Files(_, Some(c)) if c == name) {
                self.cat = Cat::Files(Which::All, None);
            }
            self.core.save();
        }
        !(yes || d.button(140.0, 38.0, 50.0, 14.0, "No", true))
    }

    pub(super) fn dialog_adv_proxy(&mut self, ui: &mut egui::Ui, adv: &mut core::routing::AdvProxy) -> bool {
        let mut d = dlu::D::new(ui);
        // one group per protocol, 41 DLU apart: label row at +10, edits at +20
        let row = |d: &mut dlu::D, y: f32, on: bool, host: &mut String, port: &mut String, user: &mut String, pass: &mut String| {
            d.label(13.0, y + 10.0, 76.0, 9.0, "Proxy");
            d.label(108.0, y + 10.0, 34.0, 9.0, "Port");
            d.label(146.0, y + 10.0, 52.0, 9.0, "UserName");
            d.label(202.0, y + 10.0, 52.0, 9.0, "Password");
            d.edit(13.0, y + 20.0, 91.0, 14.0, host, on);
            d.edit(108.0, y + 20.0, 34.0, 14.0, port, on);
            d.edit(146.0, y + 20.0, 52.0, 14.0, user, on);
            d.password(202.0, y + 20.0, 52.0, 14.0, pass, on);
        };
        // the Proxy / Socks page's HTTP proxy, repeated here as in NDM
        d.group(7.0, 3.0, 254.0, 41.0, "");
        let mut http = adv.http.take().unwrap_or_default();
        d.check(13.0, 0.0, 69.0, 12.0, &mut http.0, "Use HTTP Proxy");
        let on = http.0;
        row(&mut d, 3.0, on, &mut http.1, &mut http.2, &mut http.3, &mut http.4);
        adv.http = Some(http);
        d.group(7.0, 50.0, 254.0, 41.0, "");
        d.check(13.0, 47.0, 74.0, 12.0, &mut adv.https_on, "Use HTTPS Proxy");
        let on = adv.https_on;
        row(&mut d, 50.0, on, &mut adv.https_host, &mut adv.https_port, &mut adv.https_user, &mut adv.https_pass);
        d.group(7.0, 99.0, 254.0, 41.0, "");
        d.check(13.0, 96.0, 63.0, 12.0, &mut adv.ftp_on, "Use FTP Proxy");
        let on = adv.ftp_on;
        row(&mut d, 99.0, on, &mut adv.ftp_host, &mut adv.ftp_port, &mut adv.ftp_user, &mut adv.ftp_pass);
        d.group(7.0, 148.0, 254.0, 87.0, "");
        d.check(13.0, 145.0, 52.0, 12.0, &mut adv.socks_on, "Use Socks");
        let on = adv.socks_on;
        row(&mut d, 148.0, on, &mut adv.socks_host, &mut adv.socks_port, &mut adv.socks_user, &mut adv.socks_pass);
        if d.at(13.0, 186.0, 42.0, 10.0, |ui| ui.add_enabled(on, dlu::radio_button(adv.socks5, "Socks 5"))).clicked() {
            adv.socks5 = true;
        }
        if d.at(13.0, 199.0, 42.0, 10.0, |ui| ui.add_enabled(on, dlu::radio_button(!adv.socks5, "Socks 4"))).clicked() {
            adv.socks5 = false;
        }
        let mut dns = !adv.local_dns;
        d.at(69.0, 186.0, 50.0, 10.0, |ui| ui.add_enabled(on && adv.socks5, dlu::checkbox(&mut dns, "Proxy DNS")));
        adv.local_dns = !dns;
        d.text(14.0, 213.0, 237.0, 19.0, "Note: New Download Manager will use socks only for the protocols that are NOT checked above");
        if d.button(13.0, 240.0, 63.0, 14.0, "Get system", true) {
            // Internet Options' per-protocol proxies ("https=h:p;ftp=h:p;socks=h:p")
            let (_, server, _) = win::system_proxy();
            let get = |k: &str| server.split(';').find_map(|p| p.strip_prefix(k)).map(|hp| hp.rsplit_once(':').map_or((hp.to_string(), String::new()), |(h, p)| (h.to_string(), p.to_string())));
            if let Some((h, p)) = get("https=") {
                (adv.https_on, adv.https_host, adv.https_port) = (true, h, p);
            }
            if let Some((h, p)) = get("ftp=") {
                (adv.ftp_on, adv.ftp_host, adv.ftp_port) = (true, h, p);
            }
            if let Some((h, p)) = get("socks=") {
                (adv.socks_on, adv.socks_host, adv.socks_port) = (true, h, p);
            }
        }
        d.hline(7.0, 259.0, 254.0);
        let ok = d.button(73.0, 264.0, 50.0, 14.0, "OK", true);
        if ok {
            self.adv_done = Some(adv.clone());
        }
        !(ok || d.button(144.0, 264.0, 50.0, 14.0, "Cancel", true))
    }

    pub(super) fn dialog_find(&mut self, ui: &mut egui::Ui, text: &mut String, flags: &mut u32) -> bool {
        let mut d = dlu::D::new(ui);
        d.label(7.0, 7.0, 216.0, 8.0, "Find:");
        d.edit(7.0, 19.0, 216.0, 14.0, text, true);
        for (y, h, bit, t) in [(36.0, 10.0, 4, "File name or part of the name"), (50.0, 10.0, 8, "Description or part of the description"), (64.0, 19.0, 0x10, "Site name/Download link/Parent web page/Referer"), (97.0, 10.0, 1, "Match case"), (110.0, 10.0, 2, "Match whole string only")] {
            let mut on = *flags & bit != 0;
            d.check(if bit < 4 { 15.0 } else { 7.0 }, y, 216.0, h, &mut on, t);
            *flags = if on { *flags | bit } else { *flags & !bit };
        }
        d.hline(7.0, 88.0, 216.0);
        let ok = d.button(48.0, 127.0, 50.0, 14.0, "Find", !text.is_empty());
        if ok && *flags & 0x1c == 0 {
            // 0x4b0dc0: no search target ticked -> warn and tick "File name"
            *flags |= 4;
            self.open(Dialog::Info { title: "NDM Search settings".into(), text: "The checkbox for at least one of the search targets must be enabled".into() });
            return true;
        }
        if ok {
            let mut s = self.core.settings();
            (s.search, s.search_flags) = (text.clone(), *flags);
            drop(s);
            self.core.save();
            self.find_pos = 0;
            self.find_next();
        }
        !(ok || d.button(131.0, 127.0, 50.0, 14.0, "Cancel", true))
    }

    pub(super) fn dialog_info(ui: &mut egui::Ui, text: &str) -> bool {
        let mut d = dlu::D::new(ui);
        let h = info_height(text);
        d.text(7.0, 7.0, 246.0, h, text);
        !d.button(105.0, h + 12.0, 50.0, 14.0, "OK", true)
    }

    pub(super) fn dialog_yt_dlp(
        &mut self,
        ui: &mut egui::Ui,
        got: &mut Arc<std::sync::atomic::AtomicU64>,
        total: &mut Arc<std::sync::atomic::AtomicU64>,
        done: &mut Arc<Mutex<Option<Result<String, String>>>>,
    ) -> bool {
        let mut d = dlu::D::new(ui);
        let (g, t) = (got.load(Relaxed), total.load(Relaxed));
        let amount = if t > 0 { format!("{} / {}", human(g), human(t)) } else if g > 0 { human(g) } else { String::new() };
        // the label has the whole line (longer in other languages); the amount goes under the bar
        d.label(7.0, 4.0, 172.0, 9.0, "Downloading yt-dlp and ffmpeg");
        d.label_r(79.0, 30.0, 100.0, 9.0, RichText::new(i18n::ltr(&amount)));
        let frac = if t > 0 { g as f32 / t as f32 } else { (d.ui.input(|i| i.time) as f32 * 0.5).fract() };
        d.progress(7.0, 16.0, 172.0, 11.0, frac);
        let res = done.lock().unwrap().take();
        match res {
            Some(Ok(v)) => self.open(Dialog::Info { title: "Set yt-dlp".into(), text: i18n::trf_os("yt-dlp %s and ffmpeg are ready. NDM downloads videos from YouTube and other video sites with them, and keeps yt-dlp up to date.", &[&v]) }),
            Some(Err(e)) => self.open(Dialog::Info { title: "Set yt-dlp".into(), text: format!("{}\r\n\r\n{}", tr_os("NDM cannot download yt-dlp or ffmpeg. Check your connection and try again."), i18n::tr_msg_os(&e)) }),
            None => {
                ui.ctx().request_repaint_after(Duration::from_millis(100));
                return true;
            }
        }
        false
    }

    pub(super) fn dialog_update(&mut self, ui: &mut egui::Ui, slot: &mut Arc<Mutex<Option<Result<Option<Release>, String>>>>, quiet: &mut bool) -> bool {
        let mut d = dlu::D::new(ui);
        d.label(7.0, 4.0, 172.0, 9.0, "Checking for new version");
        let t = d.ui.input(|i| i.time) as f32;
        d.progress(7.0, 16.0, 172.0, 11.0, (t * 0.5).fract()); // marquee-like while waiting
        let res = slot.lock().unwrap().take();
        if res.is_some() {
            self.core.settings().update_checked = chrono::Local::now().timestamp();
        }
        match res {
            Some(Ok(Some(rel))) => self.open(Dialog::NewVersion { rel, get: None }),
            Some(Ok(None)) if !*quiet => self.open(Dialog::Info { title: "Quick Update".into(), text: "You are using the latest version of New Download Manager. Please check back again for updates at a later time.".into() }),
            Some(Err(e)) if !*quiet => self.open(Dialog::Info { title: "Quick Update".into(), text: i18n::tr_msg_os(&e) }),
            Some(_) => {}
            None => return true,
        }
        false
    }

    pub(super) fn dialog_new_version(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        rel: &mut Release,
        get: &mut Option<(Arc<std::sync::atomic::AtomicU64>, Arc<std::sync::atomic::AtomicU64>, Arc<Mutex<Option<Result<(), String>>>>)>,
    ) -> bool {
        let v = ui.visuals().clone();
        let accent = if v.dark_mode { Color32::from_rgb(110, 170, 255) } else { BAR };
        let (paper, line) = if v.dark_mode { (Color32::from_gray(26), Color32::from_gray(55)) } else { (Color32::WHITE, Color32::from_rgb(226, 230, 236)) };
        let logo = help_logo(ctx);
        let mut keep = true;
        egui::Frame::new().inner_margin(egui::Margin::same(18)).show(ui, |ui| {
            // header: logo, the new version and the one installed
            ui.horizontal(|ui| {
                if let Some(t) = &logo {
                    ui.add(egui::Image::new((t.id(), Vec2::splat(48.0))));
                }
                ui.add_space(8.0);
                ui.vertical(|ui| {
                    ui.label(RichText::new(tr("A new version is available")).size(19.0).strong().color(v.strong_text_color()));
                    ui.label(RichText::new(i18n::line(&format!("{} {} \u{2192} {}", tr_os("Version"), env!("CARGO_PKG_VERSION"), rel.version))).size(13.0).color(accent));
                });
            });
            ui.add_space(12.0);
            ui.label(RichText::new(tr("What's new")).size(13.0).strong());
            ui.add_space(4.0);
            // the release notes (GitHub markdown: headings, bullets, **bold**) in a card
            egui::Frame::new().fill(paper).stroke(egui::Stroke::new(1.0_f32, line)).corner_radius(8).inner_margin(egui::Margin::symmetric(12, 10)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                egui::ScrollArea::vertical().max_height(ui.available_height() - 80.0).auto_shrink(false).show(ui, |ui| {
                    let notes = if rel.notes.trim().is_empty() { tr("No release notes.") } else { rel.notes.clone() };
                    let plain = |t: &str| t.replace(['*', '`'], "");
                    for l in notes.lines().map(str::trim_end) {
                        if let Some(h) = l.strip_prefix('#') {
                            ui.add_space(4.0);
                            ui.label(RichText::new(plain(h.trim_start_matches('#').trim())).size(14.0).strong().color(v.strong_text_color()));
                        } else if let Some(b) = l.trim_start().strip_prefix("* ").or_else(|| l.trim_start().strip_prefix("- ")) {
                            ui.horizontal_wrapped(|ui| {
                                ui.label(RichText::new("\u{2022}").color(accent));
                                ui.label(plain(b));
                            });
                        } else if !l.is_empty() {
                            ui.label(plain(l));
                        }
                    }
                });
            });
            ui.add_space(14.0);
            if let Some((got, total, done)) = get {
                // downloading: a progress bar and the amount in place of the buttons
                let (g, t) = (got.load(Relaxed), total.load(Relaxed));
                let amount = if t > 0 { format!("{} / {}", human(g), human(t)) } else { human(g) };
                ui.horizontal(|ui| {
                    ui.label(RichText::new(tr("Downloading the update")).strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| ui.label(RichText::new(amount).color(v.weak_text_color())));
                });
                ui.add(egui::ProgressBar::new(if t > 0 { g as f32 / t as f32 } else { 0.0 }).fill(accent).desired_height(8.0).corner_radius(4).animate(t == 0));
                ui.add_space(4.0);
                ui.label(RichText::new(tr("New Download Manager restarts by itself when the update is installed.")).size(11.5).color(v.weak_text_color()));
                let res = done.lock().unwrap().take();
                match res {
                    Some(Ok(())) => {
                        // the setup (or the new copy) waits for this process to end: close the main
                        // window, not this dialog's, and exit anyway should that not happen
                        (self.quitting, self.updating) = (true, true);
                        ctx.send_viewport_cmd_to(ViewportId::ROOT, ViewportCommand::Close);
                        let core = self.core.clone();
                        std::thread::spawn(move || {
                            std::thread::sleep(Duration::from_secs(10));
                            core.shutdown();
                            std::process::exit(0);
                        });
                        keep = false;
                    }
                    Some(Err(e)) => {
                        self.open(Dialog::Info { title: "Quick Update".into(), text: format!("{1}{0}{2}", if e.is_empty() { "" } else { "\r\n\r\n" }, tr_os("NDM cannot download the update file.\r\nYou can download a new version of NDM from NDM home site and install it over your current version manually"), i18n::tr_msg_os(&e)) });
                        keep = false;
                    }
                    None => ui.ctx().request_repaint_after(Duration::from_millis(100)),
                }
                return;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let big = Vec2::new(110.0, 30.0);
                let now = ui.add_enabled(!rel.exe.is_empty(), egui::Button::new(RichText::new(tr("Update now")).strong().color(Color32::WHITE)).fill(accent).corner_radius(6).min_size(big));
                if now.clicked() {
                    let (got, total, done) = (Arc::new(std::sync::atomic::AtomicU64::new(0)), Arc::new(std::sync::atomic::AtomicU64::new(0)), Arc::new(Mutex::new(None)));
                    let (agent, url, g, t, dn, c) = (self.core.agent(&rel.exe), rel.exe.clone(), got.clone(), total.clone(), done.clone(), ctx.clone());
                    std::thread::spawn(move || {
                        *dn.lock().unwrap() = Some(install_update(&agent, &url, &g, &t));
                        c.request_repaint();
                    });
                    *get = Some((got, total, done));
                }
                if ui.add(egui::Button::new(tr("Later")).corner_radius(6).min_size(big)).clicked() {
                    keep = false;
                }
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    // the release's page, to download by hand (the only way when no program for this system is attached)
                    if ui.link(tr("Download page")).clicked() {
                        open_url(&rel.page);
                    }
                });
            });
        });
        keep
    }

    pub(super) fn dialog_virus(&mut self, ui: &mut egui::Ui, exe: &mut String, args: &mut String) -> bool {
        let mut d = dlu::D::new(ui);
        d.label(7.0, 10.0, 248.0, 10.0, "Virus scanner program");
        d.edit(7.0, 20.0, 299.0, 14.0, exe, true);
        if d.button(322.0, 20.0, 50.0, 14.0, "Browse", true) {
            if let Some(p) = rfd::FileDialog::new().add_filter("exe", &["exe"]).pick_file() {
                *exe = p.display().to_string();
            }
        }
        d.label(7.0, 38.0, 248.0, 10.0, "Command line parameters");
        d.edit(7.0, 48.0, 299.0, 14.0, args, true);
        d.text(7.0, 68.0, 365.0, 142.0, format!("{}\n\n{}", tr_os("If you would like to run antiviral software on download completion, you should type or browse the complete path to its executable file. If NDM knows about necessary parameters for a given executable file name, it will set them automatically. Otherwise, you will have to set them manually in \"Command line parameters\" according to antiviral software documentation. The scanned file among different parameters should be written as\r\n[File]\r\n(you should type square brackets as well). If no parameters are set, NDM will execute antiviral software appending the complete path of a downloaded file."), tr_os("Note that all modern antiviral software have \"real-time protection\" feature that checks all operations with files on your computer, and it includes all files downloaded in NDM. Thus, you need to add antiviral software scanner to NDM settings only when real-time protection is turned off in antiviral software settings.\r\nAlso, in some cases, real time protection may significantly slow down the assembly of files made by NDM. Then you need to add ndm.exe process to exceptions according to antiviral software documentation, and set up antiviral checking on this dialog.")));
        let ok = d.button(110.0, 214.0, 50.0, 14.0, "OK", true);
        if ok {
            let v = (exe.trim().to_string(), args.trim().to_string());
            self.opt_patch.push(Box::new(move |s: &mut Settings| (s.virus_exe, s.virus_args) = v));
        }
        !(ok || d.button(219.0, 214.0, 50.0, 14.0, "Cancel", true))
    }

    pub(super) fn dialog_export(&mut self, ui: &mut egui::Ui, ef2: &mut bool, mode: &mut u8) -> bool {
        let mut d = dlu::D::new(ui);
        for (k, y, t) in [(0, 6.0, "Export download queue"), (1, 18.0, "Export selected files"), (2, 30.0, "Export all files")] {
            if d.radio(11.0, y, 141.0, 10.0, *mode == k, t) {
                *mode = k;
            }
        }
        let ok = d.button(157.0, 6.0, 50.0, 14.0, "OK", true);
        if ok {
            self.export(*ef2, *mode);
        }
        !(ok || d.button(157.0, 24.0, 50.0, 14.0, "Cancel", true))
    }

    pub(super) fn dialog_columns(&mut self, ui: &mut egui::Ui, list: &mut Vec<(u8, bool, u16)>, sel: &mut usize, width: &mut String) -> bool {
        let mut d = dlu::D::new(ui);
        d.text(7.0, 7.0, 222.0, 25.0, "Check the columns that you would like visible in this list. Use the Move Up and Move Down buttons to reorder the columns however you like.");
        let r = d.listview(7.0, 36.0, 139.0, 87.0, &[]);
        d.ui.scope_builder(egui::UiBuilder::new().max_rect(r.shrink(2.0)), |ui| {
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for (k, (c, vis, w)) in list.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        // File Name can't be hidden
                        ui.add_enabled(*c != 0, egui::Checkbox::without_text(vis));
                        if ui.add_sized([ui.available_width(), 16.0], egui::SelectableLabel::new(*sel == k, tr(COLS[*c as usize].1))).clicked() {
                            (*sel, *width) = (k, w.to_string());
                        }
                    });
                }
            });
        });
        let n = list.len();
        if d.button(158.0, 36.0, 71.0, 14.0, "Move Up", *sel > 0) {
            list.swap(*sel, *sel - 1);
            *sel -= 1;
        }
        if d.button(158.0, 54.0, 71.0, 14.0, "Move Down", *sel + 1 < n) {
            list.swap(*sel, *sel + 1);
            *sel += 1;
        }
        let (c, vis, _) = list[*sel];
        if d.button(158.0, 72.0, 71.0, 14.0, "Show", !vis) {
            list[*sel].1 = true;
        }
        if d.button(158.0, 90.0, 71.0, 14.0, "Hide", vis && c != 0) {
            list[*sel].1 = false;
        }
        if d.button(158.0, 108.0, 71.0, 14.0, "Reset", true) {
            *list = core::settings::DEFAULT_COLUMNS.iter().map(|(k, w)| (*k, true, *w)).collect();
            list.extend(COLS.iter().enumerate().filter(|(k, _)| !core::settings::DEFAULT_COLUMNS.iter().any(|c| c.0 as usize == *k)).map(|(k, c)| (k as u8, false, c.2)));
            (*sel, *width) = (0, list[0].2.to_string());
        }
        d.label_r(7.0, 132.0, 123.0, 10.0, "The selected column should be");
        if d.edit(136.0, 130.0, 20.0, 14.0, width, true).changed() {
            if let Some(w) = width.trim().parse::<u16>().ok().filter(|w| (10..=2000).contains(w)) {
                list[*sel].2 = w;
            }
        }
        d.label(159.0, 132.0, 66.0, 10.0, "pixels wide");
        d.hline(7.0, 149.0, 222.0);
        let ok = d.button(107.0, 155.0, 50.0, 14.0, "OK", true);
        if ok {
            self.core.settings().columns = list.iter().filter(|c| c.1).map(|c| (c.0, c.2)).collect();
            self.core.save();
        }
        !(ok || d.button(179.0, 155.0, 50.0, 14.0, "Cancel", true))
    }

    pub(super) fn dialog_scale(&mut self, ui: &mut egui::Ui, orig: &mut [f32; 3]) -> bool {
        let mut d = dlu::D::new(ui);
        d.text(7.0, 6.0, 218.0, 18.0, "Change the size of the toolbar, the categories pane and the text of the main window. Changes show at once.");
        let before = { let s = self.core.settings(); [s.scale_toolbar, s.scale_tree, s.scale_text] };
        let mut v = before;
        for (name, k) in [("Toolbar", 0), ("Categories pane", 1), ("Text size", 2)] {
            let y = 28.0 + k as f32 * 15.0;
            d.label(7.0, y + 2.0, 66.0, 9.0, name);
            let mut pct = (v[k] * 100.0).round() as i32;
            d.at(76.0, y, 149.0, 13.0, |ui| {
                ui.spacing_mut().slider_width = (ui.available_width() - 58.0).max(40.0);
                ui.add(egui::Slider::new(&mut pct, 50..=200).step_by(5.0).suffix("%").trailing_fill(true));
            });
            v[k] = pct as f32 / 100.0;
        }
        d.hline(7.0, 76.0, 218.0);
        if d.button(7.0, 81.0, 50.0, 14.0, "Reset", v != [1.0; 3]) {
            v = [1.0; 3];
        }
        let ok = d.button(120.0, 81.0, 50.0, 14.0, "OK", true);
        let cancel = d.button(175.0, 81.0, 50.0, 14.0, "Cancel", true);
        if cancel {
            v = *orig;
        }
        if v != before {
            let mut s = self.core.settings();
            [s.scale_toolbar, s.scale_tree, s.scale_text] = v;
        }
        if ok || cancel {
            self.core.save();
        }
        !(ok || cancel)
    }

    pub(super) fn dialog_about(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) -> bool {
        let icon = texture(ctx, "app_icon", &app_icon());
        let mut d = dlu::D::new(ui);
        d.ui.put(d.rect(7.0, 6.0, 20.0, 20.0), egui::Image::new((icon.id(), Vec2::splat(32.0))));
        d.at(33.0, 3.0, 183.0, 16.0, |ui| ui.label(RichText::new(TITLE.trim_end_matches(" 1.0")).size(14.0).strong()));
        d.label(32.0, 19.0, 208.0, 8.0, i18n::line(&format!("{} {}", tr_os("Version:"), env!("CARGO_PKG_VERSION"))));
        if d.button(75.0, 31.0, 99.0, 14.0, "Check for Update", true) {
            self.check_updates(ctx, false);
        }
        !d.button(102.0, 212.0, 55.0, 14.0, "Close", true)
    }

    pub(super) fn dialog_tb_customize(&mut self, ui: &mut egui::Ui, layout: &mut Vec<i8>, avail: &mut Option<usize>, cur: &mut Option<usize>) -> bool {
        let mut d = dlu::D::new(ui);
        // available: a separator always, plus the buttons not on the bar (comctl32's lists)
        let left: Vec<i8> = std::iter::once(-1).chain((0..TB_ICONS as i8).filter(|b| !layout.contains(b))).collect();
        let dark = d.ui.visuals().dark_mode;
        let pics: Vec<Option<(egui::TextureId, Color32)>> = (0..TB_ICONS).map(|k| self.toolbar.as_ref().and_then(|t| t.pic(true, k, true, false, dark))).collect();
        let list = |ui: &mut egui::Ui, items: &[i8], sel: &mut Option<usize>| {
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for (k, b) in items.iter().enumerate() {
                    let r = ui.allocate_exact_size(Vec2::new(ui.available_width(), 22.0), Sense::click());
                    if *sel == Some(k) {
                        ui.painter().rect_filled(r.0, 0.0, ui.visuals().selection.bg_fill);
                    }
                    let label = match usize::try_from(*b) {
                        Ok(b) => {
                            if let Some((t, tint)) = pics[b] {
                                ui.painter().image(t, egui::Rect::from_min_size(r.0.min + Vec2::new(2.0, 2.0), Vec2::splat(18.0)), egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), tint);
                            }
                            i18n::tr_label(TB_BUTTONS[b].1)
                        }
                        Err(_) => tr("Separator"),
                    };
                    ui.painter().text(r.0.left_center() + Vec2::new(26.0, 0.0), Align2::LEFT_CENTER, label, FontId::proportional(12.0), ui.visuals().text_color());
                    if r.1.clicked() {
                        *sel = Some(k);
                    }
                }
            });
        };
        d.label(4.0, 5.0, 84.0, 10.0, "Available toolbar buttons:");
        let r = d.listview(4.0, 17.0, 120.0, 100.0, &[]);
        d.ui.scope_builder(egui::UiBuilder::new().max_rect(r.shrink(2.0)), |ui| list(ui, &left, avail));
        d.label(182.0, 5.0, 78.0, 10.0, "Current toolbar buttons:");
        let r = d.listview(182.0, 17.0, 120.0, 100.0, &[]);
        d.ui.scope_builder(egui::UiBuilder::new().max_rect(r.shrink(2.0)), |ui| list(ui, layout, cur));
        let before = layout.clone();
        if d.button(131.0, 42.0, 44.0, 14.0, &i18n::line(&format!("{} ->", tr_os("Add"))), avail.is_some_and(|a| a < left.len())) {
            // inserted before the selected current button, like comctl32
            let at = cur.map_or(layout.len(), |c| c.min(layout.len()));
            layout.insert(at, left[avail.unwrap()]);
            (*avail, *cur) = (None, Some(at));
        }
        if d.button(131.0, 62.0, 44.0, 14.0, &i18n::line(&format!("<- {}", tr_os("Remove"))), cur.is_some_and(|c| c < layout.len())) {
            layout.remove(cur.unwrap());
            *cur = None;
        }
        let close = d.button(308.0, 6.0, 44.0, 14.0, "Close", true);
        if d.button(308.0, 23.0, 44.0, 14.0, "Reset", true) {
            *layout = core::settings::Settings::default().tb_layout;
            (*avail, *cur) = (None, None);
        }
        if d.button(308.0, 40.0, 44.0, 14.0, "Help", help("index.htm", false)) {
            help("customization.htm", true);
        }
        if let Some(c) = cur.filter(|c| *c < layout.len()) {
            if d.button(308.0, 74.0, 44.0, 14.0, "Move Up", c > 0) {
                layout.swap(c, c - 1);
                *cur = Some(c - 1);
            }
            if d.button(308.0, 91.0, 44.0, 14.0, "Move Down", c + 1 < layout.len()) {
                layout.swap(c, c + 1);
                *cur = Some(c + 1);
            }
        } else {
            d.button(308.0, 74.0, 44.0, 14.0, "Move Up", false);
            d.button(308.0, 91.0, 44.0, 14.0, "Move Down", false);
        }
        // comctl32 applies every change to the bar at once
        if *layout != before {
            self.core.settings().tb_layout = layout.clone();
            self.core.save();
        }
        !close
    }

    pub(super) fn dialog_tip(&mut self, ctx: &egui::Context, ui: &mut egui::Ui, text: &mut String) -> bool {
        let mut d = dlu::D::new(ui);
        // NDM's OnPaint (0x54d200): the SS_BLACKFRAME box, white from the bulb static (20,17) to the
        // frame's far corner less the same inset, bitmap 104 at its own size; black text on the white
        let edge = dlu::pal(d.ui).edge;
        d.ui.painter().rect_stroke(d.rect(12.0, 11.0, 228.0, 123.0), 0.0, egui::Stroke::new(1.0_f32, edge), egui::StrokeKind::Inside);
        d.ui.painter().rect_filled(d.rect(20.0, 17.0, 212.0, 111.0), 0.0, Color32::WHITE);
        if let Some(t) = self.res.tree.get(&104) {
            let at = d.rect(20.0, 17.0, 1.0, 1.0).min;
            d.ui.painter().image(t.id(), egui::Rect::from_min_size(at, t.size_vec2() / ctx.pixels_per_point()), egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
        }
        d.label(55.0, 29.0, 168.0, 12.0, RichText::new(tr("Did you know...")).color(Color32::BLACK));
        // black on the tip's white panel (in reading order: the helper wraps it)
        let ink = d.ui.visuals().override_text_color;
        d.ui.visuals_mut().override_text_color = Some(Color32::BLACK);
        d.text(28.0, 57.0, 196.0, 66.0, text.as_str());
        d.ui.visuals_mut().override_text_color = ink;
        let mut on = self.core.settings().tips_startup;
        if d.check(12.0, 146.0, 101.0, 10.0, &mut on, "Show Tips on StartUp").changed() {
            self.core.settings().tips_startup = on;
            self.core.save();
        }
        if d.button(121.0, 143.0, 54.0, 14.0, "Next Tip", true) {
            *text = self.next_tip();
        }
        !d.button(183.0, 143.0, 54.0, 14.0, "Close", true)
    }
}

/// The height (DLU) of a message window's text: room for about 55 characters a line, at least the 38 of NDM's
/// own message windows (translations run longer, so the translated text is measured).
pub(in crate::app) fn info_height(text: &str) -> f32 {
    let lines: usize = tr(text).split('\n').map(|l| l.chars().count() / 55 + 1).sum();
    (lines as f32 * 9.0).max(38.0)
}
