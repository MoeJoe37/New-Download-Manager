use crate::app::{Align2, App, Arc, Cat, Color32, Dialog, FontId, GENERAL, HashMap, HashSet, Path, PathBuf, Rect, Relaxed, Sense, Settings, Status, Vec2, clipboard, core, dlu, egui, engine, grabber, help, human, i18n, links, menu_layout, tr, tr_os, win};
/// The grabber's menu bar and toolbar heights (points).
pub(in crate::app) const GRAB_MENU: f32 = 20.0;

pub(in crate::app) const GRAB_TB: f32 = 30.0;

/// Wizard step captions, in NDM's order.
pub(in crate::app) const GRAB_STEPS: [&str; 4] = ["Set start page", "Save files to", "Set explorer filters", "Set file filters"];

pub(in crate::app) const GRAB_FILTERS: [(&str, &str); 7] = [
    ("All Files", "*.*"),
    ("Images Files", "*.jpg;*.jpeg;*.png;*.gif;*.webp;*.bmp"),
    ("Audio Files", "*.mp3;*.flac;*.wav;*.ogg;*.m4a"),
    ("Video Files", "*.mp4;*.mkv;*.avi;*.webm;*.mov"),
    ("Compressed", "*.zip;*.rar;*.7z;*.gz;*.tar"),
    ("Documents", "*.pdf;*.doc;*.docx;*.xls;*.xlsx;*.ppt;*.pptx"),
    ("Web content", "*.htm;*.html;*.css;*.js;*.jpg;*.jpeg;*.png;*.gif;*.webp;*.svg;*.ico;*.woff;*.woff2"),
];

/// A grabber menu command (menu 256).
pub(in crate::app) enum GrabCmd {
    New,
    Save,
    SaveTpl,
    Recent(String),
    Schedule,
    Close,
}

impl App {
    pub(in crate::app) fn grabber(&mut self) {
        let opts = self.new_grab_opts();
        let folder = self.core.settings().base_dir.clone();
        self.open(Dialog::Grabber { step: 0, adv: false, node: 1, opts, save_mode: 0, save_cat: GENERAL.into(), folder, crawl: None });
    }

    /// Add the grabber's checked files to the list (queued); `start` also starts the queue.
    pub(in crate::app) fn grab(&mut self, crawl: &grabber::Crawl, opts: &grabber::Opts, save_mode: u8, save_cat: &str, folder: &str, start: bool) {
        self.core.grab_files(&mut crawl.state.lock().unwrap().files, opts, save_mode, save_cat, folder, start);
    }

    /// Explore with the wizard's settings in the background.
    pub(in crate::app) fn crawl(&self, ctx: &egui::Context, opts: &grabber::Opts) -> Arc<grabber::Crawl> {
        let c = Arc::new(grabber::Crawl::default());
        let (o, a, c2, ctx2, t) = (opts.clone(), self.core.agent(&opts.start), c.clone(), ctx.clone(), self.core.grab_tuning());
        std::thread::spawn(move || {
            grabber::run(o, a, c2, t);
            ctx2.request_repaint();
        });
        c
    }

    /// Tree menu of a saved grabber project: 0 open, 1 schedule, 2 rename, 3 delete.
    pub(in crate::app) fn project_cmd(&mut self, k: usize, name: &str) {
        let Some(p) = self.core.settings().grab_projects.iter().find(|p| p.opts.name == name).cloned() else { return };
        match k {
            0 => {
                // the results window with what the project found last time
                let c = Arc::new(grabber::Crawl::default());
                {
                    let mut st = c.state.lock().unwrap();
                    (st.files, st.pages, st.done) = (p.files.clone(), p.pages, true);
                    st.status = format!("Done. {} web pages processed, {} files found", p.pages, p.files.len()); // as grabber::run says it
                }
                c.auto_started.store(true, Relaxed);
                self.open(Dialog::Grabber { step: 0, adv: false, node: 1, opts: p.opts, save_mode: p.save_mode, save_cat: p.save_cat, folder: p.folder, crawl: Some(c) });
            }
            1 => self.open(Dialog::GrabSched { p: Box::new(p) }),
            2 => self.open(Dialog::GrabSave { name: name.to_string(), orig: Some(name.to_string()), quiet: false, proj: None, sched: false }),
            _ => {
                let mut s = self.core.settings();
                s.grab_projects.retain(|p| p.opts.name != name);
                s.grab_recent.retain(|n| n != name);
                drop(s);
                self.cat = Cat::Grabber(None);
                self.core.save();
            }
        }
        if k == 0 {
            Self::touch_recent(&mut self.core.settings(), name);
        }
    }

