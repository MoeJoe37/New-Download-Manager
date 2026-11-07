use crate::app::{
    core, date, dlu, egui, explore, human, i18n, open_folder_of, open_with, rate, time_left, tr,
    tr_os, Align, App, Cat, Color32, Column, Dialog, HashMap, HashSet, Layout, PathBuf, RichText,
    Sense, Status, TableBuilder, Vec2, Which,
};

/// List row height: NDM's 19 px at 125%.
pub(in crate::app) const ROW_H: f32 = 15.2;

/// List columns in NDM's own index order.
#[derive(Clone, Copy, PartialEq)]
pub(in crate::app) enum Col {
    Name,
    Added,
    Q,
    Size,
    Status,
    Left,
    Rate,
    LastTry,
    Desc,
    SaveTo,
    Referer,
    Parent,
}

/// (column, header, default width in px).
pub(in crate::app) const COLS: [(Col, &str, u16); 12] = [
    (Col::Name, "File Name", 150),
    (Col::Added, "Date Added", 65),
    (Col::Q, "Q", 20),
    (Col::Size, "Size", 75),
    (Col::Status, "Status", 65),
    (Col::Left, "Time left", 75),
    (Col::Rate, "Transfer rate", 95),
    (Col::LastTry, "Last Try Date", 65),
    (Col::Desc, "Description", 200),
    (Col::SaveTo, "Save To", 200),
    (Col::Referer, "Referer", 200),
    (Col::Parent, "Parent web page", 200),
];

#[derive(Default)]
pub(in crate::app) struct Row {
    pub(in crate::app) id: u64,
    pub(in crate::app) name: String,
    pub(in crate::app) url: String,
    pub(in crate::app) path: PathBuf,
    pub(in crate::app) queued: bool,
    pub(in crate::app) queue: u32,
    pub(in crate::app) size: Option<u64>,
    pub(in crate::app) done: u64,
    pub(in crate::app) status: Status,
    pub(in crate::app) running: bool,
    pub(in crate::app) speed: f64,
    pub(in crate::app) last_try: i64,
    pub(in crate::app) added: i64,
    pub(in crate::app) desc: String,
    pub(in crate::app) category: String,
    pub(in crate::app) referrer: String,
    pub(in crate::app) parent: String,
    /// After the data is in: what NDM does with it and how far along (engine::Info::post).
    pub(in crate::app) post: Option<(String, f32)>,
}

impl Row {
    fn left(&self) -> Option<u64> {
        let size = self.size?;
        (self.running && self.speed > 0.0)
            .then(|| (size.saturating_sub(self.done) as f64 / self.speed) as u64)
    }
    pub(in crate::app) fn pct(&self) -> Option<f64> {
        if let Some((_, f)) = &self.post {
            // processing: the same bar starts again for it
            return (*f >= 0.0).then_some(*f as f64 * 100.0);
        }
        self.size
            .filter(|s| *s > 0)
            .map(|s| self.done as f64 * 100.0 / s as f64)
    }
    pub(in crate::app) fn status_text(&self) -> String {
        if let Some((_, f)) = self.post.as_ref().filter(|_| self.running) {
            return if *f >= 0.0 {
                format!("{} {:.0}%", tr_os("Processing"), f * 100.0)
            } else {
                tr("Processing...")
            };
        }
        match &self.status {
            Status::Complete => tr("Complete"),
            Status::Error(_) => tr("Error"),
            Status::Connecting => tr("Connecting..."),
            _ if self.done == 0 && !self.running => String::new(),
            _ => self
                .pct()
                .map_or_else(|| human(self.done), |p| format!("{p:.2}%")),
        }
    }
    pub(in crate::app) fn cell(&self, c: Col) -> String {
        match c {
            Col::Name => {
                if self.name.is_empty() {
                    self.url.clone()
                } else {
                    self.name.clone()
                }
            }
            Col::Q => {
                if self.queued {
                    "Q".into()
                } else {
                    String::new()
                }
            }
            Col::Size => self.size.map(human).unwrap_or_default(),
            Col::Status => self.status_text(),
            Col::Left => self.left().map(time_left).unwrap_or_default(),
            Col::Rate => {
                if self.running {
                    rate(self.speed)
                } else {
                    String::new()
                }
            }
            Col::LastTry => date(self.last_try),
            Col::Desc => self.desc.clone(),
            Col::Added => date(self.added),
            Col::SaveTo => self
                .path
                .parent()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            Col::Referer => self.referrer.clone(),
            Col::Parent => self.parent.clone(),
        }
    }
}

