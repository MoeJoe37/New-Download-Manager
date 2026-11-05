use crate::app::{App, Arc, Color32, GRAB_MENU, GrabCmd, HashSet, Rect, Relaxed, RichText, Sense, Status, Vec2, core, dlu, egui, help, i18n, menu_layout, open_url, sched_time, tr, tr_os};
use crate::app::dialogs::{Dialog};

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
        d.label(7.0, 6.0, 204.0, 9.0, "Save this project with the following name");
        d.edit(7.0, 19.0, 204.0, 14.0, name, true);
        if orig.is_none() {
            d.check(18.0, 35.0, 185.0, 10.0, quiet, "Don't show this dialog if the name is unique");
        }
        let ok = d.button(53.0, 54.0, 50.0, 14.0, "OK", !name.trim().is_empty());
        if ok {
            if orig.is_none() {
                self.core.settings().grab_quiet = *quiet;
            }
            let p = proj.take().map(|p| *p);
            let n = name.trim().to_string();
            let taken = orig.is_none() && p.as_ref().is_some_and(|p| p.opts.name.trim() != n) && self.core.settings().grab_projects.iter().any(|x| x.opts.name == n);
            if taken {
                // 20615: another project already has this name
                *proj = p.map(Box::new);
                self.open(Dialog::Info { title: "Saving the project of NDM Grabber".into(), text: i18n::trf_os("The project with \"%s\" name already exists. Please select another name.", &[&n]) });
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

    pub(super) fn dialog_grab_sched_ask(&mut self, ui: &mut egui::Ui, proj: &mut Box<core::settings::GrabProject>) -> bool {
        let mut d = dlu::D::new(ui);
        d.text(7.0, 7.0, 246.0, 20.0, "To use project scheduler you should save the project at first. Save the project now?");
        let yes = d.button(140.0, 40.0, 50.0, 14.0, "Yes", true);
        if yes {
            let p = (**proj).clone();
            let name = p.opts.name.trim().to_string();
            let quiet = self.core.settings().grab_quiet;
            self.open(Dialog::GrabSave { name, orig: None, quiet, proj: Some(Box::new(p)), sched: true });
        }
        !(yes || d.button(203.0, 40.0, 50.0, 14.0, "No", true))
    }

    pub(super) fn dialog_grab_sched(&mut self, ui: &mut egui::Ui, p: &mut Box<core::settings::GrabProject>) -> bool {
        let mut d = dlu::D::new(ui);
        if let Some(t) = self.res.icon(285) {
            let r = d.rect(7.0, 7.0, 20.0, 20.0);
            d.ui.put(r, egui::Image::new((t, Vec2::splat(32.0))));
        }
        d.label(41.0, 6.0, 124.0, 9.0, "Project:");
        d.label(42.0, 18.0, 127.0, 9.0, p.opts.name.clone());
        d.group(7.0, 32.0, 190.0, 43.0, "Step 1. Select type");
        for (y, v, t) in [(45.0, false, "One-time exploring/downloading"), (58.0, true, "Periodic synchronization")] {
            if d.radio(11.0, y, 182.0, 10.0, p.periodic == v, t) {
                p.periodic = v;
            }
        }
        d.group(7.0, 82.0, 191.0, 58.0, "Step 2. Select action");
        for (y, v, t) in [(97.0, 0, "Explore only"), (111.0, 1, "Explore site and download matched files"), (123.0, 2, "Download checked files")] {
            if d.radio(11.0, y, 179.0, 10.0, p.action == v, t) {
                p.action = v;
            }
        }
        d.group(207.0, 7.0, 223.0, 133.0, "Step 3. Schedule");
        let sc = &mut p.sched;
        d.check(217.0, 19.0, 95.0, 10.0, &mut sc.start_on, "Start download at");
        d.edit(318.0, 17.0, 59.0, 15.0, &mut sc.start_at, sc.start_on);
        if p.periodic {
            d.check(217.0, 36.0, 90.0, 10.0, &mut sc.again_on, "Start again every");
            let (mut h, mut m) = (sc.again_h.to_string(), sc.again_m.to_string());
            if d.edit(310.0, 35.0, 19.0, 14.0, &mut h, sc.again_on).changed() {
                sc.again_h = h.trim().parse().unwrap_or(0);
            }
            d.label(332.0, 38.0, 25.0, 8.0, "hours");
            if d.edit(362.0, 35.0, 19.0, 14.0, &mut m, sc.again_on).changed() {
                sc.again_m = m.trim().parse().unwrap_or(0);
            }
            d.label(383.0, 38.0, 23.0, 8.0, "min");
        } else {
            if d.radio(225.0, 36.0, 54.0, 10.0, !sc.daily, "Once at") {
                sc.daily = false;
            }
            if d.radio(225.0, 52.0, 45.0, 10.0, sc.daily, "Daily") {
                sc.daily = true;
            }
            let days = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
            for (k, day) in days.iter().enumerate() {
                let (x, y) = if k < 4 { (275.0, [52.0, 63.0, 73.0, 83.0][k]) } else { (353.0, [52.0, 63.0, 73.0][k - 4]) };
                let daily = sc.daily;
                d.at(x, y, 72.0, 10.0, |ui| ui.add_enabled(daily, dlu::checkbox(&mut sc.days[k], day)));
            }
        }
        d.hline(217.0, 94.0, 197.0);
        d.check(217.0, 100.0, 97.0, 10.0, &mut sc.stop_on, "Stop download at");
        d.edit(318.0, 97.0, 59.0, 14.0, &mut sc.stop_at, sc.stop_on);
        d.hline(218.0, 114.0, 197.0);
        d.text(212.0, 118.0, 214.0, 18.0, "Note: NDM should be running in system tray at the specified time to start a scheduled project");
        d.check(9.0, 145.0, 185.0, 10.0, &mut sc.hangup, "Hang up modem when done");
        d.check(9.0, 158.0, 191.0, 10.0, &mut sc.exit_done, "Exit New Download Manager when done");
        d.check(9.0, 170.0, 188.0, 10.0, &mut sc.off_done, "Turn off computer when done");
        let kinds = ["Shut down", "Sleep", "Hibernate"].map(tr);
        let mut kind = kinds[sc.off_kind as usize % 3].clone();
        if d.combo(23.0, 182.0, 104.0, 14.0, "goff", &mut kind, &kinds.each_ref().map(String::as_str), sc.off_done) {
            sc.off_kind = kinds.iter().position(|k| *k == kind).unwrap_or(0) as u8;
        }
        let off = sc.off_done;
        d.at(23.0, 200.0, 158.0, 10.0, |ui| ui.add_enabled(off, dlu::checkbox(&mut sc.off_force, "Force processes to terminate")));
        d.check(212.0, 146.0, 213.0, 10.0, &mut sc.open_on, "Open the following file when done:");
        d.edit(211.0, 158.0, 198.0, 14.0, &mut sc.open_file, sc.open_on);
        if d.button(412.0, 158.0, 15.0, 14.0, "...", sc.open_on) {
            if let Some(f) = rfd::FileDialog::new().pick_file() {
                sc.open_file = f.display().to_string();
            }
        }
        let t = |s: &str| sched_time(s).map(|t| t.format("%H:%M").to_string());
        let times = (t(&sc.start_at), t(&sc.stop_at));
        let ok = d.button(296.0, 198.0, 50.0, 14.0, "OK", times.0.is_some() && times.1.is_some());
        if ok {
            (sc.start_at, sc.stop_at) = (times.0.unwrap(), times.1.unwrap());
            let mut p = (**p).clone();
            p.opts.overwrite |= p.periodic; // grabber.chm "Scheduler": synchronization replaces changed files
            let periodic_now = p.periodic && p.sched.again_on && !p.sched.start_on;
            if let Some(x) = self.core.settings().grab_projects.iter_mut().find(|x| x.opts.name == p.opts.name) {
                *x = p.clone();
            }
            self.core.save();
            if periodic_now {
                self.core.run_project(p); // no start time: the first pass now, then every N hours
            }
        }
        !(ok || d.button(357.0, 198.0, 50.0, 14.0, "Cancel", true))
    }

    pub(super) fn dialog_grab_login(
        &mut self,
        ui: &mut egui::Ui,
        realm: &str,
        site: &str,
        user: &mut String,
        pass: &mut String,
        save: &mut bool,
    ) -> bool {
        let mut d = dlu::D::new(ui);
        if let Some(t) = self.res.icon(272) {
            let r = d.rect(10.0, 7.0, 20.0, 20.0);
            d.ui.put(r, egui::Image::new((t, Vec2::splat(32.0))));
        }
        d.label(40.0, 9.0, 223.0, 10.0, "Type your user name and password.");
        d.label(40.0, 28.0, 225.0, 10.0, i18n::line(&format!("{} {}", tr_os("Realm:"), i18n::ltr(realm))));
        d.label(40.0, 48.0, 73.0, 10.0, "User name:");
        d.edit(117.0, 46.0, 146.0, 14.0, user, true);
        d.label(40.0, 69.0, 72.0, 10.0, "Password:");
        d.password(117.0, 66.0, 146.0, 14.0, pass, true);
        d.check(40.0, 93.0, 216.0, 10.0, save, "Save this password in NDM password list");
        let ok = d.button(155.0, 117.0, 50.0, 14.0, "OK", !user.is_empty());
        if ok {
            if *save {
                self.core.settings().logins.push(core::settings::Login { site: site.to_owned(), user: user.clone(), pass: pass.clone() });
                self.core.save();
            }
            self.grab_auth = Some((user.clone(), pass.clone()));
        }
        !(ok || d.button(212.0, 117.0, 50.0, 14.0, "Cancel", true))
    }

    pub(super) fn dialog_grabber(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        step: &mut u8,
        adv: &mut bool,
        node: &mut usize,
        opts: &mut crate::grabber::Opts,
        save_mode: &mut u8,
        save_cat: &mut String,
        folder: &mut String,
        crawl: &mut Option<Arc<crate::grabber::Crawl>>,
    ) -> bool {
        if let Some(n) = self.grab_load.take() {
            self.project_cmd(0, &n);
            return false;
        }
        if let Some(n) = self.grab_saved_as.take() {
            opts.name = n;
        }
        if let Some(ck) = self.grab_cookies.take() {
            opts.cookies = ck;
            *crawl = Some(self.crawl(ctx, opts));
        }
        if let Some(c) = crawl.clone() {
            let (finished, login) = { let s = c.state.lock().unwrap(); (s.done, s.need_login.clone()) };
            if let Some(realm) = login {
                // "Connecting to the site" (template 279): ask once, then explore again with the login
                c.state.lock().unwrap().need_login = None;
                let site = url::Url::parse(opts.start.trim()).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default();
                self.open(Dialog::GrabLogin { realm, site, user: opts.login.clone(), pass: String::new(), save: false });
            }
            if let Some((user, pass)) = self.grab_auth.take() {
                (opts.auth, opts.login, opts.pass) = (true, user, pass);
                *crawl = Some(self.crawl(ctx, opts));
                return true;
            }
            if opts.start_all && finished && !c.auto_started.swap(true, Relaxed) {
                self.grab(&c, opts, *save_mode, save_cat, folder, true);
            }
            let (keep, back, again) = self.grabber_results(ctx, ui, &c, opts, *save_mode, save_cat, folder, node);
            if back {
                c.stop.store(true, Relaxed);
                (*crawl, *step) = (None, 0);
            }
            if again {
                c.stop.store(true, Relaxed);
                *crawl = Some(self.crawl(ctx, opts));
            }
            if !keep && opts.add_on_close {
                self.grab(&c, opts, *save_mode, save_cat, folder, false);
            }
            if !keep && !opts.name.trim().is_empty() {
                // closing the results window saves the project (template 269 unless "don't show")
                let st = c.state.lock().unwrap();
                let proj = core::settings::GrabProject { opts: opts.clone(), save_mode: *save_mode, save_cat: save_cat.clone(), folder: folder.clone(), files: st.files.clone(), pages: st.pages, ..Default::default() };
                drop(st);
                let name = opts.name.trim().to_string();
                let (quiet, taken) = { let s = self.core.settings(); (s.grab_quiet, s.grab_projects.iter().any(|p| p.opts.name == name)) };
                if quiet && !taken {
                    self.save_project(&name, None, Some(proj));
                } else {
                    self.open(Dialog::GrabSave { name, orig: None, quiet, proj: Some(Box::new(proj)), sched: false });
                }
            }
            return keep;
        }
        // NDM's wizard (template 239 + pages 257 / 259 / 264 / 240) under its menu bar (menu 256)
        let saved = self.core.settings().grab_projects.iter().any(|p| p.opts.name == opts.name.trim());
        let full = ui.max_rect();
        let cmd = {
            let mut bar = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(full.min, Vec2::new(full.width(), GRAB_MENU))).layout(menu_layout()));
            self.grab_menu(&mut bar)
        };
        match cmd {
            Some(GrabCmd::New) => {
                (*opts, *step, *adv) = (self.new_grab_opts(), 0, false);
                return true;
            }
            Some(GrabCmd::Save) => {
                let proj = core::settings::GrabProject { opts: opts.clone(), save_mode: *save_mode, save_cat: save_cat.clone(), folder: folder.clone(), ..Default::default() };
                self.open(Dialog::GrabSave { name: opts.name.trim().to_string(), orig: None, quiet: false, proj: Some(Box::new(proj)), sched: false });
            }
            Some(GrabCmd::SaveTpl) => {
                let proj = core::settings::GrabProject { opts: opts.clone(), save_mode: *save_mode, save_cat: save_cat.clone(), folder: folder.clone(), ..Default::default() };
                self.open(Dialog::GrabTpl { name: String::new(), proj: Box::new(proj), replace: false });
            }
            Some(GrabCmd::Recent(n)) => {
                self.project_cmd(0, &n);
                return false;
            }
            Some(GrabCmd::Schedule) => {
                let proj = core::settings::GrabProject { opts: opts.clone(), save_mode: *save_mode, save_cat: save_cat.clone(), folder: folder.clone(), ..Default::default() };
                self.grab_schedule(proj, saved);
            }
            Some(GrabCmd::Close) => return false,
            None => {}
        }
        let mut body = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_max(full.min + Vec2::new(0.0, GRAB_MENU), full.max)));
        let mut d = dlu::D::new(&mut body);
        // the picture on the left (static 1801, in a black frame), cropped to fill it without stretching
        let pic = d.rect(7.0, 7.0, 85.0, 160.0);
        match self.res.grab_pic.as_ref() {
            Some(t) => {
                let [w, h] = t.size().map(|v| v as f32);
                let (box_a, img_a) = (pic.width() / pic.height(), w / h);
                let uv = if img_a > box_a {
                    let k = box_a / img_a;
                    Rect::from_min_max(egui::pos2((1.0 - k) / 2.0, 0.0), egui::pos2((1.0 + k) / 2.0, 1.0))
                } else {
                    let k = img_a / box_a;
                    Rect::from_min_max(egui::pos2(0.0, (1.0 - k) / 2.0), egui::pos2(1.0, (1.0 + k) / 2.0))
                };
                d.ui.painter().image(t.id(), pic, uv, Color32::WHITE);
            }
            None => drop(d.ui.painter().rect_filled(pic, 0.0, Color32::from_rgb(28, 93, 178))),
        }
        d.ui.painter().rect_stroke(pic, 0.0, egui::Stroke::new(1.0_f32, Color32::BLACK), egui::StrokeKind::Inside);
        let adv_label = i18n::line(&if *adv { format!("<< {}", tr_os("Advanced")) } else { format!("{} >>", tr_os("Advanced")) });
        // NDM's own English where its language files still carry older words
        let en = |new: &str, old: &str| if i18n::current().is_empty() { new.to_string() } else { tr(old) };
        match *step {
            0 => {
                d.label(106.0, 2.0, 164.0, 9.0, "Grabber Project Name");
                d.edit(106.0, 12.0, 166.0, 14.0, &mut opts.name, true);
                d.label(106.0, 29.0, 287.0, 9.0, "Start page/address");
                d.edit(106.0, 39.0, 289.0, 14.0, &mut opts.start, true);
                d.label(106.0, 56.0, 147.0, 9.0, "Project template");
                let mut names: Vec<String> = crate::grabber::TEMPLATES.iter().map(|t| t.to_string()).collect();
                names.extend(self.core.settings().grab_templates.iter().map(|t| t.opts.name.clone()));
                let shown: Vec<String> = names.iter().enumerate().map(|(k, n)| if k < crate::grabber::TEMPLATES.len() { tr(n) } else { n.clone() }).collect();
                let cur_k = names.iter().position(|n| *n == opts.template).unwrap_or(0);
                let mut cur = shown[cur_k].clone();
                if d.combo(106.0, 66.0, 289.0, 14.0, "gtpl", &mut cur, &shown.iter().map(String::as_str).collect::<Vec<_>>(), true) {
                    if let Some(k) = shown.iter().position(|s| *s == cur) {
                        self.apply_template(&names[k], opts, save_mode, save_cat);
                    }
                }
                d.text(106.0, 80.0, 290.0, 26.0, "If you select a project template, the wizard will make the required project settings for the selected template on the next steps. Note that you may always change any settings manually.");
                d.hline(106.0, 111.0, 289.0);
                d.check(106.0, 115.0, 161.0, 10.0, &mut opts.auth, "Use authorization");
                d.label_r(107.0, 131.0, 50.0, 9.0, "Login");
                d.edit(160.0, 128.0, 74.0, 14.0, &mut opts.login, opts.auth);
                d.label_r(240.0, 131.0, 61.0, 9.0, "Password");
                d.password(305.0, 128.0, 73.0, 14.0, &mut opts.pass, opts.auth);
                d.text(106.0, 150.0, 188.0, 17.0, "Press Advanced button to enable manual login or to disable a logout page");
                if d.button(305.0, 153.0, 73.0, 14.0, &adv_label, true) {
                    *adv = !*adv;
                }
                if *adv {
                    d.hline(7.0, 176.0, 396.0);
                    d.check(105.0, 184.0, 289.0, 10.0, &mut opts.manual_on, "Enter login and password manually at the following web page:");
                    d.edit(105.0, 196.0, 290.0, 14.0, &mut opts.manual, opts.manual_on);
                    d.check(106.0, 220.0, 288.0, 10.0, &mut opts.logout_on, "Don't open the logout page:");
                    d.edit(105.0, 232.0, 290.0, 14.0, &mut opts.logout, opts.logout_on);
                    d.text(106.0, 248.0, 286.0, 24.0, "Many sites have a logout page(s) that the Grabber should not open.  You may use asterisk wildcard here to specify a pattern for different logout pages.");
                }
            }
            1 => {
                d.group(103.0, 1.0, 294.0, 123.0, "Save To");
                if d.radio(110.0, 8.0, 268.0, 22.0, *save_mode == 0, &en("Every file to a folder according to NDM category of the file", "Every file to the directory according to the category of the file")) {
                    *save_mode = 0;
                }
                if d.radio(110.0, 33.0, 270.0, 10.0, *save_mode == 1, &en("All files to the folder associated with the following NDM category:", "All files to one category")) {
                    *save_mode = 1;
                }
                let cat_names = self.core.settings().category_names();
                let cats: Vec<&str> = cat_names.iter().map(String::as_str).collect();
                d.combo(130.0, 43.0, 130.0, 14.0, "gcat", save_cat, &cats, *save_mode == 1);
                let cat_dir = self.core.settings().folder_for(save_cat);
                d.edit(130.0, 58.0, 256.0, 14.0, &mut cat_dir.clone(), false);
                if d.radio(110.0, 79.0, 186.0, 10.0, *save_mode == 2, &en("All files to the following folder", "All files to one directory")) {
                    *save_mode = 2;
                }
                let m2 = *save_mode == 2;
                d.edit(130.0, 89.0, 256.0, 14.0, folder, m2);
                d.at(130.0, 107.0, 198.0, 10.0, |ui| ui.add_enabled(m2, dlu::checkbox(&mut opts.subfolders, "Use original relative subfolders")));
                if d.button(331.0, 106.0, 55.0, 14.0, "Browse...", m2) {
                    if let Some(p) = rfd::FileDialog::new().set_directory(folder.as_str()).pick_folder() {
                        *folder = p.display().to_string();
                    }
                }
                d.check(110.0, 128.0, 280.0, 10.0, &mut opts.convert, "Convert the links in downloaded html files to the local files for offline browsing");
                d.check(110.0, 144.0, 282.0, 10.0, &mut opts.overwrite, "Overwrite existing files");
                d.check(110.0, 156.0, 284.0, 19.0, &mut opts.add_on_close, "Add checked files to NDM main list and to download queue on closing the grabber");
            }
            2 => {
                d.text(106.0, 7.0, 283.0, 25.0, "At this step you should specify what web pages to explore to find the required files. At the next step, you will be able to set file types, location, and other filters.");
                if d.radio(106.0, 35.0, 206.0, 10.0, opts.whole_site, "Explore the whole site") {
                    opts.whole_site = true;
                }
                if d.radio(106.0, 48.0, 241.0, 10.0, !opts.whole_site, "Explore the specified number of link levels:") {
                    opts.whole_site = false;
                }
                let ws = opts.whole_site;
                let r = d.rect(119.0, 60.0, 20.0, 14.0);
                d.ui.put(r, |ui: &mut egui::Ui| ui.add_enabled(!ws, egui::DragValue::new(&mut opts.levels).range(0..=20)));
                d.label(141.0, 63.0, 113.0, 9.0, "levels within the base site");
                d.drag(267.0, 60.0, 20.0, 14.0, &mut opts.levels_other, 0, 5);
                d.label(289.0, 63.0, 89.0, 9.0, "levels on other sites");
                // SS_NOTIFY statics drawn as links (0x50020100): they open the grabber's help
                let link = |d: &mut dlu::D, y: f32, w: f32, t: &str, c: Color32| d.at(119.0, y, w, 9.0, |ui| ui.add(egui::Label::new(RichText::new(tr(t)).color(c)).sense(Sense::click()))).clicked();
                if link(&mut d, 77.0, 261.0, "What is the number of link levels?", Color32::from_rgb(0, 0, 255)) {
                    help("grabber.htm", true);
                }
                let mut ignore = !opts.popups;
                if d.check(109.0, 91.0, 240.0, 10.0, &mut ignore, "Ignore popup windows").changed() {
                    opts.popups = !ignore;
                }
                d.at(109.0, 104.0, 240.0, 10.0, |ui| ui.add_enabled(ws, dlu::checkbox(&mut opts.no_parent, "Don't explore parent directories")));
                d.check(109.0, 117.0, 240.0, 10.0, &mut opts.main_domain, "Explore all sites within the main domain");
                d.check(109.0, 130.0, 240.0, 10.0, &mut opts.js, "Process JavaScript");
                if link(&mut d, 141.0, 274.0, "Use this option carefully. What is the risk?", Color32::GRAY) {
                    help("grabber.htm", true);
                }
                if d.button(305.0, 153.0, 73.0, 14.0, &adv_label, true) {
                    *adv = !*adv;
                }
                if *adv {
                    d.hline(7.0, 176.0, 396.0);
                    d.check(105.0, 182.0, 271.0, 10.0, &mut opts.explore_in_on, "Explore web pages within the following paths/domains only:");
                    d.edit(105.0, 193.0, 291.0, 27.0, &mut opts.explore_in, opts.explore_in_on);
                    d.check(105.0, 224.0, 254.0, 10.0, &mut opts.explore_not_on, "Don't explore web pages within the following paths/domains:");
                    d.edit(105.0, 235.0, 290.0, 27.0, &mut opts.explore_not, opts.explore_not_on);
                    d.label(105.0, 264.0, 298.0, 9.0, "Use semicolons ( ; ) to separate entries.");
                }
            }
            _ => {
                d.label(106.0, 6.0, 216.0, 9.0, "Download the following files (file types)");
                self.grab_filter(&mut d, 16.0, &mut opts.masks, false);
                d.label(106.0, 36.0, 198.0, 9.0, "Don't download the following files (file types)");
                self.grab_filter(&mut d, 46.0, &mut opts.exclude, true);
                d.check(106.0, 69.0, 237.0, 10.0, &mut opts.files_same_site, "Search files on this site only");
                let sub = opts.subfolders && *save_mode == 2;
                d.at(106.0, 87.0, 294.0, 10.0, |ui| ui.add_enabled(!sub, dlu::checkbox(&mut opts.hide_dups, "Hide duplicate files found in different locations")));
                d.check(106.0, 105.0, 262.0, 10.0, &mut opts.start_all, "Start downloading all matched files at once");
                d.text(106.0, 116.0, 292.0, 34.0, "You can explore the site at first, check the files that you need, and download them in the main Grabber window, or add them to the main list of NDM. This option downloads all explored files immediately.");
                if d.button(305.0, 153.0, 73.0, 14.0, &adv_label, true) {
                    *adv = !*adv;
                }
                if *adv {
                    d.hline(7.0, 177.0, 393.0);
                    d.group(10.0, 183.0, 101.0, 76.0, "Download if file size is");
                    let units: Vec<String> = crate::grabber::SIZE_UNITS.iter().map(|u| tr(u)).collect();
                    let unit_refs: Vec<&str> = units.iter().map(String::as_str).collect();
                    for (k, y, t) in [(0usize, 195.0, "Not less than"), (1, 228.0, "Not more than")] {
                        let on = if k == 0 { &mut opts.min_on } else { &mut opts.max_on };
                        d.check(20.0, y, 81.0, 10.0, on, t);
                        let on = *on;
                        let mut n = opts.size[k].to_string();
                        if d.edit(20.0, y + 11.0, 33.0, 13.0, &mut n, on).changed() {
                            opts.size[k] = n.trim().parse().unwrap_or(0);
                        }
                        let mut u = units[opts.units[k] as usize % 3].clone();
                        if d.combo(56.0, y + 11.0, 41.0, 13.0, ["gmin", "gmax"][k], &mut u, &unit_refs, on) {
                            opts.units[k] = units.iter().position(|x| *x == u).unwrap_or(0) as u8;
                        }
                    }
                    d.check(126.0, 182.0, 268.0, 10.0, &mut opts.files_in_on, "Download the files located within the following paths/domains only :");
                    d.edit(126.0, 193.0, 268.0, 27.0, &mut opts.files_in, opts.files_in_on);
                    d.check(126.0, 225.0, 268.0, 10.0, &mut opts.files_not_on, "Don't download the files located within the following paths/domains:");
                    d.edit(126.0, 236.0, 268.0, 27.0, &mut opts.files_not, opts.files_not_on);
                    d.label(126.0, 264.0, 267.0, 9.0, "Use semicolons ( ; ) to separate entries.");
                }
            }
        }
        // the advanced half (274 DLU pages) pushes the buttons down by 97 DLU
        let o = if *adv && *step != 1 { 97.0 } else { 0.0 };
        d.hline(7.0, 183.0 + o, 392.0);
        let ok = url::Url::parse(opts.start.trim()).is_ok_and(|u| u.scheme().starts_with("http") && u.host_str().is_some());
        if d.button(155.0, 190.0 + o, 60.0, 14.0, "< Back", *step > 0) {
            (*step, *adv) = (*step - 1, false);
        }
        if d.button(221.0, 190.0 + o, 60.0, 14.0, "Next >", ok) {
            if *step < 3 {
                (*step, *adv) = (*step + 1, false);
            } else if opts.manual_on {
                // NDM opens the page in the user's browser and the
                // extension hands back the site's cookies once the user has logged in
                match url::Url::parse(opts.manual.trim()).ok().filter(|u| u.scheme().starts_with("http")) {
                    Some(u) => {
                        *self.core.login_wait.lock().unwrap() = u.host_str().map(str::to_ascii_lowercase);
                        self.core.login_cookies.lock().unwrap().clear();
                        open_url(u.as_str());
                        self.open(Dialog::GrabManual { url: u.to_string() });
                    }
                    None => self.open(Dialog::Info { title: "Manual login page".into(), text: "Please enter manual login page or uncheck the corresponding checkbox".into() }),
                }
            } else {
                *crawl = Some(self.crawl(ctx, opts));
            }
        }
        !d.button(324.0, 190.0 + o, 60.0, 14.0, "Close", true)
    }

    pub(super) fn dialog_grab_tpl(&mut self, ui: &mut egui::Ui, name: &mut String, proj: &mut Box<core::settings::GrabProject>, replace: &mut bool) -> bool {
        // template 268; built-in names are reserved, an existing one is replaced on Yes
        let mut d = dlu::D::new(ui);
        if *replace {
            d.text(7.0, 7.0, 172.0, 18.0, "The template with such name already exists. Do you want to replace the template?");
        } else {
            d.text(7.0, 7.0, 172.0, 18.0, "Please enter a name for the grabber template");
        }
        d.edit(6.0, 29.0, 173.0, 14.0, name, !*replace);
        let n = name.trim().to_string();
        let ok = d.button(34.0, 49.0, 50.0, 14.0, if *replace { "Yes" } else { "OK" }, !n.is_empty());
        let cancel = d.button(102.0, 49.0, 50.0, 14.0, if *replace { "No" } else { "Cancel" }, true);
        if ok && crate::grabber::TEMPLATES.iter().any(|t| t.eq_ignore_ascii_case(&n) || tr(t).eq_ignore_ascii_case(&n)) {
            self.open(Dialog::Info { title: "Template Name".into(), text: "This name is reserved. Please select another name for your template.".into() });
            return true;
        }
        let exists = self.core.settings().grab_templates.iter().any(|t| t.opts.name == n);
        if ok && exists && !*replace {
            *replace = true;
            return true;
        }
        if ok {
            let mut t = (**proj).clone();
            (t.opts.name, t.opts.start, t.opts.login, t.opts.pass, t.opts.cookies, t.opts.template, t.files, t.pages) = (n.clone(), String::new(), String::new(), String::new(), String::new(), String::new(), vec![], 0);
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
        let names: Vec<String> = self.core.settings().grab_projects.iter().map(|p| p.opts.name.clone()).collect();
        let lr = d.listview(7.0, 7.0, 186.0, 124.0, &[]);
        let mut open = false;
        d.ui.scope_builder(egui::UiBuilder::new().max_rect(lr.shrink(2.0)), |ui| {
            egui::ScrollArea::vertical().id_salt("gload").auto_shrink(false).show(ui, |ui| {
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
                    self.open(Dialog::Info { title: "Load project".into(), text: "Please select the project name!".into() });
                    return true;
                }
            }
        }
        !(open || cancel)
    }

    pub(super) fn dialog_grab_settings(&mut self, ui: &mut egui::Ui, explore: &mut String, dl: &mut String, link: &mut bool, iec: &mut bool, adv: &mut bool) -> bool {
        // template 262, checked (explore 1-20, download 1-10)
        let mut d = dlu::D::new(ui);
        d.edit(7.0, 6.0, 21.0, 14.0, explore, true);
        d.label(33.0, 10.0, 239.0, 10.0, "files to explore at the same time (1 to 10)");
        d.edit(7.0, 25.0, 22.0, 14.0, dl, true);
        d.label(33.0, 28.0, 239.0, 8.0, "files to download at the same time (1 to 10)");
        d.text(7.0, 43.0, 278.0, 18.0, "Please note that a web server may reject requests if you set a large number of files to explore (download) at the same time.");
        d.hline(7.0, 66.0, 276.0);
        d.check(7.0, 75.0, 278.0, 10.0, iec, "Look up files in IE cache before downloading");
        d.check(7.0, 89.0, 278.0, 10.0, link, "Use link texts as download descriptions when adding files to NDM main list");
        d.check(7.0, 103.0, 278.0, 8.0, adv, "Use Advanced web page processing");
        let ok = d.button(164.0, 118.0, 50.0, 14.0, "OK", true);
        if ok {
            let (e, n) = (explore.trim().parse::<usize>().ok().filter(|v| (1..=20).contains(v)), dl.trim().parse::<usize>().ok().filter(|v| (1..=10).contains(v)));
            let bad = if e.is_none() { Some("Invalid number of files explored at the same time. Please set a correct number.") } else if n.is_none() { Some("Invalid number of files downloaded at the same time. Please set a correct number.") } else { None };
            if let Some(text) = bad {
                self.open(Dialog::Info { title: "Settings of NDM Grabber".into(), text: text.into() });
                return true;
            }
            let mut s = self.core.settings();
            (s.grab_explore, s.grab_dl, s.grab_link_text, s.grab_ie_cache, s.grab_adv) = (e.unwrap(), n.unwrap(), *link, *iec, *adv);
            drop(s);
            self.core.save();
        }
        !(ok || d.button(235.0, 118.0, 50.0, 14.0, "Cancel", true))
    }

    pub(super) fn dialog_grab_manual(&mut self, ui: &mut egui::Ui, url: &str) -> bool {
        let mut d = dlu::D::new(ui);
        d.label(7.0, 7.0, 246.0, 9.0, "Enter login and password manually at the following web page:");
        d.label(7.0, 20.0, 246.0, 9.0, url.to_owned());
        let ok = d.button(140.0, 49.0, 50.0, 14.0, "OK", true);
        let cancel = d.button(203.0, 49.0, 50.0, 14.0, "Cancel", true);
        if ok || cancel {
            *self.core.login_wait.lock().unwrap() = None;
            let ck = std::mem::take(&mut *self.core.login_cookies.lock().unwrap());
            if ok {
                self.grab_cookies = Some(ck);
            }
        }
        !(ok || cancel)
    }

    pub(super) fn dialog_grab_stats(&mut self, ui: &mut egui::Ui, crawl: &mut Arc<crate::grabber::Crawl>) -> bool {
        // template 271
        let st = crawl.state.lock().unwrap();
        let (done, pages, total, explored) = (st.done, st.pages, st.files.len(), st.explored);
        let urls: HashSet<String> = st.files.iter().map(|f| f.url.clone()).collect();
        drop(st);
        let downloaded = self.core.jobs().iter().filter(|j| {
            let i = j.info();
            i.status == Status::Complete && urls.contains(&i.url)
        }).count();
        let mut d = dlu::D::new(ui);
        d.label_r(3.0, 3.0, 36.0, 9.0, "Status:");
        d.label(41.0, 3.0, 63.0, 9.0, if done { tr("Complete") } else { tr("Exploring site") });
        d.hline(5.0, 14.0, 101.0);
        d.label(7.0, 17.0, 97.0, 9.0, "Web pages processed");
        d.label_r(7.0, 28.0, 51.0, 9.0, "Simple");
        d.label(65.0, 29.0, 39.0, 9.0, pages.to_string());
        d.label_r(7.0, 37.0, 51.0, 9.0, "Advanced");
        d.label(65.0, 38.0, 39.0, 9.0, "0");
        d.hline(5.0, 49.0, 101.0);
        d.label(7.0, 52.0, 97.0, 9.0, "Files");
        for (y, k, v) in [(63.0, "Total", total), (72.0, "Explored", explored), (81.0, "Matched", total), (90.0, "Downloaded", downloaded)] {
            d.label_r(7.0, y, 54.0, 9.0, k);
            d.label(65.0, y, 38.0, 9.0, v.to_string());
        }
        true
    }
}