    /// Options -> Schedule the project / toolbar: the scheduler (284) for a saved project, else NDM's
    /// "save it first?" question.
    pub(in crate::app) fn grab_schedule(&mut self, proj: core::settings::GrabProject, saved: bool) {
        if saved {
            let name = proj.opts.name.trim().to_string();
            self.project_cmd(1, &name);
        } else {
            self.open(Dialog::GrabSchedAsk { proj: Box::new(proj) });
        }
    }

    /// `name` to the top of Recent Projects (4 kept).
    fn touch_recent(s: &mut Settings, name: &str) {
        s.grab_recent.retain(|n| n != name);
        s.grab_recent.insert(0, name.to_string());
        s.grab_recent.truncate(4);
    }

    /// Store a grabber project under `name` (replacing one of that name, or `orig` when renaming).
    pub(in crate::app) fn save_project(&mut self, name: &str, orig: Option<&str>, proj: Option<core::settings::GrabProject>) {
        let mut s = self.core.settings();
        match proj {
            Some(mut p) => {
                p.opts.name = name.to_string();
                match s.grab_projects.iter_mut().find(|x| x.opts.name == name) {
                    Some(x) => {
                        // keep its schedule
                        (p.sched, p.periodic, p.action) = (x.sched.clone(), x.periodic, x.action);
                        *x = p;
                    }
                    None => s.grab_projects.push(p),
                }
                Self::touch_recent(&mut s, name);
            }
            None => {
                if let Some(x) = s.grab_projects.iter_mut().find(|x| Some(x.opts.name.as_str()) == orig) {
                    x.opts.name = name.to_string();
                }
                for n in s.grab_recent.iter_mut().filter(|n| Some(n.as_str()) == orig) {
                    *n = name.to_string();
                }
                if matches!(&self.cat, Cat::Grabber(Some(c)) if Some(c.as_str()) == orig) {
                    self.cat = Cat::Grabber(Some(name.to_string()));
                }
            }
        }
        drop(s);
        self.core.save();
    }

    /// The grabber window after the wizard: site tree on the left, found files on the right.
    /// Returns (keep open, back to wizard).
    /// New wizard settings: the start page from the clipboard, the file types of Options -> File types.
    pub(in crate::app) fn new_grab_opts(&self) -> grabber::Opts {
        let masks = self.core.settings().file_types.split_whitespace().map(|e| format!("*.{}", e.trim_start_matches('.'))).collect::<Vec<_>>().join(",");
        let start = links(&clipboard()).into_iter().next().unwrap_or_else(|| "https://".into());
        let n = self.core.settings().grab_projects.len() + 1;
        grabber::Opts { name: format!("project_{n}"), start, levels: 2, no_parent: true, masks, main_domain: true, hide_dups: true, size: [1, 10], units: [0, 1], ..Default::default() }
    }