/// Does download `r` match the search: each ticked target is compared as a
/// substring, or whole with flag 2; case-insensitive unless flag 1.
pub(in crate::app) fn find_hit(flags: u32, needle: &str, r: &Row) -> bool {
    let up = |t: &str| {
        if flags & 1 != 0 {
            t.to_string()
        } else {
            t.to_uppercase()
        }
    };
    let n = up(needle);
    let hit = |t: &str| {
        if flags & 2 != 0 {
            up(t) == n
        } else {
            up(t).contains(&n)
        }
    };
    (flags & 4 != 0 && hit(&r.name))
        || (flags & 8 != 0 && hit(&r.desc))
        || (flags & 0x10 != 0 && [&r.url, &r.parent, &r.referrer].iter().any(|t| hit(t)))
}

impl App {
    pub(in crate::app) fn open_find(&mut self) {
        let (text, flags) = {
            let s = self.core.settings();
            (s.search.clone(), s.search_flags)
        };
        self.open(Dialog::Find { text, flags });
    }

    /// Find Next: select the next matching download from `find_pos` on, wrapping
    /// to the top; "No such files found" when nothing in the list matches.
    pub(in crate::app) fn find_next(&mut self) {
        let (needle, flags) = {
            let s = self.core.settings();
            (s.search.clone(), s.search_flags)
        };
        if needle.is_empty() {
            return self.open_find();
        }
        let rows = self.rows();
        let from = self.find_pos.min(rows.len());
        match (from..rows.len())
            .chain(0..from)
            .find(|&k| find_hit(flags, &needle, &rows[k]))
        {
            Some(k) => {
                self.sel = HashSet::from([rows[k].id]);
                self.anchor = Some(rows[k].id);
                (self.find_pos, self.scroll_to) = (k + 1, Some(k));
            }
            None => self.open(Dialog::Info {
                title: "Find file".into(),
                text: "No such files found".into(),
            }),
        }
    }

