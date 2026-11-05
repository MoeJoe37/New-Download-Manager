use crate::app::dialogs::Dialog;
use crate::app::{
    core, dlu, egui, help, i18n, menu_layout, open_url, tr, tr_os, App, Arc, Color32, GrabCmd,
    Rect, Relaxed, RichText, Sense, Vec2, GRAB_MENU,
};

impl App {
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
            let (finished, login) = {
                let s = c.state.lock().unwrap();
                (s.done, s.need_login.clone())
            };
            if let Some(realm) = login {
                // "Connecting to the site" (template 279): ask once, then explore again with the login
                c.state.lock().unwrap().need_login = None;
                let site = url::Url::parse(opts.start.trim())
                    .ok()
                    .and_then(|u| u.host_str().map(str::to_string))
                    .unwrap_or_default();
                self.open(Dialog::GrabLogin {
                    realm,
                    site,
                    user: opts.login.clone(),
                    pass: String::new(),
                    save: false,
                });
            }
            if let Some((user, pass)) = self.grab_auth.take() {
                (opts.auth, opts.login, opts.pass) = (true, user, pass);
                *crawl = Some(self.crawl(ctx, opts));
                return true;
            }
            if opts.start_all && finished && !c.auto_started.swap(true, Relaxed) {
                self.grab(&c, opts, *save_mode, save_cat, folder, true);
            }
            let (keep, back, again) =
                self.grabber_results(ctx, ui, &c, opts, *save_mode, save_cat, folder, node);
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
                let proj = core::settings::GrabProject {
                    opts: opts.clone(),
                    save_mode: *save_mode,
                    save_cat: save_cat.clone(),
                    folder: folder.clone(),
                    files: st.files.clone(),
                    pages: st.pages,
                    ..Default::default()
                };
                drop(st);
                let name = opts.name.trim().to_string();
                let (quiet, taken) = {
                    let s = self.core.settings();
                    (
                        s.grab_quiet,
                        s.grab_projects.iter().any(|p| p.opts.name == name),
                    )
                };
                if quiet && !taken {
                    self.save_project(&name, None, Some(proj));
                } else {
                    self.open(Dialog::GrabSave {
                        name,
                        orig: None,
                        quiet,
                        proj: Some(Box::new(proj)),
                        sched: false,
                    });
                }
            }
            return keep;
        }
        // NDM's wizard (template 239 + pages 257 / 259 / 264 / 240) under its menu bar (menu 256)
        let saved = self
            .core
            .settings()
            .grab_projects
            .iter()
            .any(|p| p.opts.name == opts.name.trim());
        let full = ui.max_rect();
        let cmd = {
            let mut bar = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(Rect::from_min_size(
                        full.min,
                        Vec2::new(full.width(), GRAB_MENU),
                    ))
                    .layout(menu_layout()),
            );
            self.grab_menu(&mut bar)
        };
        match cmd {
            Some(GrabCmd::New) => {
                (*opts, *step, *adv) = (self.new_grab_opts(), 0, false);
                return true;
            }
            Some(GrabCmd::Save) => {
                let proj = core::settings::GrabProject {
                    opts: opts.clone(),
                    save_mode: *save_mode,
                    save_cat: save_cat.clone(),
                    folder: folder.clone(),
                    ..Default::default()
                };
                self.open(Dialog::GrabSave {
                    name: opts.name.trim().to_string(),
                    orig: None,
                    quiet: false,
                    proj: Some(Box::new(proj)),
                    sched: false,
                });
            }
            Some(GrabCmd::SaveTpl) => {
                let proj = core::settings::GrabProject {
                    opts: opts.clone(),
                    save_mode: *save_mode,
                    save_cat: save_cat.clone(),
                    folder: folder.clone(),
                    ..Default::default()
                };
                self.open(Dialog::GrabTpl {
                    name: String::new(),
                    proj: Box::new(proj),
                    replace: false,
                });
            }
            Some(GrabCmd::Recent(n)) => {
                self.project_cmd(0, &n);
                return false;
            }
            Some(GrabCmd::Schedule) => {
                let proj = core::settings::GrabProject {
                    opts: opts.clone(),
                    save_mode: *save_mode,
                    save_cat: save_cat.clone(),
                    folder: folder.clone(),
                    ..Default::default()
                };
                self.grab_schedule(proj, saved);
            }
            Some(GrabCmd::Close) => return false,
            None => {}
        }
        let mut body = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_max(
            full.min + Vec2::new(0.0, GRAB_MENU),
            full.max,
        )));
        let mut d = dlu::D::new(&mut body);
        // the picture on the left (static 1801, in a black frame), cropped to fill it without stretching
        let pic = d.rect(7.0, 7.0, 85.0, 160.0);
        match self.res.grab_pic.as_ref() {
            Some(t) => {
                let [w, h] = t.size().map(|v| v as f32);
                let (box_a, img_a) = (pic.width() / pic.height(), w / h);
                let uv = if img_a > box_a {
                    let k = box_a / img_a;
                    Rect::from_min_max(
                        egui::pos2((1.0 - k) / 2.0, 0.0),
                        egui::pos2((1.0 + k) / 2.0, 1.0),
                    )
                } else {
                    let k = img_a / box_a;
                    Rect::from_min_max(
                        egui::pos2(0.0, (1.0 - k) / 2.0),
                        egui::pos2(1.0, (1.0 + k) / 2.0),
                    )
                };
                d.ui.painter().image(t.id(), pic, uv, Color32::WHITE);
            }
            None => drop(
                d.ui.painter()
                    .rect_filled(pic, 0.0, Color32::from_rgb(28, 93, 178)),
            ),
        }
        d.ui.painter().rect_stroke(
            pic,
            0.0,
            egui::Stroke::new(1.0_f32, Color32::BLACK),
            egui::StrokeKind::Inside,
        );
        let adv_label = i18n::line(&if *adv {
            format!("<< {}", tr_os("Advanced"))
        } else {
            format!("{} >>", tr_os("Advanced"))
        });
        // NDM's own English where its language files still carry older words
        let en = |new: &str, old: &str| {
            if i18n::current().is_empty() {
                new.to_string()
            } else {
                tr(old)
            }
        };
        match *step {
            0 => {
                d.label(106.0, 2.0, 164.0, 9.0, "Grabber Project Name");
                d.edit(106.0, 12.0, 166.0, 14.0, &mut opts.name, true);
                d.label(106.0, 29.0, 287.0, 9.0, "Start page/address");
                d.edit(106.0, 39.0, 289.0, 14.0, &mut opts.start, true);
                d.label(106.0, 56.0, 147.0, 9.0, "Project template");
                let mut names: Vec<String> = crate::grabber::TEMPLATES
                    .iter()
                    .map(|t| t.to_string())
                    .collect();
                names.extend(
                    self.core
                        .settings()
                        .grab_templates
                        .iter()
                        .map(|t| t.opts.name.clone()),
                );
                let shown: Vec<String> = names
                    .iter()
                    .enumerate()
                    .map(|(k, n)| {
                        if k < crate::grabber::TEMPLATES.len() {
                            tr(n)
                        } else {
                            n.clone()
                        }
                    })
                    .collect();
                let cur_k = names.iter().position(|n| *n == opts.template).unwrap_or(0);
                let mut cur = shown[cur_k].clone();
                if d.combo(
                    106.0,
                    66.0,
                    289.0,
                    14.0,
                    "gtpl",
                    &mut cur,
                    &shown.iter().map(String::as_str).collect::<Vec<_>>(),
                    true,
                ) {
                    if let Some(k) = shown.iter().position(|s| *s == cur) {
                        self.apply_template(&names[k], opts, save_mode, save_cat);
                    }
                }
                d.text(106.0, 80.0, 290.0, 26.0, "If you select a project template, the wizard will make the required project settings for the selected template on the next steps. Note that you may always change any settings manually.");
                d.hline(106.0, 111.0, 289.0);
                d.check(
                    106.0,
                    115.0,
                    161.0,
                    10.0,
                    &mut opts.auth,
                    "Use authorization",
                );
                d.label_r(107.0, 131.0, 50.0, 9.0, "Login");
                d.edit(160.0, 128.0, 74.0, 14.0, &mut opts.login, opts.auth);
                d.label_r(240.0, 131.0, 61.0, 9.0, "Password");
                d.password(305.0, 128.0, 73.0, 14.0, &mut opts.pass, opts.auth);
                d.text(
                    106.0,
                    150.0,
                    188.0,
                    17.0,
                    "Press Advanced button to enable manual login or to disable a logout page",
                );
                if d.button(305.0, 153.0, 73.0, 14.0, &adv_label, true) {
                    *adv = !*adv;
                }
                if *adv {
                    d.hline(7.0, 176.0, 396.0);
                    d.check(
                        105.0,
                        184.0,
                        289.0,
                        10.0,
                        &mut opts.manual_on,
                        "Enter login and password manually at the following web page:",
                    );
                    d.edit(105.0, 196.0, 290.0, 14.0, &mut opts.manual, opts.manual_on);
                    d.check(
                        106.0,
                        220.0,
                        288.0,
                        10.0,
                        &mut opts.logout_on,
                        "Don't open the logout page:",
                    );
                    d.edit(105.0, 232.0, 290.0, 14.0, &mut opts.logout, opts.logout_on);
                    d.text(106.0, 248.0, 286.0, 24.0, "Many sites have a logout page(s) that the Grabber should not open.  You may use asterisk wildcard here to specify a pattern for different logout pages.");
                }
            }
            1 => {
                d.group(103.0, 1.0, 294.0, 123.0, "Save To");
                if d.radio(
                    110.0,
                    8.0,
                    268.0,
                    22.0,
                    *save_mode == 0,
                    &en(
                        "Every file to a folder according to NDM category of the file",
                        "Every file to the directory according to the category of the file",
                    ),
                ) {
                    *save_mode = 0;
                }
                if d.radio(
                    110.0,
                    33.0,
                    270.0,
                    10.0,
                    *save_mode == 1,
                    &en(
                        "All files to the folder associated with the following NDM category:",
                        "All files to one category",
                    ),
                ) {
                    *save_mode = 1;
                }
                let cat_names = self.core.settings().category_names();
                let cats: Vec<&str> = cat_names.iter().map(String::as_str).collect();
                d.combo(
                    130.0,
                    43.0,
                    130.0,
                    14.0,
                    "gcat",
                    save_cat,
                    &cats,
                    *save_mode == 1,
                );
                let cat_dir = self.core.settings().folder_for(save_cat);
                d.edit(130.0, 58.0, 256.0, 14.0, &mut cat_dir.clone(), false);
                if d.radio(
                    110.0,
                    79.0,
                    186.0,
                    10.0,
                    *save_mode == 2,
                    &en(
                        "All files to the following folder",
                        "All files to one directory",
                    ),
                ) {
                    *save_mode = 2;
                }
                let m2 = *save_mode == 2;
                d.edit(130.0, 89.0, 256.0, 14.0, folder, m2);
                d.at(130.0, 107.0, 198.0, 10.0, |ui| {
                    ui.add_enabled(
                        m2,
                        dlu::checkbox(&mut opts.subfolders, "Use original relative subfolders"),
                    )
                });
                if d.button(331.0, 106.0, 55.0, 14.0, "Browse...", m2) {
                    if let Some(p) = rfd::FileDialog::new()
                        .set_directory(folder.as_str())
                        .pick_folder()
                    {
                        *folder = p.display().to_string();
                    }
                }
                d.check(110.0, 128.0, 280.0, 10.0, &mut opts.convert, "Convert the links in downloaded html files to the local files for offline browsing");
                d.check(
                    110.0,
                    144.0,
                    282.0,
                    10.0,
                    &mut opts.overwrite,
                    "Overwrite existing files",
                );
                d.check(110.0, 156.0, 284.0, 19.0, &mut opts.add_on_close, "Add checked files to NDM main list and to download queue on closing the grabber");
            }
            2 => {
                d.text(106.0, 7.0, 283.0, 25.0, "At this step you should specify what web pages to explore to find the required files. At the next step, you will be able to set file types, location, and other filters.");
                if d.radio(
                    106.0,
                    35.0,
                    206.0,
                    10.0,
                    opts.whole_site,
                    "Explore the whole site",
                ) {
                    opts.whole_site = true;
                }
                if d.radio(
                    106.0,
                    48.0,
                    241.0,
                    10.0,
                    !opts.whole_site,
                    "Explore the specified number of link levels:",
                ) {
                    opts.whole_site = false;
                }
                let ws = opts.whole_site;
                let r = d.rect(119.0, 60.0, 20.0, 14.0);
                d.ui.put(r, |ui: &mut egui::Ui| {
                    ui.add_enabled(!ws, egui::DragValue::new(&mut opts.levels).range(0..=20))
                });
                d.label(141.0, 63.0, 113.0, 9.0, "levels within the base site");
                d.drag(267.0, 60.0, 20.0, 14.0, &mut opts.levels_other, 0, 5);
                d.label(289.0, 63.0, 89.0, 9.0, "levels on other sites");
                // SS_NOTIFY statics drawn as links (0x50020100): they open the grabber's help
                let link = |d: &mut dlu::D, y: f32, w: f32, t: &str, c: Color32| {
                    d.at(119.0, y, w, 9.0, |ui| {
                        ui.add(
                            egui::Label::new(RichText::new(tr(t)).color(c)).sense(Sense::click()),
                        )
                    })
                    .clicked()
                };
                if link(
                    &mut d,
                    77.0,
                    261.0,
                    "What is the number of link levels?",
                    Color32::from_rgb(0, 0, 255),
                ) {
                    help("grabber.htm", true);
                }
                let mut ignore = !opts.popups;
                if d.check(
                    109.0,
                    91.0,
                    240.0,
                    10.0,
                    &mut ignore,
                    "Ignore popup windows",
                )
                .changed()
                {
                    opts.popups = !ignore;
                }
                d.at(109.0, 104.0, 240.0, 10.0, |ui| {
                    ui.add_enabled(
                        ws,
                        dlu::checkbox(&mut opts.no_parent, "Don't explore parent directories"),
                    )
                });
                d.check(
                    109.0,
                    117.0,
                    240.0,
                    10.0,
                    &mut opts.main_domain,
                    "Explore all sites within the main domain",
                );
                d.check(
                    109.0,
                    130.0,
                    240.0,
                    10.0,
                    &mut opts.js,
                    "Process JavaScript",
                );
                if link(
                    &mut d,
                    141.0,
                    274.0,
                    "Use this option carefully. What is the risk?",
                    Color32::GRAY,
                ) {
                    help("grabber.htm", true);
                }
                if d.button(305.0, 153.0, 73.0, 14.0, &adv_label, true) {
                    *adv = !*adv;
                }
                if *adv {
                    d.hline(7.0, 176.0, 396.0);
                    d.check(
                        105.0,
                        182.0,
                        271.0,
                        10.0,
                        &mut opts.explore_in_on,
                        "Explore web pages within the following paths/domains only:",
                    );
                    d.edit(
                        105.0,
                        193.0,
                        291.0,
                        27.0,
                        &mut opts.explore_in,
                        opts.explore_in_on,
                    );
                    d.check(
                        105.0,
                        224.0,
                        254.0,
                        10.0,
                        &mut opts.explore_not_on,
                        "Don't explore web pages within the following paths/domains:",
                    );
                    d.edit(
                        105.0,
                        235.0,
                        290.0,
                        27.0,
                        &mut opts.explore_not,
                        opts.explore_not_on,
                    );
                    d.label(
                        105.0,
                        264.0,
                        298.0,
                        9.0,
                        "Use semicolons ( ; ) to separate entries.",
                    );
                }
            }
            _ => {
                d.label(
                    106.0,
                    6.0,
                    216.0,
                    9.0,
                    "Download the following files (file types)",
                );
                self.grab_filter(&mut d, 16.0, &mut opts.masks, false);
                d.label(
                    106.0,
                    36.0,
                    198.0,
                    9.0,
                    "Don't download the following files (file types)",
                );
                self.grab_filter(&mut d, 46.0, &mut opts.exclude, true);
                d.check(
                    106.0,
                    69.0,
                    237.0,
                    10.0,
                    &mut opts.files_same_site,
                    "Search files on this site only",
                );
                let sub = opts.subfolders && *save_mode == 2;
                d.at(106.0, 87.0, 294.0, 10.0, |ui| {
                    ui.add_enabled(
                        !sub,
                        dlu::checkbox(
                            &mut opts.hide_dups,
                            "Hide duplicate files found in different locations",
                        ),
                    )
                });
                d.check(
                    106.0,
                    105.0,
                    262.0,
                    10.0,
                    &mut opts.start_all,
                    "Start downloading all matched files at once",
                );
                d.text(106.0, 116.0, 292.0, 34.0, "You can explore the site at first, check the files that you need, and download them in the main Grabber window, or add them to the main list of NDM. This option downloads all explored files immediately.");
                if d.button(305.0, 153.0, 73.0, 14.0, &adv_label, true) {
                    *adv = !*adv;
                }
                if *adv {
                    d.hline(7.0, 177.0, 393.0);
                    d.group(10.0, 183.0, 101.0, 76.0, "Download if file size is");
                    let units: Vec<String> =
                        crate::grabber::SIZE_UNITS.iter().map(|u| tr(u)).collect();
                    let unit_refs: Vec<&str> = units.iter().map(String::as_str).collect();
                    for (k, y, t) in [
                        (0usize, 195.0, "Not less than"),
                        (1, 228.0, "Not more than"),
                    ] {
                        let on = if k == 0 {
                            &mut opts.min_on
                        } else {
                            &mut opts.max_on
                        };
                        d.check(20.0, y, 81.0, 10.0, on, t);
                        let on = *on;
                        let mut n = opts.size[k].to_string();
                        if d.edit(20.0, y + 11.0, 33.0, 13.0, &mut n, on).changed() {
                            opts.size[k] = n.trim().parse().unwrap_or(0);
                        }
                        let mut u = units[opts.units[k] as usize % 3].clone();
                        if d.combo(
                            56.0,
                            y + 11.0,
                            41.0,
                            13.0,
                            ["gmin", "gmax"][k],
                            &mut u,
                            &unit_refs,
                            on,
                        ) {
                            opts.units[k] = units.iter().position(|x| *x == u).unwrap_or(0) as u8;
                        }
                    }
                    d.check(
                        126.0,
                        182.0,
                        268.0,
                        10.0,
                        &mut opts.files_in_on,
                        "Download the files located within the following paths/domains only :",
                    );
                    d.edit(
                        126.0,
                        193.0,
                        268.0,
                        27.0,
                        &mut opts.files_in,
                        opts.files_in_on,
                    );
                    d.check(
                        126.0,
                        225.0,
                        268.0,
                        10.0,
                        &mut opts.files_not_on,
                        "Don't download the files located within the following paths/domains:",
                    );
                    d.edit(
                        126.0,
                        236.0,
                        268.0,
                        27.0,
                        &mut opts.files_not,
                        opts.files_not_on,
                    );
                    d.label(
                        126.0,
                        264.0,
                        267.0,
                        9.0,
                        "Use semicolons ( ; ) to separate entries.",
                    );
                }
            }
        }
        // the advanced half (274 DLU pages) pushes the buttons down by 97 DLU
        let o = if *adv && *step != 1 { 97.0 } else { 0.0 };
        d.hline(7.0, 183.0 + o, 392.0);
        let ok = url::Url::parse(opts.start.trim())
            .is_ok_and(|u| u.scheme().starts_with("http") && u.host_str().is_some());
        if d.button(155.0, 190.0 + o, 60.0, 14.0, "< Back", *step > 0) {
            (*step, *adv) = (*step - 1, false);
        }
        if d.button(221.0, 190.0 + o, 60.0, 14.0, "Next >", ok) {
            if *step < 3 {
                (*step, *adv) = (*step + 1, false);
            } else if opts.manual_on {
                // NDM opens the page in the user's browser and the
                // extension hands back the site's cookies once the user has logged in
                match url::Url::parse(opts.manual.trim())
                    .ok()
                    .filter(|u| u.scheme().starts_with("http"))
                {
                    Some(u) => {
                        *self.core.login_wait.lock().unwrap() =
                            u.host_str().map(str::to_ascii_lowercase);
                        self.core.login_cookies.lock().unwrap().clear();
                        open_url(u.as_str());
                        self.open(Dialog::GrabManual { url: u.to_string() });
                    }
                    None => self.open(Dialog::Info {
                        title: "Manual login page".into(),
                        text:
                            "Please enter manual login page or uncheck the corresponding checkbox"
                                .into(),
                    }),
                }
            } else {
                *crawl = Some(self.crawl(ctx, opts));
            }
        }
        !d.button(324.0, 190.0 + o, 60.0, 14.0, "Close", true)
    }
}