    /// The grabber's menu bar (in a `menu_layout` Ui).
    pub(in crate::app) fn grab_menu(&mut self, ui: &mut egui::Ui) -> Option<GrabCmd> {
        let (mut cmd, mut load, mut settings) = (None, false, false);
        let recent: Vec<String> = {
            let s = self.core.settings();
            s.grab_recent.iter().filter(|n| s.grab_projects.iter().any(|p| p.opts.name == **n)).cloned().collect()
        };
        egui::menu::bar(ui, |ui| {
            ui.menu_button(tr("Project"), |ui| {
                if ui.button(tr("New")).clicked() {
                    cmd = Some(GrabCmd::New);
                    ui.close_menu();
                }
                if ui.button(tr("Load...")).clicked() {
                    load = true;
                    ui.close_menu();
                }
                if ui.button(tr("Save")).clicked() {
                    cmd = Some(GrabCmd::Save);
                    ui.close_menu();
                }
                if ui.button(tr("Save current settings as a template")).clicked() {
                    cmd = Some(GrabCmd::SaveTpl);
                    ui.close_menu();
                }
                ui.separator();
                // "1 name"... "4 name", or a greyed "Recent..." when there are none
                ui.menu_button(tr("Recent Projects"), |ui| {
                    if recent.is_empty() {
                        ui.add_enabled(false, egui::Button::new(tr("Recent...")));
                    }
                    for (k, n) in recent.iter().enumerate() {
                        if ui.button(format!("{} {n}", k + 1)).clicked() {
                            cmd = Some(GrabCmd::Recent(n.clone()));
                            ui.close_menu();
                        }
                    }
                });
                ui.separator();
                if ui.button(tr("Close")).clicked() {
                    cmd = Some(GrabCmd::Close);
                    ui.close_menu();
                }
            });
            ui.menu_button(tr("Options"), |ui| {
                if ui.button(tr("Grabber Settings")).clicked() {
                    settings = true;
                    ui.close_menu();
                }
                if ui.button(tr("Schedule the project")).clicked() {
                    cmd = Some(GrabCmd::Schedule);
                    ui.close_menu();
                }
            });
            if ui.button(tr("Help")).clicked() {
                help("grabber.htm", true);
            }
        });
        if load {
            if self.core.settings().grab_projects.is_empty() {
                self.open(Dialog::Info { title: "Load project".into(), text: "There are no saved projects of NDM grabber".into() });
            } else {
                self.open(Dialog::GrabLoad { sel: None });
            }
        }
        if settings {
            let s = self.core.settings();
            let d = Dialog::GrabSettings { explore: s.grab_explore.to_string(), dl: s.grab_dl.to_string(), link: s.grab_link_text, iec: s.grab_ie_cache, adv: s.grab_adv };
            drop(s);
            self.open(d);
        }
        cmd
    }

    /// Project template `name` (combo 1511): one of NDM's built-ins, or one the
    /// user saved. The project's name, start page and login stay.
    pub(in crate::app) fn apply_template(&self, name: &str, opts: &mut grabber::Opts, save_mode: &mut u8, save_cat: &mut String) {
        let mask = |f: &str| GRAB_FILTERS.iter().find(|x| x.0 == f).map_or("*.*", |x| x.1).to_string();
        let cat = |c: &str, cur: &str| if self.core.settings().category_names().iter().any(|n| n == c) { c.to_string() } else { cur.to_string() };
        opts.template = name.to_string();
        let Some(k) = grabber::TEMPLATES.iter().position(|t| *t == name) else {
            let Some(t) = self.core.settings().grab_templates.iter().find(|t| t.opts.name == name).cloned() else { return };
            let keep = opts.clone();
            *opts = grabber::Opts { name: keep.name, start: keep.start, auth: keep.auth, login: keep.login, pass: keep.pass, manual_on: keep.manual_on, manual: keep.manual, cookies: keep.cookies, template: keep.template, ..t.opts };
            (*save_mode, *save_cat) = (t.save_mode, t.save_cat);
            return;
        };
        if let Some((mode, c)) = grabber::builtin_template(k, opts, &mask) {
            *save_mode = mode;
            if let Some(c) = c {
                *save_cat = cat(c, save_cat);
            }
        }
    }