    pub(in crate::app) fn rows(&self) -> Vec<Row> {
        let grab_urls: HashSet<String> = match &self.cat {
            Cat::Grabber(Some(p)) => self
                .core
                .settings()
                .grab_projects
                .iter()
                .filter(|x| x.opts.name == *p)
                .flat_map(|x| x.files.iter().map(|f| f.url.clone()))
                .collect(),
            _ => HashSet::new(),
        };
        let mut rows: Vec<Row> = self
            .core
            .jobs()
            .iter()
            .map(|j| {
                let speed = j.speed();
                let i = j.info();
                Row {
                    id: i.id,
                    name: i.name.clone(),
                    url: i.url.clone(),
                    path: i.path(),
                    queued: i.queued,
                    queue: i.queue,
                    size: i.size,
                    done: i.done(),
                    status: i.status.clone(),
                    running: j.running(),
                    speed,
                    last_try: i.last_try,
                    added: i.added,
                    desc: i.description.clone(),
                    category: i.category.clone(),
                    referrer: i.referrer.clone(),
                    parent: i.parent.clone(),
                    post: i.post.clone(),
                }
            })
            .filter(|r| match &self.cat {
                Cat::Files(w, c) => {
                    let st = match w {
                        Which::All => true,
                        Which::Unfinished => r.status != Status::Complete,
                        Which::Finished => r.status == Status::Complete,
                    };
                    st && c.as_ref().is_none_or(|c| r.category == *c)
                }
                Cat::Grabber(None) => false,
                Cat::Grabber(Some(_)) => grab_urls.contains(r.url.as_str()),
                Cat::Queue(q) => r.queued && q.is_none_or(|q| r.queue == q),
            })
            .filter(|r| {
                let f = self.filter.trim().to_lowercase();
                f.is_empty()
                    || [&r.name, &r.url, &r.desc]
                        .iter()
                        .any(|t| t.to_lowercase().contains(&f))
            })
            .collect();
        let (col, asc) = self.sort;
        rows.sort_by(|a, b| {
            let o = match col {
                Col::Added => (a.added, a.id).cmp(&(b.added, b.id)),
                Col::Name => a
                    .cell(Col::Name)
                    .to_lowercase()
                    .cmp(&b.cell(Col::Name).to_lowercase()),
                Col::Q => a.queued.cmp(&b.queued),
                Col::Size => a.size.cmp(&b.size),
                Col::Status => a.pct().unwrap_or(0.0).total_cmp(&b.pct().unwrap_or(0.0)),
                Col::Left => a.left().cmp(&b.left()),
                Col::Rate => a.speed.total_cmp(&b.speed),
                Col::LastTry => a.last_try.cmp(&b.last_try),
                Col::Desc => a.desc.cmp(&b.desc),
                Col::SaveTo => a.path.cmp(&b.path),
                Col::Referer => a.referrer.cmp(&b.referrer),
                Col::Parent => a.parent.cmp(&b.parent),
            };
            if asc {
                o
            } else {
                o.reverse()
            }
        });
        rows
    }

    pub(in crate::app) fn sort_by(&mut self, c: Col) {
        self.sort = if self.sort.0 == c {
            (c, !self.sort.1)
        } else {
            (c, true)
        };
    }

