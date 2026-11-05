use crate::app::{Align2, App, Color32, DARK_TREE, Dialog, FontId, GENERAL, HashSet, Path, Sense, Settings, Vec2, cat_icon_files, cat_icon_sets, core, dlu, egui, explore, i18n, tb_picture, texture, tr};
use crate::app::resources::{cached_texture, cat_icons_dir, glyph_strip};

#[derive(Clone, Copy, PartialEq)]
pub(in crate::app) enum Which {
    All,
    Unfinished,
    Finished,
}

/// The selected tree node: a status folder (optionally narrowed to a category), grabber projects or a queue.
#[derive(Clone, PartialEq)]
pub(in crate::app) enum Cat {
    Files(Which, Option<String>),
    /// "Grabber projects" (None) or one saved project.
    Grabber(Option<String>),
    /// "Queues" (None) or one queue.
    Queue(Option<u32>),
}

impl App {
    /// "NDM categories" (template 204): add (None) or edit a category.
    pub(in crate::app) fn edit_category(&mut self, orig: Option<String>) {
        let s = self.core.settings();
        let cat = orig.as_ref().and_then(|n| s.categories.iter().find(|c| c.name == *n).cloned()).unwrap_or_default();
        let folder = if cat.name.is_empty() { s.base_dir.clone() } else { s.folder_for(&cat.name) };
        drop(s);
        self.open(Dialog::Category { orig, cat, folder, error: String::new() });
    }

    pub(in crate::app) fn save_category(&mut self, orig: Option<&str>, cat: core::settings::Category, folder: &str) {
        let name = cat.name.clone();
        let mut s = self.core.settings();
        match orig.and_then(|o| s.categories.iter_mut().find(|c| c.name == o)) {
            Some(c) => *c = cat,
            None => s.categories.push(cat),
        }
        if let Some(o) = orig.filter(|o| *o != name) {
            s.cat_dirs.remove(o);
        }
        // only store a folder that differs from the default <base>\<name>
        let default = Path::new(&s.base_dir).join(&name).display().to_string();
        if folder.is_empty() || folder == default { s.cat_dirs.remove(&name) } else { s.cat_dirs.insert(name.clone(), folder.into()) };
        drop(s);
        if let Some(o) = orig.filter(|o| *o != name) {
            for j in self.core.jobs() {
                let mut i = j.info();
                if i.category == o {
                    i.category = name.clone();
                }
            }
            if let Cat::Files(_, Some(c)) = &mut self.cat {
                if c == o {
                    *c = name;
                }
            }
        }
        self.core.save();
        // an open Options window keeps its own copy: give it the new list (its OK would drop it otherwise)
        let (cats, dirs) = { let s = self.core.settings(); (s.categories.clone(), s.cat_dirs.clone()) };
        if self.dialogs.iter().any(|(_, d)| matches!(d, Dialog::Options { .. })) {
            self.opt_patch.push(Box::new(move |s: &mut Settings| (s.categories, s.cat_dirs) = (cats, dirs)));
        }
    }

    /// A tree item's picture from the chosen category icon set, if the set has one for it.
    pub(in crate::app) fn cat_pic(&mut self, ctx: &egui::Context, v: &Cat) -> Option<egui::TextureId> {
        let set = self.core.settings().cat_icons.clone();
        if set.is_empty() {
            return None;
        }
        let running = matches!(v, Cat::Queue(Some(q)) if self.core.queue_running(*q));
        let dir = cat_icons_dir().join(&set);
        cat_icon_files(v, running).into_iter().find_map(|name| {
            self.cat_pics
                .entry((set.clone(), name.clone()))
                .or_insert_with(|| tb_picture(&dir.join(format!("{name}.png")), 64, 64).map(|px| texture(ctx, &format!("cat_{set}_{name}"), &px)))
                .as_ref()
                .map(|t| t.id())
        })
    }

    /// View -> Category icons: NDM's own, or a set from the category icons folder.
    pub(in crate::app) fn cat_icons_menu(&mut self, ui: &mut egui::Ui) {
        let cur = self.core.settings().cat_icons.clone();
        let sets = cat_icon_sets();
        let mut pick = None;
        if ui.radio(cur.is_empty() || !sets.contains(&cur), tr("Built-in icons")).clicked() {
            pick = Some(String::new());
        }
        for name in sets {
            if ui.radio(cur == name, name.as_str()).clicked() {
                pick = Some(name);
            }
        }
        if let Some(name) = pick {
            self.core.settings().cat_icons = name;
            self.core.save();
            self.cat_pics.clear(); // changed pictures load again
            ui.close_menu();
        }
        ui.separator();
        if ui.button(tr("Open category icons folder")).clicked() {
            let dir = cat_icons_dir();
            let _ = std::fs::create_dir_all(&dir);
            explore(dir);
            ui.close_menu();
        }
    }