    /// A file-type filter: NDM's drop-down list ("All Files ( *.* )",...) and its "Add Filter" button,
    /// which here takes masks typed into a small popup. `exclude`: the first entry is "Don't use exclude filter".
    pub(in crate::app) fn grab_filter(&mut self, d: &mut dlu::D, y: f32, masks: &mut String, exclude: bool) {
        let label = |m: &str| match GRAB_FILTERS.iter().find(|f| f.1 == m) {
            Some(f) => i18n::line(&format!("{}  ( {} )", tr_os(f.0), f.1)),
            None if m.is_empty() => tr("Don't use exclude filter"),
            None => format!("( {m} )"),
        };
        let mut choices: Vec<String> = GRAB_FILTERS.iter().map(|f| label(f.1)).collect();
        if exclude {
            choices.insert(0, label(""));
        }
        let mut cur = label(masks);
        if !choices.contains(&cur) {
            choices.push(cur.clone());
        }
        let refs: Vec<&str> = choices.iter().map(String::as_str).collect();
        if d.combo(106.0, y, 194.0, 14.0, if exclude { "gexc" } else { "ginc" }, &mut cur, &refs, true) {
            *masks = if exclude && cur == label("") { String::new() } else { GRAB_FILTERS.iter().find(|f| label(f.1) == cur).map_or_else(|| masks.clone(), |f| f.1.to_string()) };
        }
        let r = d.rect(305.0, y - 1.0, 73.0, 14.0);
        let id = d.ui.id().with(("gaddf", exclude));
        let resp = d.ui.put(r, egui::Button::new(tr("Add Filter")).min_size(r.size()));
        if resp.clicked() {
            self.grab_filter_buf = masks.clone();
            d.ui.memory_mut(|m| m.toggle_popup(id));
        }
        let buf = &mut self.grab_filter_buf;
        egui::popup_below_widget(d.ui, id, &resp, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
            ui.set_min_width(260.0);
            ui.add(egui::TextEdit::singleline(buf).hint_text("*.zip;*.pdf"));
            if ui.button(tr("OK")).clicked() {
                *masks = buf.split([',', ';', ' ']).map(str::trim).filter(|m| !m.is_empty()).collect::<Vec<_>>().join(";");
                ui.memory_mut(|m| m.close_popup());
            }
        });
    }

    /// Grabber "Thumbnails View": the found pictures in a grid, drawn from the file once it is downloaded.
    fn grab_thumbs(&mut self, ui: &mut egui::Ui, rect: Rect, items: &[(String, Option<PathBuf>)]) {
        const CELL: f32 = 104.0;
        let mut decoded = 0;
        ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
            egui::ScrollArea::vertical().id_salt("gthumbs").auto_shrink(false).show(ui, |ui| {
                let cols = ((rect.width() / CELL) as usize).max(1);
                for row in items.chunks(cols) {
                    let (r, _) = ui.allocate_exact_size(Vec2::new(rect.width() - 14.0, CELL + 16.0), Sense::hover());
                    for (k, (name, path)) in row.iter().enumerate() {
                        let cell = Rect::from_min_size(r.min + Vec2::new(k as f32 * CELL + 4.0, 4.0), Vec2::splat(CELL - 8.0));
                        ui.painter().rect_stroke(cell, 0.0, egui::Stroke::new(1.0_f32, Color32::from_gray(190)), egui::StrokeKind::Inside);
                        // ponytail: decoded on the UI thread, at most 3 a frame; cached for the session
                        let tex = path.as_ref().and_then(|p| {
                            if !self.thumbs.contains_key(p) && decoded < 3 {
                                decoded += 1;
                                let t = image::open(p).ok().map(|img| {
                                    let px = img.thumbnail(192, 192).to_rgba8();
                                    ui.ctx().load_texture(p.display().to_string(), egui::ColorImage::from_rgba_unmultiplied([px.width() as usize, px.height() as usize], px.as_raw()), egui::TextureOptions::LINEAR)
                                });
                                self.thumbs.insert(p.clone(), t);
                            }
                            self.thumbs.get(p).cloned().flatten()
                        });
                        match tex {
                            Some(t) => {
                                let [w, h] = t.size().map(|v| v as f32);
                                let s = ((cell.width() - 6.0) / w).min((cell.height() - 6.0) / h).min(1.0);
                                ui.painter().image(t.id(), Rect::from_center_size(cell.center(), Vec2::new(w, h) * s), Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
                            }
                            None => drop(ui.painter().text(cell.center(), Align2::CENTER_CENTER, "🖼", FontId::proportional(28.0), Color32::GRAY)),
                        }
                        let clip = Rect::from_min_size(egui::pos2(cell.left(), cell.bottom()), Vec2::new(cell.width(), 16.0));
                        ui.painter().with_clip_rect(clip).text(clip.center(), Align2::CENTER_CENTER, name, FontId::proportional(11.0), ui.visuals().text_color());
                    }
                }
            });
        });
        if decoded > 0 {
            ui.ctx().request_repaint();
        }
    }

    /// The Grabber's main window (template 241 under menu 256 and toolbar 260): the explored files with NDM's
    /// columns, a tree to filter them (All Files, Link View by page, Folder View by site), the toolbar's
    /// commands. Returns (keep the window, back to the wizard, explore again).
    #[allow(clippy::too_many_arguments)]
    pub(in crate::app) fn grabber_results(&mut self, ctx: &egui::Context, ui: &mut egui::Ui, c: &Arc<grabber::Crawl>, opts: &grabber::Opts, save_mode: u8, save_cat: &str, folder: &str, node: &mut usize) -> (bool, bool, bool) {
        let (done, total, checked, pages) = {
            let s = c.state.lock().unwrap();
            let mut pages: Vec<String> = s.page_list.clone();
            for f in &s.files {
                if !f.page.is_empty() && !pages.contains(&f.page) {
                    pages.push(f.page.clone());
                }
            }
            (s.done, s.files.len(), s.files.iter().filter(|f| f.checked).count(), pages)
        };
        let urls: HashSet<String> = c.state.lock().unwrap().files.iter().map(|f| f.url.clone()).collect();
        let jobs: HashMap<String, Arc<engine::Job>> = self.core.jobs().into_iter().filter(|j| urls.contains(&j.info().url)).map(|j| {
            let u = j.info().url.clone();
            (u, j)
        }).collect();
        let saved = self.core.settings().grab_projects.iter().any(|p| p.opts.name == opts.name.trim());
        let full = ui.max_rect();
        let cmd = {
            let mut bar = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(full.min, Vec2::new(full.width(), GRAB_MENU))).layout(menu_layout()));
            self.grab_menu(&mut bar)
        };
        let (mut keep, mut back, mut again) = (true, false, false);
        let mut action = None;
        match cmd {
            Some(GrabCmd::New) => back = true,
            Some(GrabCmd::Save) => {
                let st = c.state.lock().unwrap();
                let proj = core::settings::GrabProject { opts: opts.clone(), save_mode, save_cat: save_cat.into(), folder: folder.into(), files: st.files.clone(), pages: st.pages, ..Default::default() };
                drop(st);
                self.open(Dialog::GrabSave { name: opts.name.trim().to_string(), orig: None, quiet: false, proj: Some(Box::new(proj)), sched: false });
            }
            Some(GrabCmd::SaveTpl) => {
                let proj = core::settings::GrabProject { opts: opts.clone(), save_mode, save_cat: save_cat.into(), folder: folder.into(), ..Default::default() };
                self.open(Dialog::GrabTpl { name: String::new(), proj: Box::new(proj), replace: false });
            }
            Some(GrabCmd::Recent(n)) => {
                self.project_cmd(0, &n);
                keep = false;
            }
            Some(GrabCmd::Schedule) => action = Some(8),
            Some(GrabCmd::Close) => keep = false,
            None => {}
        }
        // toolbar 260: NDM's 24 px strip (bitmaps 265 / 266), one image per command, a separator after the second
        let ppp = ctx.pixels_per_point();
        let tb = Rect::from_min_size(full.min + Vec2::new(0.0, GRAB_MENU), Vec2::new(full.width(), GRAB_TB));
        ui.painter().hline(tb.x_range(), tb.top(), egui::Stroke::new(1.0_f32, Color32::from_gray(200)));
        let running = jobs.values().any(|j| j.running());
        let buttons: [(usize, &str, bool); 10] = [
            (0, "Start Exploring", done),
            (1, "Stop Exploring", !done),
            (2, "Check All Files", total > 0),
            (3, "Uncheck All Files", total > 0),
            (4, "Start Download checked files", checked > 0),
            (5, "Stop download checked files", running),
            (6, "Add checked files to the main NDM list and to the download queue", checked > 0),
            (7, "Update all", done),
            (8, "Schedule the project", true),
            (9, "Show grabber statistics", true),
        ];
        let size = Vec2::splat(24.0 / ppp);
        let mut x = tb.left() + 4.0;
        for (k, (img, tip, enabled)) in buttons.into_iter().enumerate() {
            if k == 2 {
                ui.painter().vline(x + 3.0, tb.y_range().shrink(5.0), egui::Stroke::new(1.0_f32, Color32::from_gray(170)));
                x += 8.0;
            }
            let r = Rect::from_min_size(egui::pos2(x, tb.center().y - size.y / 2.0 - 3.0), size + Vec2::splat(6.0));
            let resp = ui.interact(r, ui.id().with(("gtb", k)), if enabled { Sense::click() } else { Sense::hover() }).on_hover_text(tr(tip));
            if enabled && resp.hovered() {
                ui.painter().rect_stroke(r, 0.0, egui::Stroke::new(1.0_f32, Color32::from_gray(160)), egui::StrokeKind::Inside);
            }
            let uv = Rect::from_min_max(egui::pos2(img as f32 / 10.0, 0.0), egui::pos2((img + 1) as f32 / 10.0, 1.0));
            if let Some([n, h, g]) = self.res.grab_tb.as_ref() {
                let t = if !enabled { g } else if resp.hovered() { h } else { n };
                ui.painter().image(t.id(), Rect::from_center_size(r.center(), size), uv, Color32::WHITE);
            } else {
                ui.painter().text(r.center(), Align2::CENTER_CENTER, ["⌕", "■", "☑", "☐", "▶", "⏹", "⏬", "⟳", "⏰", "ℹ"][img], FontId::proportional(14.0), if enabled { ui.visuals().text_color() } else { Color32::GRAY });
            }
            if resp.clicked() {
                action = Some(img);
            }
            x += r.width() + 1.0;
        }
        let mut body = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_max(egui::pos2(full.left(), tb.bottom()), full.max)));
        let mut d = dlu::D::new(&mut body);

        // tree (1431): Thumbnails View, All Files, Link View > pages, Folder View > main domain / other sites
        let host = url::Url::parse(opts.start.trim()).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default();
        let domain = host.rsplitn(3, '.').take(2).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join(".");
        let (pic_eye, fold, fold_open) = (self.res.tree(282), self.res.tree(215), self.res.tree(214));
        let page_icon = self.icon(ctx, "html");
        let short = |u: &str| u.split_once("://").map_or(u, |x| x.1).to_string();
        let mut nodes: Vec<(u8, usize, Option<egui::TextureId>, String)> = vec![(0, 0, pic_eye, tr("Thumbnails View")), (0, 1, if *node == 1 { fold_open } else { fold }, tr("All Files")), (0, 2, page_icon, tr("Link View"))];
        nodes.extend(pages.iter().enumerate().map(|(k, p)| (1, 10 + k, page_icon, short(p))));
        nodes.push((0, 3, fold_open, tr("Folder View")));
        nodes.push((1, 4, fold, i18n::line(&format!("{} {}", i18n::ltr(&format!("*.{domain}")), tr_os("sites")))));
        nodes.push((1, 5, fold, tr("Other sites")));
        let tree = d.listview(7.0, 7.0, 101.0, 244.0, &[]);
        d.ui.scope_builder(egui::UiBuilder::new().max_rect(tree.shrink(2.0)), |ui| {
            egui::ScrollArea::both().id_salt("gtree").auto_shrink(false).show(ui, |ui| {
                for (depth, id, icon, label) in &nodes {
                    let (row, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width().max(150.0), 16.0), Sense::click());
                    let mut x = row.left() + 3.0 + *depth as f32 * 16.0;
                    if let Some(t) = icon {
                        ui.painter().image(*t, Rect::from_min_size(egui::pos2(x, row.center().y - 6.5), Vec2::new(16.0, 13.0)), Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
                    }
                    x += 19.0;
                    let font = egui::TextStyle::Body.resolve(ui.style());
                    let g = ui.painter().layout_no_wrap(label.clone(), font, if *node == *id { Color32::WHITE } else { ui.visuals().text_color() });
                    let lr = Rect::from_min_size(egui::pos2(x - 2.0, row.top()), Vec2::new(g.size().x + 4.0, row.height()));
                    if *node == *id {
                        ui.painter().rect_filled(lr, 0.0, Color32::from_rgb(0, 120, 215));
                    }
                    ui.painter().galley(egui::pos2(x, row.center().y - g.size().y / 2.0), g, ui.visuals().text_color());
                    if resp.clicked() {
                        *node = *id;
                    }
                }
            });
        });

        // list (1023): NDM's seven columns, horizontally scrolled
        const COLS: [(&str, f32); 7] = [("File Name", 111.0), ("File Type", 154.0), ("Size", 57.0), ("Status", 57.0), ("Link Text", 57.0), ("Download from", 86.0), ("Save to", 86.0)];
        let dx = d.dx();
        let lr = d.listview(115.0, 7.0, 386.0, 244.0, &[]);
        let width: f32 = COLS.iter().map(|c| c.1 * dx).sum();
        let shown = |f: &grabber::Found| -> bool {
            let fh = url::Url::parse(&f.url).ok().and_then(|u| u.host_str().map(str::to_ascii_lowercase)).unwrap_or_default();
            let ours = !domain.is_empty() && (fh == domain || fh.ends_with(&format!(".{domain}")));
            match *node {
                0 => ["jpg", "jpeg", "png", "gif", "webp", "bmp", "svg"].contains(&core::ext(&f.name).as_str()),
                4 => ours,
                5 => !ours,
                k if k >= 10 => pages.get(k - 10).is_some_and(|p| *p == f.page),
                _ => true,
            }
        };
        let rows: Vec<(usize, String, String)> = c.state.lock().unwrap().files.iter().enumerate().filter(|(_, f)| shown(f)).map(|(k, f)| (k, f.name.clone(), core::ext(&f.name))).collect();
        let icons: Vec<Option<egui::TextureId>> = rows.iter().map(|r| self.icon(ctx, &r.2)).collect();
        let types: Vec<String> = rows.iter().map(|r| self.type_names.entry(r.2.clone()).or_insert_with(|| i18n::line(&win::type_name(&r.2))).clone()).collect();
        let font = egui::TextStyle::Body.resolve(d.ui.style());
        let ink = d.ui.visuals().text_color();
        let line = dlu::pal(d.ui).line;
        let save_dir = |name: &str, url: &str| self.core.grab_dir(name, url, save_mode, save_cat, folder, opts.subfolders);
        if *node != 0 {
        d.ui.scope_builder(egui::UiBuilder::new().max_rect(lr.shrink(1.0)), |ui| {
            egui::ScrollArea::horizontal().id_salt("glist").auto_shrink(false).show(ui, |ui| {
                ui.set_min_width(width);
                let (head, _) = ui.allocate_exact_size(Vec2::new(width, 20.0), Sense::hover());
                let mut x = head.left();
                for (name, w) in COLS {
                    ui.painter().text(egui::pos2(x + 6.0, head.center().y), Align2::LEFT_CENTER, tr(name), font.clone(), ink);
                    x += w * dx;
                    ui.painter().vline(x - 1.0, head.y_range().shrink(3.0), egui::Stroke::new(1.0_f32, line));
                }
                ui.painter().hline(head.x_range(), head.bottom(), egui::Stroke::new(1.0_f32, line));
                egui::ScrollArea::vertical().id_salt("glistv").auto_shrink(false).show(ui, |ui| {
                    let mut s = c.state.lock().unwrap();
                    for (n, (k, name, _)) in rows.iter().enumerate() {
                        let f = &mut s.files[*k];
                        let (row, _) = ui.allocate_exact_size(Vec2::new(width, 19.0), Sense::hover());
                        let cb = Rect::from_min_size(row.left_center() + Vec2::new(4.0, -7.0), Vec2::splat(14.0));
                        ui.put(cb, egui::Checkbox::without_text(&mut f.checked));
                        if let Some(t) = icons[n] {
                            ui.painter().image(t, Rect::from_min_size(row.left_center() + Vec2::new(22.0, -8.0), Vec2::splat(16.0)), Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
                        }
                        let job = jobs.get(&f.url).map(|j| j.info().clone());
                        let status = job.as_ref().map_or(String::new(), |i| match &i.status {
                            Status::Complete => tr("Complete"),
                            Status::Error(_) => tr("Error"),
                            Status::Downloading | Status::Connecting => format!("{}%", i.size.filter(|s| *s > 0).map_or(0, |s| i.done() * 100 / s)),
                            Status::Stopped => String::new(),
                        });
                        let cells = [String::new(), types[n].clone(), job.as_ref().and_then(|i| i.size).or(f.size).map(human).unwrap_or_default(), status, f.text.clone(), f.url.clone(), Path::new(&save_dir(name, &f.url)).join(name).display().to_string()];
                        let mut x = row.left();
                        for (ci, text) in cells.iter().enumerate() {
                            let w = COLS[ci].1 * dx;
                            let (tx, t) = if ci == 0 { (x + 42.0, name.clone()) } else { (x + 6.0, text.clone()) };
                            let clip = Rect::from_min_max(egui::pos2(x, row.top()), egui::pos2(x + w - 4.0, row.bottom()));
                            ui.painter().with_clip_rect(clip.intersect(ui.clip_rect())).text(egui::pos2(tx, row.center().y), Align2::LEFT_CENTER, t, font.clone(), ink);
                            x += w;
                        }
                    }
                });
            });
        });
        } else {
            let items: Vec<(String, Option<PathBuf>)> = {
                let s = c.state.lock().unwrap();
                rows.iter().map(|(k, name, _)| (name.clone(), jobs.get(&s.files[*k].url).map(|j| j.info().clone()).filter(|i| i.status == Status::Complete).map(|i| i.path()))).collect()
            };
            self.grab_thumbs(d.ui, lr.shrink(1.0), &items);
        }
        d.hline(7.0, 258.0, 494.0);
        // statics 1501-1506: five small sunken boxes along the bottom
        for k in 0..5 {
            let r = d.rect(18.0 + k as f32 * 20.0, 267.0, 11.0, 10.0);
            d.ui.painter().rect_stroke(r, 0.0, egui::Stroke::new(1.0_f32, Color32::from_gray(160)), egui::StrokeKind::Inside);
        }
        if d.button(250.0, 265.0, 60.0, 14.0, "< Back", true) {
            back = true;
        }
        d.button(316.0, 265.0, 60.0, 14.0, "Next >", false);
        if d.button(441.0, 265.0, 60.0, 14.0, "Close", true) {
            keep = false;
        }
        match action {
            Some(0) | Some(7) => again = true,
            Some(1) => c.stop.store(true, Relaxed),
            Some(k @ (2 | 3)) => c.state.lock().unwrap().files.iter_mut().for_each(|f| f.checked = k == 2),
            Some(4) => self.grab(c, opts, save_mode, save_cat, folder, true),
            Some(5) => {
                let ids: HashSet<u64> = jobs.values().map(|j| j.info().id).collect();
                self.core.grab_dl_ids.lock().unwrap().retain(|id| !ids.contains(id));
                jobs.values().filter(|j| j.running()).for_each(|j| j.stop());
            }
            Some(6) => self.grab(c, opts, save_mode, save_cat, folder, false),
            Some(8) => {
                let st = c.state.lock().unwrap();
                let proj = core::settings::GrabProject { opts: opts.clone(), save_mode, save_cat: save_cat.into(), folder: folder.into(), files: st.files.clone(), pages: st.pages, ..Default::default() };
                drop(st);
                self.grab_schedule(proj, saved);
            }
            Some(9) => self.open(Dialog::GrabStats { crawl: c.clone() }),
            _ => {}
        }
        if !keep {
            c.stop.store(true, Relaxed);
        }
        (keep, back, again)
    }
}