    pub(in crate::app) fn table(&mut self, ui: &mut egui::Ui, rows: &[Row]) {
        let (ctrl, shift) = ui.input(|i| (i.modifiers.command, i.modifiers.shift));
        let mut sort = None;
        let mut action: Option<(usize, bool)> = None; // (row, double click)
        let mut menu_cmd: Option<(&str, u64)> = None;
        let mut columns_dlg = false;
        let mut dbl = None;
        let mut to_queue: Option<(Option<u32>, u64)> = None; // (queue or a new one, row)
        let queues: Vec<(u32, String)> = self
            .core
            .settings()
            .queues
            .iter()
            .map(|q| (q.id, tr(&q.name)))
            .collect();
        let (mut cols, dbl_open): (Vec<(Col, &str, f32)>, bool) = {
            let s = self.core.settings();
            (
                s.columns
                    .iter()
                    .filter_map(|(k, w)| COLS.get(*k as usize).map(|c| (c.0, c.1, *w as f32)))
                    .collect(),
                s.dbl_open,
            )
        };
        if i18n::is_rtl() {
            cols.reverse(); // columns mirror, as the tree does
        }
        let icons: Vec<Option<egui::TextureId>> = rows
            .iter()
            .map(|r| self.icon(ui.ctx(), &core::ext(&r.cell(Col::Name))))
            .collect();
        let nt = dlu::ndm().then(|| dlu::theme(ui.visuals().dark_mode));
        let font_h = egui::TextStyle::Body.resolve(ui.style()).size;
        // taller rows for a bigger font; NDM Style gives every row room to breathe
        let row_h = if nt.is_some() {
            (font_h * 2.4).max(24.0)
        } else {
            ROW_H.max(font_h * 1.5)
        };
        // Q column: the queue's tree picture (main / synchronization, idle / running) like NDM's list
        let qpics: HashMap<u32, egui::TextureId> = self
            .core
            .settings()
            .queues
            .iter()
            .filter_map(|q| {
                self.res
                    .tree(
                        [[311, 312], [315, 316]][q.sync as usize]
                            [self.core.queue_running(q.id) as usize],
                    )
                    .map(|t| (q.id, t))
            })
            .collect();
        let mut row_rects = Vec::new();
        if let Some(t) = &nt {
            ui.visuals_mut().widgets.noninteractive.bg_stroke.color = t.raised; // the column edges
        }
        // NDM's columns touch; cell text starts 7 px in (icon 8 px), measured at 125%
        ui.spacing_mut().item_spacing = Vec2::ZERO;
        let mut tb = TableBuilder::new(ui);
        if let Some(k) = self.scroll_to.take() {
            tb = tb.scroll_to_row(k, Some(Align::Center));
        }
        // right-to-left languages: cells read from the right edge, the file icon right of the name
        let cell = if i18n::is_rtl() {
            Layout::right_to_left(Align::Center)
        } else {
            Layout::left_to_right(Align::Center)
        };
        let mut tb = tb
            .striped(false)
            .resizable(true)
            .auto_shrink(false)
            .sense(Sense::click())
            .cell_layout(cell);
        for (k, (_, _, w)) in cols.iter().enumerate() {
            tb = tb.column(if k + 1 == cols.len() {
                Column::remainder().at_least(*w)
            } else {
                Column::initial(*w).at_least(20.0).clip(true)
            });
        }
        // NDM's list at 125%: 24 px header, 19 px rows
        tb.header(
            if nt.is_some() {
                (font_h * 2.4).max(24.0)
            } else {
                19.2
            },
            |mut h| {
                for (c, name, _) in &cols {
                    let (_, resp) = h.col(|ui| {
                        ui.add_space(if nt.is_some() { 8.0 } else { 4.0 });
                        let arrow = if self.sort.0 == *c {
                            if self.sort.1 {
                                " ⏶"
                            } else {
                                " ⏷"
                            }
                        } else {
                            ""
                        };
                        let head = match &nt {
                            Some(t) => {
                                RichText::new(i18n::line(&format!("{}{arrow}", tr_os(name))))
                                    .color(if self.sort.0 == *c { t.accent } else { t.muted })
                            }
                            None => RichText::new(i18n::line(&format!("{}{arrow}", tr_os(name)))),
                        };
                        // a longer translation gets smaller type in a narrow column
                        let head = dlu::fit(
                            ui,
                            head.into(),
                            ui.available_width() - 4.0,
                            egui::TextStyle::Body,
                        );
                        if ui
                            .add(
                                egui::Label::new(head)
                                    .selectable(false)
                                    .sense(Sense::click()),
                            )
                            .clicked()
                        {
                            sort = Some(*c);
                        }
                    });
                    // NDM's header menu has one item (0x4613c0, popup +0x230)
                    resp.context_menu(|ui| {
                        crate::dlu::scale_popup(ui);
                        if ui.button(tr("Columns...")).clicked() {
                            columns_dlg = true;
                            ui.close_menu();
                        }
                    });
                }
            },
        )
        .body(|body| {
            body.rows(row_h, rows.len(), |mut row| {
                let k = row.index();
                let r = &rows[k];
                row.set_selected(self.sel.contains(&r.id));
                for (c, _, _) in &cols {
                    row.col(|ui| {
                        ui.add_space(if nt.is_some() {
                            8.0
                        } else if *c == Col::Name {
                            1.6
                        } else {
                            4.0
                        });
                        if *c == Col::Name {
                            if let Some(t) = icons[k] {
                                ui.add(egui::Image::new((t, Vec2::splat(16.0))));
                                ui.add_space(2.0);
                            }
                        }
                        if let Some(t) = qpics.get(&r.queue).filter(|_| *c == Col::Q && r.queued) {
                            ui.add(egui::Image::new((*t, Vec2::new(16.0, 13.0))));
                            return;
                        }
                        // names, folders and addresses as they are written (Arabic and Hebrew in reading order)
                        let text = match c {
                            Col::Name | Col::Desc | Col::SaveTo | Col::Referer | Col::Parent => {
                                i18n::shown(&r.cell(*c))
                            }
                            _ => r.cell(*c),
                        };
                        let error = matches!(r.status, Status::Error(_));
                        if let (Some(t), Col::Status) = (&nt, *c) {
                            // NDM Style: the status in its colour over a thin progress bar
                            let ink = if error {
                                t.danger
                            } else if r.status == Status::Complete {
                                t.success
                            } else if r.running {
                                t.accent
                            } else {
                                t.muted
                            };
                            if let Some(f) =
                                r.pct().filter(|f| *f > 0.0 && r.status != Status::Complete)
                            {
                                let (cell, rtl) = (ui.max_rect(), i18n::is_rtl());
                                let (x0, x1) = if rtl {
                                    (cell.left() + 10.0, cell.right())
                                } else {
                                    (cell.left(), cell.right() - 10.0)
                                };
                                let bar = egui::Rect::from_min_max(
                                    egui::pos2(x0, cell.bottom() - 6.0),
                                    egui::pos2(x1, cell.bottom() - 3.0),
                                );
                                if bar.width() > 8.0 {
                                    ui.painter().rect_filled(bar, 1.5, t.raised);
                                    // filling from where the text starts (the right in right-to-left languages)
                                    let w = bar.width() * (f / 100.0).clamp(0.0, 1.0) as f32;
                                    let done = if rtl {
                                        egui::Rect::from_min_max(
                                            egui::pos2(bar.right() - w, bar.top()),
                                            bar.max,
                                        )
                                    } else {
                                        egui::Rect::from_min_size(
                                            bar.min,
                                            Vec2::new(w, bar.height()),
                                        )
                                    };
                                    ui.painter().rect_filled(
                                        done,
                                        1.5,
                                        if r.running { t.accent } else { t.muted },
                                    );
                                }
                            }
                            ui.add(
                                egui::Label::new(RichText::new(text).color(ink))
                                    .selectable(false)
                                    .truncate(),
                            );
                            return;
                        }
                        let text = if *c == Col::Status && error {
                            RichText::new(text).color(Color32::RED)
                        } else {
                            RichText::new(text)
                        };
                        ui.add(egui::Label::new(text).selectable(false).truncate());
                    });
                }
                let resp = row.response();
                if resp.rect.is_positive() {
                    row_rects.push(resp.rect); // a row egui didn't lay out has Rect::NOTHING (bottom = -inf)
                }
                if resp.double_clicked() {
                    action = Some((k, true));
                } else if resp.clicked() || (resp.secondary_clicked() && !self.sel.contains(&r.id))
                {
                    action = Some((k, false));
                }
                // the list's context menu
                resp.context_menu(|ui| {
                    crate::dlu::scale_popup(ui);
                    let done = r.status == Status::Complete;
                    let mut item = |ui: &mut egui::Ui, cmd: &'static str, enabled: bool| {
                        if ui
                            .add_enabled(enabled, egui::Button::new(tr(cmd)))
                            .clicked()
                        {
                            menu_cmd = Some((cmd, r.id));
                            ui.close_menu();
                        }
                    };
                    item(ui, "Open", done);
                    item(ui, "Open with...", done);
                    item(ui, "Open folder", true);
                    ui.separator();
                    // NDM shows this item behind a setting (0x469250, DAT_00758d28); always on here
                    item(ui, "Move/Rename (Ctrl-M)", !r.running);
                    ui.separator();
                    item(ui, "Redownload", !r.running);
                    ui.separator();
                    item(ui, "Resume Download", !done && !r.running);
                    item(ui, "Stop Download", r.running);
                    item(ui, "Speed limit...", !done);
                    ui.separator();
                    item(
                        ui,
                        "Recover download",
                        matches!(r.status, Status::Error(_)) && !r.running,
                    );
                    item(ui, "Refresh download address", !done);
                    ui.separator();
                    item(ui, "Remove", true);
                    ui.separator();
                    ui.add_enabled_ui(!done, |ui| {
                        ui.menu_button(tr("Add to download queue"), |ui| {
                            crate::dlu::scale_popup(ui);
                            for (id, name) in &queues {
                                if ui
                                    .add_enabled(
                                        !(r.queued && r.queue == *id),
                                        egui::Button::new(name),
                                    )
                                    .clicked()
                                {
                                    to_queue = Some((Some(*id), r.id));
                                    ui.close_menu();
                                }
                            }
                            ui.separator();
                            if ui.button(tr("Create new queue")).clicked() {
                                to_queue = Some((None, r.id));
                                ui.close_menu();
                            }
                        });
                    });
                    item(ui, "Delete from queue", r.queued);
                    ui.separator();
                    ui.menu_button(tr("On Double click"), |ui| {
                        crate::dlu::scale_popup(ui);
                        for (on, label) in [(true, "Open"), (false, "Properties")] {
                            if ui.radio(dbl_open == on, tr(label)).clicked() {
                                dbl = Some(on);
                                ui.close_menu();
                            }
                        }
                    });
                    ui.separator();
                    item(ui, "Properties", true);
                });
            });
        });
        if let Some(t) = &nt {
            // NDM Style: a hairline between rows, none below the last
            for r in &row_rects {
                ui.painter().hline(
                    r.x_range(),
                    r.bottom() + ui.spacing().item_spacing.y / 2.0,
                    egui::Stroke::new(1.0_f32, t.raised),
                );
            }
        } else {
            // NDM's list grid (LVS_EX_GRIDLINES / its dark-mode custom grid, 0x475710): a line under every row,
            // continued down the empty part of the list
            let area = ui.min_rect().intersect(ui.clip_rect()); // only the visible part
                                                                // the fixed row pitch: a measured row rect can be 0 tall, and a 0 pitch never ends the loop below
            let pitch = row_h + ui.spacing().item_spacing.y;
            let mut y = row_rects
                .last()
                .map_or(area.top() + 22.0 + ui.spacing().item_spacing.y, |r| {
                    r.bottom() + ui.spacing().item_spacing.y / 2.0
                })
                .max(area.top());
            let stroke = egui::Stroke::new(
                1.0_f32,
                Color32::from_gray(if self.dark { 85 } else { 225 }),
            );
            for r in &row_rects {
                ui.painter().hline(
                    area.x_range(),
                    r.bottom() + ui.spacing().item_spacing.y / 2.0,
                    stroke,
                );
            }
            while y < area.bottom() {
                ui.painter().hline(area.x_range(), y, stroke);
                y += pitch;
            }
        }
        if columns_dlg {
            self.open_columns();
        }
        if let Some(on) = dbl {
            self.core.settings().dbl_open = on;
        }
        if let Some((q, id)) = to_queue {
            let ids: Vec<u64> = if self.sel.contains(&id) {
                self.sel.iter().copied().collect()
            } else {
                vec![id]
            };
            match q {
                Some(q) => self.core.set_queue(&ids, Some(q)),
                None => self.new_queue(ids),
            }
        }
        if let Some(c) = sort {
            self.sort_by(c);
        }
        if let Some((k, double)) = action {
            let id = rows[k].id;
            if double {
                self.sel = HashSet::from([id]);
                if dbl_open && rows[k].status == Status::Complete {
                    explore(&rows[k].path);
                } else {
                    self.properties(&rows[k]);
                }
            } else if shift && self.anchor.is_some() {
                let a = rows
                    .iter()
                    .position(|r| Some(r.id) == self.anchor)
                    .unwrap_or(k);
                self.sel = rows[a.min(k)..=a.max(k)].iter().map(|r| r.id).collect();
            } else if ctrl {
                if !self.sel.remove(&id) {
                    self.sel.insert(id);
                }
                self.anchor = Some(id);
            } else {
                self.sel = HashSet::from([id]);
                self.anchor = Some(id);
            }
        }
        if let Some((cmd, id)) = menu_cmd {
            let ids: Vec<u64> = if self.sel.contains(&id) {
                self.sel.iter().copied().collect()
            } else {
                vec![id]
            };
            let row = rows.iter().find(|r| r.id == id).unwrap();
            match cmd {
                "Open" => explore(&row.path),
                "Open with..." => {
                    open_with(&row.path);
                }
                "Open folder" => open_folder_of(&row.path),
                "Move/Rename (Ctrl-M)" => self.move_rename(id),
                "Recover download" => self.open(Dialog::Recovery { id }),
                "Refresh download address" => self.refresh_address(id),
                "Resume Download" => self.resume(&ids),
                "Stop Download" => self.stop(&ids),
                "Redownload" => self.redownload(&ids),
                "Delete from queue" => self.core.set_queue(&ids, None),
                "Speed limit..." => {
                    let kb = self.core.find(id).map_or(0, |j| j.info().limit_kb);
                    self.open(Dialog::SpeedLimit {
                        ids,
                        on: kb > 0,
                        kb: if kb > 0 { kb.to_string() } else { "200".into() },
                    });
                }
                "Remove" => self.ask_delete(ids),
                _ => self.properties(row),
            }
        }
    }

    /// Move/Rename: pick a new path; a finished file is moved on disk, an unfinished one moves its .part.
    pub(in crate::app) fn move_rename(&mut self, id: u64) {
        let Some(j) = self.core.find(id).filter(|j| !j.running()) else {
            return;
        };
        let (path, done) = {
            let i = j.info();
            (i.path(), i.status == Status::Complete)
        };
        let mut fd = rfd::FileDialog::new().set_title(tr_os("Move/Rename"));
        if let Some(dir) = path.parent().filter(|d| d.exists()) {
            fd = fd.set_directory(dir);
        }
        if let Some(n) = path.file_name() {
            fd = fd.set_file_name(n.to_string_lossy());
        }
        let Some(to) = fd.save_file() else { return };
        if to == path {
            return;
        }
        let mut i = j.info();
        let from = if done { i.path() } else { i.part() };
        let mut moved_info = i.clone();
        moved_info.folder = to
            .parent()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        moved_info.name = to
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let target = if done { to.clone() } else { moved_info.part() };
        // rename, or copy + delete when moving to another drive
        let moved = !from.exists()
            || std::fs::rename(&from, &target).is_ok()
            || (std::fs::copy(&from, &target).is_ok() && std::fs::remove_file(&from).is_ok());
        if moved {
            i.folder = to
                .parent()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            i.name = to
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
        } else {
            drop(i);
            self.open(Dialog::Info {
                title: "Move/Rename".into(),
                text: i18n::trf_os(
                    "Cannot move %s to %s.",
                    &[
                        &i18n::ltr(&from.display().to_string()),
                        &i18n::ltr(&target.display().to_string()),
                    ],
                ),
            });
        }
    }

    /// "Properties": the progress window for unfinished downloads, the completion window otherwise.
    pub(in crate::app) fn properties(&mut self, row: &Row) {
        let id = row.id;
        if row.status != Status::Complete {
            let details = self.core.settings().prog_details;
            return self.open(Dialog::Progress {
                id,
                tab: 0,
                details,
                min: false,
            });
        }
        self.file_properties(id);
    }

    /// View -> Customize URL List / header menu -> Columns... (template 209).
    pub(in crate::app) fn open_columns(&mut self) {
        let shown = self.core.settings().columns.clone();
        let mut list: Vec<(u8, bool, u16)> = shown.iter().map(|(k, w)| (*k, true, *w)).collect();
        list.extend(
            COLS.iter()
                .enumerate()
                .filter(|(k, _)| !shown.iter().any(|c| c.0 as usize == *k))
                .map(|(k, c)| (k as u8, false, c.2)),
        );
        let width = list[0].2.to_string();
        self.open(Dialog::Columns {
            list,
            sel: 0,
            width,
        });
    }
}