    /// NDM's category pane (template 102): "Categories" header frame with an x button over a
    /// SysTreeView32 (TVS_HASBUTTONS | TVS_HASLINES | TVS_LINESATROOT): dotted lines, +/- boxes,
    /// All Downloads / Unfinished / Finished each holding the categories, Grabber projects, Queues.
    pub(in crate::app) fn categories(&mut self, ui: &mut egui::Ui) {
        let dark = ui.visuals().dark_mode;
        let (edge, text) = (if dark { Color32::from_gray(90) } else { Color32::from_gray(160) }, ui.visuals().text_color());
        // View -> Custom scale: rows, icons and text of the pane
        let sc = self.core.settings().scale_tree;
        let nt = dlu::ndm().then(|| dlu::theme(dark));
        let rtl = i18n::is_rtl();
        if let Some(t) = &nt {
            // NDM Style: a quiet section title with the hide button beside it
            let head = ui.allocate_exact_size(Vec2::new(ui.available_width(), 24.0 * sc), Sense::hover()).0;
            let font = FontId::new(9.0 * sc, egui::FontFamily::Name("segoe".into()));
            let title = tr("Categories").to_uppercase();
            let (at, align) = if rtl { (head.right_center() - Vec2::new(10.0, 0.0), Align2::RIGHT_CENTER) } else { (head.left_center() + Vec2::new(10.0, 0.0), Align2::LEFT_CENTER) };
            ui.painter().text(at, align, title, font, t.muted);
            let xc = if rtl { head.left_center() + Vec2::new(12.0, 0.0) } else { head.right_center() - Vec2::new(12.0, 0.0) };
            let xr = egui::Rect::from_center_size(xc, Vec2::splat(18.0 * sc));
            let x = ui.interact(xr, ui.id().with("cats_x"), Sense::click()).on_hover_text(tr("Hide categories"));
            if x.hovered() {
                ui.painter().rect_filled(xr, dlu::R_CTRL, t.raised);
            }
            let d = 3.5 * sc;
            for (a, b) in [(Vec2::new(-d, -d), Vec2::new(d, d)), (Vec2::new(-d, d), Vec2::new(d, -d))] {
                ui.painter().line_segment([xr.center() + a, xr.center() + b], egui::Stroke::new(1.3_f32, if x.hovered() { t.text } else { t.muted }));
            }
            if x.clicked() {
                self.show_cats = false;
            }
        } else {
            // header: group frame [4,1,95,16] with "Categories" and the owner-drawn x [87,8,9,8]
            let head = ui.allocate_exact_size(Vec2::new(ui.available_width(), 14.4 * sc), Sense::hover()).0;
            ui.painter().rect_stroke(head, 0.0, egui::Stroke::new(1.0_f32, edge), egui::StrokeKind::Inside);
            if rtl {
                ui.painter().text(head.right_center() + Vec2::new(-5.0, 1.0), Align2::RIGHT_CENTER, tr("Categories"), egui::TextStyle::Body.resolve(ui.style()), text);
            } else {
                ui.painter().text(head.left_center() + Vec2::new(5.0, 1.0), Align2::LEFT_CENTER, tr("Categories"), egui::TextStyle::Body.resolve(ui.style()), text);
            }
            let xc = if rtl { head.left_center() + Vec2::new(10.0, 1.0) } else { head.right_center() - Vec2::new(10.0, -1.0) };
            let xr = egui::Rect::from_center_size(xc, Vec2::splat(13.0));
            let x = ui.interact(xr, ui.id().with("cats_x"), Sense::click()).on_hover_text(tr("Hide categories"));
            if x.hovered() {
                ui.painter().rect_filled(xr, 0.0, if dark { Color32::from_gray(80) } else { Color32::from_gray(220) });
            }
            // NDM's owner-drawn close cross
            for (a, b) in [(Vec2::new(-3.5, -3.5), Vec2::new(3.5, 3.5)), (Vec2::new(-3.5, 3.5), Vec2::new(3.5, -3.5))] {
                ui.painter().line_segment([xr.center() + a, xr.center() + b], egui::Stroke::new(1.2_f32, text));
            }
            if x.clicked() {
                self.show_cats = false;
            }
        }

        // one settings() lock per statement: two in the same expression deadlock
        let cats: Vec<String> = self.core.settings().categories.iter().map(|c| c.name.clone()).collect();
        let queues: Vec<(u32, String, bool)> = self.core.settings().queues.iter().map(|q| (q.id, tr(&q.name), q.sync)).collect();
        let img = |id: u16| self.res.tree(id);
        let cat_img = |c: &str| match c {
            "Compressed" => img(225),
            "Documents" => img(223),
            "Music" => img(220),
            "Programs" => img(222),
            "Video" => img(221),
            _ => img(215),
        };
        // (depth, key for expand state or "", closed icon, open icon, label, value, has children)
        let mut nodes: Vec<(u8, &str, Option<egui::TextureId>, Option<egui::TextureId>, String, Cat, bool)> = Vec::new();
        for (which, key, label, (closed, open)) in [(Which::All, "all", "All Downloads", (215, 214)), (Which::Unfinished, "unf", "Unfinished", (219, 218)), (Which::Finished, "fin", "Finished", (217, 216))] {
            nodes.push((0, key, img(closed), img(open), tr(label), Cat::Files(which, None), !cats.is_empty()));
            if self.expanded.contains(key) {
                nodes.extend(cats.iter().map(|c| (1, "", cat_img(c), cat_img(c), tr(c), Cat::Files(which, Some(c.clone())), false)));
            }
        }
        let projects: Vec<String> = self.core.settings().grab_projects.iter().map(|p| p.opts.name.clone()).collect();
        nodes.push((0, "gp", img(282), img(282), tr("Grabber projects"), Cat::Grabber(None), !projects.is_empty()));
        if self.expanded.contains("gp") {
            nodes.extend(projects.into_iter().map(|n| (1, "", img(282), img(282), n.clone(), Cat::Grabber(Some(n)), false)));
        }
        nodes.push((0, "q", img(311), img(313), tr("Queues"), Cat::Queue(None), true));
        if self.expanded.contains("q") {
            nodes.extend(queues.iter().map(|(id, name, sync)| {
                let i = img([[311, 312], [315, 316]][*sync as usize][self.core.queue_running(*id) as usize]);
                (1, "", i, i, name.clone(), Cat::Queue(Some(*id)), false)
            }));
        }

        let ctx = ui.ctx().clone();
        let custom: Vec<Option<egui::TextureId>> = nodes.iter().map(|n| self.cat_pic(&ctx, &n.5)).collect();
        let mut queue_dlg = None;
        let mut grab_cmd: Option<(usize, String)> = None;
        let mut new_queue = false;
        let mut cat_dlg: Option<Option<String>> = None; // Some(None) = add, Some(Some(name)) = edit
        let mut toggle = None;
        let fill = if dark { DARK_TREE } else { Color32::WHITE };
        let line = if dark { Color32::from_gray(110) } else { Color32::from_gray(160) };
        // NDM's tree at 125%: 19 px rows, 21 px indent
        let (row_h, indent) = (15.2 * sc, 17.0 * sc);
        let frame = if nt.is_some() { egui::Frame::new().inner_margin(egui::Margin { left: 4, right: 4, top: 0, bottom: 2 }) } else { egui::Frame::new().fill(fill).stroke(egui::Stroke::new(1.0_f32, edge)).inner_margin(egui::Margin { left: 2, right: 2, top: 2, bottom: 2 }) };
        // NDM Style's pictures: one-colour glyphs, coloured as they are drawn
        let gctx = ui.ctx().clone();
        let glyph = |cp: u32| {
            let px = (16.0 * sc * gctx.pixels_per_point()).round() as usize;
            cached_texture(&gctx, &format!("side{cp:x}_{px}"), || glyph_strip(&[(cp, [255; 3])], px, px, px as f32 * 0.95)).map(|t| t.id())
        };
        let running: HashSet<u32> = queues.iter().map(|q| q.0).filter(|q| self.core.queue_running(*q)).collect();
        let side_glyph = |v: &Cat| match v {
            Cat::Files(Which::All, None) => 0xE896,
            Cat::Files(Which::Unfinished, None) => 0xE769,
            Cat::Files(Which::Finished, None) => 0xE930,
            Cat::Files(_, Some(c)) => match c.as_str() {
                "Compressed" => 0xE7B8,
                "Documents" => 0xE8A5,
                "Music" => 0xE8D6,
                "Programs" => 0xE756,
                "Video" => 0xE714,
                _ => 0xE8B7,
            },
            Cat::Grabber(_) => 0xE774,
            Cat::Queue(None) => 0xE8FD,
            Cat::Queue(Some(q)) => if running.contains(q) { 0xE768 } else { 0xE823 },
        };
        frame.show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            let modern = !self.core.settings().classic;
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for (k, (depth, key, icon, open_icon, label, value, kids)) in nodes.iter().enumerate() {
                    let resp = if let Some(t) = &nt {
                    // NDM Style: tall rows, a rounded highlight, chevrons and one-colour icons, no tree lines
                    let rh = 25.0 * sc;
                    let (row, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), rh), Sense::hover());
                    let resp = ui.interact(row, ui.id().with(("node", k)), Sense::click());
                    let p = ui.painter();
                    let sel = self.cat == *value;
                    let pill = row.shrink2(Vec2::new(0.0, 1.0 * sc));
                    if sel {
                        p.rect_filled(pill, dlu::R_CTRL, t.accent_soft);
                    } else if resp.hovered() {
                        p.rect_filled(pill, dlu::R_CTRL, t.raised);
                    }
                    let dir = if rtl { -1.0 } else { 1.0 };
                    let edge_x = if rtl { row.right() } else { row.left() };
                    let x0 = edge_x + (6.0 + *depth as f32 * 16.0) * sc * dir;
                    let my = row.center().y;
                    if *kids {
                        let open = !key.is_empty() && self.expanded.contains(*key);
                        let c = egui::pos2(x0 + 5.0 * sc * dir, my);
                        let v = |x: f32, y: f32| c + Vec2::new(x * sc * dir, y * sc);
                        let pts = if open { vec![v(-3.5, -1.5), v(0.0, 2.0), v(3.5, -1.5)] } else { vec![v(-1.5, -3.5), v(2.0, 0.0), v(-1.5, 3.5)] };
                        let hr = egui::Rect::from_center_size(c, Vec2::splat(14.0 * sc));
                        let er = ui.interact(hr, ui.id().with(("exp", k)), Sense::click());
                        ui.painter().add(egui::Shape::line(pts, egui::Stroke::new(1.4_f32, if er.hovered() { t.text } else { t.muted })));
                        if er.clicked() {
                            toggle = Some(key.to_string());
                        }
                    }
                    let ix = x0 + 14.0 * sc * dir;
                    let isz = 16.0 * sc;
                    let ir = egui::Rect::from_min_size(egui::pos2(if rtl { ix - isz } else { ix }, my - isz / 2.0), Vec2::splat(isz));
                    let ink = if sel { t.accent } else if *depth == 0 { t.text } else { t.muted };
                    if let Some(tex) = custom[k] {
                        ui.painter().image(tex, ir, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
                    } else if let Some(tex) = glyph(side_glyph(value)) {
                        ui.painter().image(tex, ir, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), if sel { t.accent } else { t.muted });
                    }
                    let lx = ix + (isz + 7.0 * sc) * dir;
                    let room = (if rtl { lx - row.left() } else { row.right() - lx }) - 4.0;
                    // a longer translation gets smaller type before it is cut short
                    let font = dlu::fit_font(ui, label, FontId::new(10.4 * sc, egui::FontFamily::Name("segoe".into())), room);
                    let label = if rtl { dlu::cut_left(label, room, |l| ui.fonts(|f| f.layout_no_wrap(l.to_string(), font.clone(), ink).size().x)) } else { label.clone() };
                    let mut job = egui::text::LayoutJob::simple_singleline(label, font, if sel { t.accent } else { ink });
                    job.wrap = egui::text::TextWrapping { max_width: room.max(10.0), max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
                    let g = ui.painter().layout_job(job);
                    let gx = if rtl { lx - g.size().x } else { lx };
                    ui.painter().galley(egui::pos2(gx, my - g.size().y / 2.0), g, ink);
                    let _ = (icon, open_icon);
                    resp
                    } else {
                    let (row, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), row_h), Sense::hover());
                    let p = ui.painter();
                    // RTL languages mirror the tree as Windows does (WS_EX_LAYOUTRTL): levels grow leftwards
                    let dir = if rtl { -1.0 } else { 1.0 };
                    let cx = |d: u8| if rtl { row.right() - 7.0 * sc - d as f32 * indent } else { row.left() + 7.0 * sc + d as f32 * indent }; // centre of a level's +/- box
                    let my = row.center().y;
                    // dotted lines: to the item, down to the next sibling, and the parent's line passing by
                    let dots = |a: egui::Pos2, b: egui::Pos2| {
                        let n = ((b - a).length() / 2.0) as i32;
                        for s in (0..=n).step_by(2) {
                            let t = a + (b - a) * (s as f32 / n.max(1) as f32);
                            p.rect_filled(egui::Rect::from_min_size(t, Vec2::splat(1.0)), 0.0, line);
                        }
                    };
                    let next_sibling = nodes[k + 1..].iter().take_while(|n| n.0 >= *depth).any(|n| n.0 == *depth);
                    let first = k == 0;
                    dots(egui::pos2(cx(*depth), if first { my } else { row.top() }), egui::pos2(cx(*depth), if next_sibling { row.bottom() } else { my }));
                    dots(egui::pos2(cx(*depth).min(cx(*depth) + 10.0 * dir), my), egui::pos2(cx(*depth).max(cx(*depth) + 10.0 * dir), my));
                    if *depth == 1 && nodes[k + 1..].iter().any(|n| n.0 == 0) {
                        dots(egui::pos2(cx(0), row.top()), egui::pos2(cx(0), row.bottom()));
                    }
                    let open = !key.is_empty() && self.expanded.contains(*key);
                    if *kids && modern {
                        // NDM (Explorer-themed tree): a chevron instead of the +/- box
                        let c = egui::pos2(cx(*depth), my);
                        let v = |x: f32, y: f32| c + Vec2::new(x * dir, y);
                        let pts = if open { vec![v(-3.5, -1.5), v(0.0, 2.0), v(3.5, -1.5)] } else { vec![v(-1.5, -3.5), v(2.0, 0.0), v(-1.5, 3.5)] };
                        p.add(egui::Shape::line(pts, egui::Stroke::new(1.2_f32, Color32::from_gray(if self.dark { 190 } else { 90 }))));
                        if ui.interact(egui::Rect::from_center_size(c, Vec2::splat(13.0)), ui.id().with(("exp", k)), Sense::click()).clicked() {
                            toggle = Some(key.to_string());
                        }
                    } else if *kids {
                        let b = egui::Rect::from_center_size(egui::pos2(cx(*depth), my), Vec2::splat(9.0));
                        p.rect_filled(b, 0.0, fill);
                        p.rect_stroke(b, 0.0, egui::Stroke::new(1.0_f32, line), egui::StrokeKind::Inside);
                        p.hline(b.x_range().shrink(2.0), my, egui::Stroke::new(1.0_f32, text));
                        if !open {
                            p.vline(b.center().x, b.y_range().shrink(2.0), egui::Stroke::new(1.0_f32, text));
                        }
                        if ui.interact(b.expand(2.0), ui.id().with(("exp", k)), Sense::click()).clicked() {
                            toggle = Some(key.to_string());
                        }
                    }
                    let sel = self.cat == *value;
                    // x: the icon's edge nearest the line; in RTL the icon and label run leftwards from it
                    let mut x = cx(*depth) + 9.0 * sc * dir;
                    if let Some(t) = custom[k] {
                        let left = if rtl { x - 15.0 * sc } else { x + 1.0 * sc };
                        p.image(t, egui::Rect::from_min_size(egui::pos2(left, my - 7.0 * sc), Vec2::splat(14.0 * sc)), egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
                    } else if let Some(t) = if open || sel { open_icon.or(*icon) } else { *icon } {
                        let left = if rtl { x - 16.0 * sc } else { x };
                        p.image(t, egui::Rect::from_min_size(egui::pos2(left, my - 6.5 * sc), Vec2::new(16.0, 13.0) * sc), egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
                    }
                    x += 17.0 * sc * dir;
                    let font = FontId::proportional(10.0 * sc);
                    let g = ui.painter().layout_no_wrap(label.clone(), font, if sel { Color32::WHITE } else { text });
                    let lw = g.size().x + 4.0;
                    let lr = egui::Rect::from_min_size(egui::pos2(if rtl { x - lw } else { x }, my - 8.0 * sc), Vec2::new(lw, 16.0 * sc));
                    if sel {
                        ui.painter().rect_filled(lr, 0.0, if dark { Color32::from_gray(0x50) } else { Color32::from_rgb(0, 120, 215) });
                    }
                    ui.painter().galley(egui::pos2(lr.left() + 2.0, my - g.size().y / 2.0), g, text);
                    let hit = if rtl { egui::Rect::from_min_max(egui::pos2(row.left(), row.top()), egui::pos2(x + 19.0, row.bottom())) } else { egui::Rect::from_min_max(egui::pos2(x - 19.0, row.top()), egui::pos2(row.right(), row.bottom())) };
                    ui.interact(hit, ui.id().with(("node", k)), Sense::click())
                    };
                    if resp.clicked() || resp.secondary_clicked() {
                        self.cat = value.clone();
                    }
                    if resp.double_clicked() && *kids {
                        toggle = Some(key.to_string());
                    }
                    // NDM's tree menus: category popup +0x220, grabber +0x224, queue +0x228.
                    resp.context_menu(|ui| match value {
                        Cat::Queue(q) => {
                            let id = q.unwrap_or(core::queue::MAIN_QUEUE);
                            let on = self.core.queue_running(id);
                            if ui.add_enabled(q.is_some() && !on, egui::Button::new(tr("Start now"))).clicked() {
                                self.core.start_queue(id);
                                ui.close_menu();
                            }
                            if ui.add_enabled(q.is_some() && on, egui::Button::new(tr("Stop"))).clicked() {
                                self.core.stop_queue(id);
                                ui.close_menu();
                            }
                            ui.separator();
                            for (t, tab) in [("Edit queue", 1), ("Schedule", 0)] {
                                if ui.add_enabled(q.is_some(), egui::Button::new(tr(t))).clicked() {
                                    queue_dlg = Some((id, tab));
                                    ui.close_menu();
                                }
                            }
                            ui.separator();
                            if ui.add_enabled(q.is_some_and(|q| self.core.settings().queue(q).is_some_and(|x| !x.builtin())), egui::Button::new(tr("Delete"))).clicked() {
                                self.core.delete_queue(id);
                                self.cat = Cat::Queue(None);
                                ui.close_menu();
                            }
                            ui.separator();
                            if ui.button(tr("Create new queue")).clicked() {
                                new_queue = true;
                                ui.close_menu();
                            }
                        }
                        Cat::Grabber(p) => {
                            for (k, t) in ["Open project", "Schedule project", "Rename project", "Delete project"].into_iter().enumerate() {
                                if ui.add_enabled(p.is_some(), egui::Button::new(tr(t))).clicked() {
                                    grab_cmd = p.clone().map(|p| (k, p));
                                    ui.close_menu();
                                }
                            }
                        }
                        Cat::Files(_, c) => {
                            let c = c.as_deref().unwrap_or(GENERAL);
                            if ui.button(tr("Browse")).clicked() {
                                explore(self.core.settings().folder_for(c));
                                ui.close_menu();
                            }
                            if ui.add_enabled(c != GENERAL, egui::Button::new(tr("Properties"))).clicked() {
                                cat_dlg = Some(Some(c.to_string()));
                                ui.close_menu();
                            }
                            ui.separator();
                            if ui.button(tr("Add category")).clicked() {
                                cat_dlg = Some(None);
                                ui.close_menu();
                            }
                            if ui.add_enabled(c != GENERAL, egui::Button::new(tr("Delete"))).clicked() {
                                self.open(Dialog::CatDelete { name: c.to_string() });
                                ui.close_menu();
                            }
                        }
                    });
                }
            });
        });
        if let Some((k, name)) = grab_cmd {
            self.project_cmd(k, &name);
        }
        if let Some(key) = toggle {
            if !self.expanded.remove(&key) {
                self.expanded.insert(key);
            }
            self.core.settings().expanded = self.expanded.iter().cloned().collect();
        }
        if let Some((id, tab)) = queue_dlg {
            self.open_scheduler(tab);
            if let Some((_, Dialog::Scheduler { queue, .. })) = self.dialogs.last_mut() {
                *queue = self.core.settings().queue(id).cloned().unwrap_or_default();
            }
        }
        if new_queue {
            self.new_queue(Vec::new());
        }
        if let Some(orig) = cat_dlg {
            self.edit_category(orig);
        }
    }
}
