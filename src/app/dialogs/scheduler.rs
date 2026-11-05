use crate::app::dialogs::Dialog;
use crate::app::{
    core, dlu, egui, help, human, i18n, tr, tr_os, with_login, App, Color32, Rect, RichText,
    Status, Vec2, PAGE_DX,
};

impl App {
    pub(super) fn dialog_scheduler(
        &mut self,
        ui: &mut egui::Ui,
        tab: &mut u8,
        queue: &mut core::queue::Queue,
        qsel: &mut Option<u64>,
        error: &mut String,
    ) -> bool {
        // CNewSchedulerDlg (template 308): queue tree left, tab pages right, buttons along the bottom.
        let queue_icon = self.res.icon(331);
        let running = self.core.queue_running(queue.id);
        let queues: Vec<(u32, String, bool, bool)> = self
            .core
            .settings()
            .queues
            .iter()
            .map(|q| (q.id, tr(&q.name), self.core.queue_running(q.id), q.sync))
            .collect();
        if queue.id != LIMITS && !queues.iter().any(|q| q.0 == queue.id) {
            *queue = self.core.settings().queues[0].clone(); // deleted from elsewhere
        }
        let mut pick = None;
        let mut d = dlu::D::new(ui);
        if let Some(t) = queue_icon {
            let r = d.rect(7.0, 4.0, 20.0, 20.0);
            d.ui.put(
                Rect::from_min_size(r.min, Vec2::splat(32.0)),
                egui::Image::new((t, Vec2::splat(32.0))),
            );
        }
        let r = d.rect(32.0, 15.0, 61.0, 9.0);
        d.ui.put(r, egui::Label::new(tr("Queues")));
        let tree = d.listview(7.0, 27.0, 115.0, 233.0, &[]);
        d.ui.scope_builder(egui::UiBuilder::new().max_rect(tree.shrink(3.0)), |ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            // NDM's queue pictures: yellow folders, green for synchronization, a clock while running;
            // last the "Download limits" node (red cross)
            let limits = (LIMITS, tr("Download limits"), false, false);
            for (id, name, on, sync) in queues.iter().chain([&limits]) {
                ui.horizontal(|ui| {
                    let pic = if *id == LIMITS {
                        329
                    } else {
                        [[311, 312], [315, 316]][*sync as usize][*on as usize]
                    };
                    if let Some(t) = self.res.tree(pic) {
                        ui.add(egui::Image::new((t, Vec2::new(16.0, 13.0))));
                    }
                    if ui
                        .selectable_label(*id == queue.id, name.as_str())
                        .clicked()
                        && *id != queue.id
                    {
                        pick = Some(*id);
                    }
                });
            }
        });
        d.vline(132.0, 7.0, 272.0);
        let is_limits = queue.id == LIMITS;
        if is_limits {
            // the scheduler's "Download limits" page: Options -> Connection's quota, applied at once
            let mut s = self.core.settings();
            // positions measured on NDM; the texts grey out with the box unticked
            d.check(160.0, 20.5, 80.0, 10.0, &mut s.quota_on, "Download limits");
            let on = s.quota_on;
            let grey = |t: &str| {
                RichText::new(tr(t)).color(if on {
                    Color32::PLACEHOLDER
                } else {
                    Color32::GRAY
                })
            };
            d.label_r(150.0, 41.0, 105.0, 10.0, grey("Download no more than"));
            d.at(260.0, 41.0, 23.0, 10.0, |ui| {
                ui.add_enabled(on, egui::DragValue::new(&mut s.quota_mb).range(1..=999_999))
            });
            d.label(288.0, 41.0, 40.0, 10.0, grey("MBytes"));
            d.label_r(200.0, 60.0, 55.0, 10.0, grey("every"));
            d.at(260.0, 60.0, 23.0, 10.0, |ui| {
                ui.add_enabled(on, egui::DragValue::new(&mut s.quota_hours).range(1..=23))
            });
            d.label(288.0, 60.0, 40.0, 10.0, grey("hours"));
            d.at(160.0, 78.5, 160.0, 10.0, |ui| {
                ui.add_enabled(
                    on,
                    dlu::checkbox(&mut s.quota_warn, "Show warning before stopping downloads"),
                )
            });
        }
        let builtin = self
            .core
            .settings()
            .queue(queue.id)
            .is_none_or(core::queue::Queue::builtin);
        // shown like NDM's time pickers ("11:00:00 PM"); stored as HH:MM
        for t in [&mut queue.sched.start_at, &mut queue.sched.stop_at] {
            if let Ok(v) = chrono::NaiveTime::parse_from_str(t, "%H:%M") {
                *t = v.format("%-I:%M:%S %p").to_string();
            }
        }
        let sync = &mut queue.sync;
        let (sched, max_active, limit_kb) =
            (&mut queue.sched, &mut queue.max_active, &mut queue.limit_kb);
        if !is_limits {
            // the queue's name centred over the tabs; pages are laid out 2.0 px/DLU across (1.6 at 100%)
            let r = d.rect(142.0, 3.0, 327.0, 9.0);
            d.ui.put(
                r,
                egui::Label::new(i18n::line(&format!(
                    "{}{}",
                    tr_os(&queue.name),
                    if running { " \u{25b6}" } else { "" }
                ))),
            );
            d.tabs(
                142.0,
                15.5,
                327.0,
                246.0,
                tab,
                &["Schedule", "Files in the queue"],
            );
            let mut d2 = dlu::D::new_offset(d.ui, 145.6, 27.0).with_dx(PAGE_DX);
            if *tab == 0 {
                // template 306; the built-in queues keep their type
                let periodic = *sync;
                for (x, w, v, t) in [
                    (7.0, 126.0, false, "One-time downloading"),
                    (141.0, 132.0, true, "Periodic synchronization"),
                ] {
                    if builtin && periodic != v {
                        d2.radio_off(x, 3.0, w, 10.0, t);
                    } else if d2.radio(x, 3.0, w, 10.0, periodic == v, t) {
                        *sync = v;
                    }
                }
                d2.hline(7.0, 16.0, 266.0);
                d2.check(
                    7.0,
                    20.0,
                    249.0,
                    10.0,
                    &mut sched.on_startup,
                    "Start download on New Download Manager startup",
                );
                d2.hline(7.0, 33.0, 266.0);
                d2.check(
                    7.0,
                    42.0,
                    97.0,
                    10.0,
                    &mut sched.start_on,
                    "Start download at",
                );
                d2.edit(110.0, 39.0, 59.0, 15.0, &mut sched.start_at, sched.start_on);
                let on = sched.start_on;
                if periodic {
                    // synchronization runs daily, starting again every N hours after a pass
                    sched.daily = true;
                    d2.at(7.0, 60.0, 90.0, 10.0, |ui| {
                        ui.add_enabled(on, dlu::checkbox(&mut sched.again_on, "Start again every"))
                    });
                    let en = on && sched.again_on;
                    d2.at(100.0, 59.0, 19.0, 14.0, |ui| {
                        ui.add_enabled(en, egui::DragValue::new(&mut sched.again_h).range(0..=999))
                    });
                    d2.label(122.0, 62.0, 25.0, 8.0, "hours");
                    d2.at(152.0, 59.0, 19.0, 14.0, |ui| {
                        ui.add_enabled(en, egui::DragValue::new(&mut sched.again_m).range(0..=59))
                    });
                    d2.label(173.0, 62.0, 23.0, 8.0, "min");
                    d2.label(
                        15.0,
                        87.0,
                        39.0,
                        9.0,
                        RichText::new(tr("Daily")).color(Color32::GRAY),
                    );
                } else {
                    if d2.radio(12.0, 60.0, 54.0, 10.0, on && !sched.daily, "Once at") && on {
                        sched.daily = false;
                    }
                    if d2.radio(12.0, 78.0, 45.0, 10.0, on && sched.daily, "Daily") && on {
                        sched.daily = true;
                    }
                }
                let days = [
                    "Sunday",
                    "Monday",
                    "Tuesday",
                    "Wednesday",
                    "Thursday",
                    "Friday",
                    "Saturday",
                ];
                let pos = [
                    (62.0, 78.0),
                    (62.0, 88.0),
                    (62.0, 98.0),
                    (135.0, 78.0),
                    (135.0, 88.0),
                    (135.0, 98.0),
                    (209.0, 78.0),
                ];
                for (k, (x, y)) in pos.iter().enumerate() {
                    let en = on && sched.daily;
                    d2.at(*x, *y, 70.0, 10.0, |ui| {
                        ui.add_enabled(en, dlu::checkbox(&mut sched.days[k], days[k]))
                    });
                }
                d2.hline(8.0, 112.0, 265.0);
                d2.check(
                    8.0,
                    121.0,
                    99.0,
                    10.0,
                    &mut sched.stop_on,
                    "Stop download at",
                );
                d2.edit(111.0, 119.0, 59.0, 14.0, &mut sched.stop_at, sched.stop_on);
                d2.check(
                    8.0,
                    136.0,
                    204.0,
                    10.0,
                    &mut sched.retry_on,
                    "Number of retries for each file if downloading failed :",
                );
                d2.drag(216.0, 132.0, 32.0, 14.0, &mut sched.retries, 1, 999);
                d2.hline(8.0, 151.0, 265.0);
                d2.check(
                    7.0,
                    156.0,
                    235.0,
                    10.0,
                    &mut sched.open_on,
                    "Open the following file when done:",
                );
                d2.edit(7.0, 168.0, 237.0, 14.0, &mut sched.open_file, sched.open_on);
                if d2.button(249.0, 168.0, 15.0, 14.0, "...", sched.open_on) {
                    if let Some(p) = rfd::FileDialog::new().pick_file() {
                        sched.open_file = p.display().to_string();
                    }
                }
                d2.check(
                    7.0,
                    186.0,
                    203.0,
                    10.0,
                    &mut sched.hangup,
                    "Hang up modem when done",
                );
                d2.check(
                    7.0,
                    197.0,
                    207.0,
                    10.0,
                    &mut sched.exit_done,
                    "Exit New Download Manager when done",
                );
                d2.check(
                    7.0,
                    208.0,
                    182.0,
                    10.0,
                    &mut sched.off_done,
                    "Turn off computer when done",
                );
                let kinds = ["Shut down", "Sleep", "Hibernate"];
                let mut kind = kinds[sched.off_kind.min(2) as usize].to_string();
                if d2.combo(
                    194.0,
                    208.0,
                    79.0,
                    14.0,
                    "offkind",
                    &mut kind,
                    &kinds,
                    sched.off_done,
                ) {
                    sched.off_kind = kinds.iter().position(|k| *k == kind).unwrap_or(0) as u8;
                }
                d2.at(19.0, 219.0, 174.0, 10.0, |ui| {
                    ui.add_enabled(
                        sched.off_done,
                        dlu::checkbox(&mut sched.off_force, "Force processes to terminate"),
                    )
                });
                if !error.is_empty() {
                    d2.label(
                        7.0,
                        220.0,
                        266.0,
                        9.0,
                        RichText::new(error.as_str()).color(Color32::RED),
                    );
                }
            } else {
                // template 307
                d2.label(7.0, 8.0, 41.0, 10.0, "Download");
                d2.drag(53.0, 5.0, 26.0, 14.0, max_active, 1, 20);
                d2.label(83.0, 8.0, 90.0, 10.0, "files at the same time");
                // each of the queue's downloads at most this fast while it runs
                let mut on = *limit_kb > 0;
                if d2
                    .check(176.0, 7.0, 40.0, 10.0, &mut on, "Limit to")
                    .changed()
                {
                    *limit_kb = if on { 200 } else { 0 };
                }
                if on {
                    d2.drag(216.0, 5.0, 28.0, 14.0, limit_kb, 1, 1_000_000);
                }
                d2.label(246.0, 8.0, 29.0, 10.0, "KBytes/sec");
                let queued: Vec<(u64, String, String, String)> = self
                    .core
                    .jobs()
                    .iter()
                    .map(|j| j.info())
                    .filter(|i| i.queued && i.queue == queue.id)
                    .map(|i| {
                        let st = match &i.status {
                            Status::Complete => tr("Complete"),
                            Status::Error(_) => tr("Error"),
                            _ => String::new(),
                        };
                        (
                            i.id,
                            if i.name.is_empty() {
                                i.url.clone()
                            } else {
                                i.name.clone()
                            },
                            i.size.map(human).unwrap_or_default(),
                            st,
                        )
                    })
                    .collect();
                let list = d2.listview(
                    8.0,
                    23.0,
                    264.0,
                    181.0,
                    &[("File Name", 140.0), ("Size", 60.0), ("Status", 62.0)],
                );
                d2.rows(list, |ui| {
                    egui::ScrollArea::vertical()
                        .auto_shrink(false)
                        .show(ui, |ui| {
                            for (id, name, size, st) in &queued {
                                ui.horizontal(|ui| {
                                    if ui
                                        .add_sized(
                                            [140.0 * PAGE_DX - 6.0, 17.0],
                                            egui::SelectableLabel::new(
                                                *qsel == Some(*id),
                                                name.as_str(),
                                            ),
                                        )
                                        .clicked()
                                    {
                                        *qsel = Some(*id);
                                    }
                                    ui.add_sized(
                                        [60.0 * PAGE_DX - 6.0, 17.0],
                                        egui::Label::new(size.as_str()),
                                    );
                                    ui.label(st.as_str());
                                });
                            }
                        });
                });
                let has = qsel.is_some_and(|q| queued.iter().any(|x| x.0 == q));
                for (k, (glyph, tip, dir)) in [
                    ("⏶", "Move up", -1),
                    ("⏷", "Move down", 1),
                    ("✖", "Delete from queue", 0),
                ]
                .into_iter()
                .enumerate()
                {
                    let r = d2.rect([9.0, 29.0, 47.0][k], 210.0, 15.0, 14.0);
                    if d2
                        .ui
                        .put(r, |ui: &mut egui::Ui| {
                            ui.add_enabled(has, egui::Button::new(glyph).min_size(r.size()))
                        })
                        .on_hover_text(tip)
                        .clicked()
                    {
                        if dir == 0 {
                            self.core.set_queue(&[qsel.unwrap()], None)
                        } else {
                            self.move_in_queue(qsel.unwrap(), dir)
                        }
                    }
                }
            }
        }
        let new = d.button(14.3, 266.5, 48.0, 12.5, "New queue", true);
        if d.button(67.7, 266.5, 48.5, 12.5, "Delete", !builtin) {
            self.core.delete_queue(queue.id);
            pick = Some(core::queue::MAIN_QUEUE);
        }
        if d.button(
            144.0,
            266.5,
            55.0,
            12.5,
            "Start now",
            !running && !is_limits,
        ) {
            self.core.start_queue(queue.id);
        }
        if d.button(199.0, 266.5, 48.0, 12.5, "Stop", running) {
            self.core.stop_queue(queue.id);
        }
        if d.button(261.0, 266.5, 48.0, 12.5, "Help", help("index.htm", false)) {
            help("scheduler.htm", true);
        }
        if is_limits {
            d.button(324.5, 266.5, 48.0, 12.5, "Apply", false);
        } else if d.button(324.5, 266.5, 48.0, 12.5, "Apply", true) {
            let t = |s: &str| sched_time(s).map(|t| t.format("%H:%M").to_string());
            match (t(&queue.sched.start_at), t(&queue.sched.stop_at)) {
                (Some(a), Some(b)) => {
                    (queue.sched.start_at, queue.sched.stop_at) = (a, b);
                    if let Some(q) = self
                        .core
                        .settings()
                        .queues
                        .iter_mut()
                        .find(|q| q.id == queue.id)
                    {
                        *q = queue.clone();
                    }
                    self.core.save();
                    error.clear();
                    // 0x4ffe20: once ever, the first time a queue gets a start time
                    if queue.sched.start_on && !self.core.settings().tray_note_shown {
                        self.core.settings().tray_note_shown = true;
                        self.open(Dialog::Info { title: "NDM Queues".into(), text: "You have scheduled the start time of queue processing.\r\nPlease note that in order to start the queue at the specified time, it's necessary to have NDM running at that time at least in the taskbar notification area - tray (see the picture below)".into() });
                    }
                }
                _ => *error = "Times must be like 11:00:00 PM or 23:00".into(),
            }
        }
        if let Some(id) = pick {
            // like switching queues in NDM's tree: unapplied edits of the old one are dropped
            *queue = if id == LIMITS {
                core::queue::Queue {
                    id,
                    name: "Download limits".into(),
                    ..Default::default()
                }
            } else {
                self.core.settings().queue(id).cloned().unwrap_or_default()
            };
            (*qsel, *error) = (None, String::new());
        }
        if new {
            self.new_queue(Vec::new());
        }
        !d.button(378.0, 266.5, 48.5, 12.5, "Close", true)
    }

    pub(super) fn dialog_quota_hit(ui: &mut egui::Ui, text: &str) -> bool {
        let mut d = dlu::D::new(ui);
        d.text(7.0, 7.0, 325.0, 60.0, text);
        !d.button(144.0, 72.0, 50.0, 14.0, "OK", true)
    }

    pub(super) fn dialog_queue_name(
        &mut self,
        ui: &mut egui::Ui,
        name: &mut String,
        files: &[u64],
    ) -> bool {
        let mut d = dlu::D::new(ui);
        d.text(
            7.0,
            6.0,
            166.0,
            18.0,
            "Enter a name for the new queue that will be displayed in the list of queues",
        );
        d.edit(7.0, 28.0, 172.0, 14.0, name, true);
        let ok = d.button(36.0, 53.0, 50.0, 14.0, "OK", !name.trim().is_empty());
        if ok {
            let q = self.core.add_queue(name);
            self.core.set_queue(files, Some(q));
            self.core.save();
        }
        !(ok || d.button(98.0, 53.0, 50.0, 14.0, "Cancel", true))
    }

    pub(super) fn dialog_queue_pick(
        &mut self,
        ui: &mut egui::Ui,
        ids: &[u64],
        q: &mut String,
        start: &mut bool,
        never: &mut bool,
        batch: &mut bool,
    ) -> bool {
        // template 345; "+" = new queue (310) for these files
        let mut d = dlu::D::new(ui);
        d.label(7.0, 7.0, 143.0, 9.0, "Add files to the queue:");
        let names: Vec<String> = self
            .core
            .settings()
            .queues
            .iter()
            .map(|x| x.name.clone())
            .collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        d.combo(7.0, 18.0, 126.0, 14.0, "qpick", q, &refs, true);
        let plus = d.button(138.0, 17.0, 16.0, 14.0, "+", true);
        if plus {
            self.open(Dialog::QueueName {
                name: String::new(),
                files: ids.to_owned(),
            });
        }
        d.check(7.0, 34.0, 147.0, 10.0, start, "Start queue processing");
        d.check(7.0, 74.0, 147.0, 10.0, never, "Don't ask me again");
        let ok = d.button(55.0, 53.0, 50.0, 14.0, "OK", true);
        if ok {
            let qid = self
                .core
                .settings()
                .queues
                .iter()
                .find(|x| x.name == *q)
                .map_or(core::queue::MAIN_QUEUE, |x| x.id);
            self.core.set_queue(ids, Some(qid));
            if *never {
                let mut s = self.core.settings();
                if *batch {
                    s.ask_queue_batch = false
                } else {
                    s.ask_queue_later = false
                }
            }
            if *start {
                self.core.start_queue(qid);
            }
            self.core.save();
        }
        !(ok || plus)
    }

    pub(super) fn dialog_batch(
        &mut self,
        ui: &mut egui::Ui,
        url: &mut String,
        from: &mut u32,
        to: &mut u32,
        width: &mut u32,
        letters: &mut bool,
        lf: &mut String,
        lt: &mut String,
        auth: &mut bool,
        login: &mut String,
        pass: &mut String,
    ) -> bool {
        let mut d = dlu::D::new(ui);
        d.text(7.0, 4.0, 302.0, 37.0, "It's possible to add a group of sequential file names like img001.jpg, img002.jpg, etc., img100.jpg to New Download Manager download queue. Use the asterisk wildcard for the file name pattern. \nFor example:   http://www.example.com/pictures/img*.jpg");
        d.label(7.0, 47.0, 30.0, 10.0, "Address");
        d.edit(39.0, 44.0, 270.0, 14.0, url, true);
        d.group(7.0, 64.0, 302.0, 55.0, "Replace asterisk to");
        if d.radio(15.0, 79.0, 66.0, 10.0, !*letters, "Numbers") {
            *letters = false;
        }
        if d.radio(92.0, 79.0, 65.0, 10.0, *letters, "Letters") {
            *letters = true;
        }
        d.label(11.0, 100.0, 33.0, 10.0, "From:");
        d.label(107.0, 100.0, 22.0, 10.0, "To:");
        d.label_r(203.0, 100.0, 61.0, 10.0, "Wildcard size");
        if *letters {
            // one letter each, a..z or A..Z; no wildcard size
            d.edit(53.0, 95.0, 38.0, 16.0, lf, true);
            d.edit(136.0, 95.0, 38.0, 16.0, lt, true);
            d.edit(268.0, 95.0, 34.0, 16.0, &mut "1".to_string(), false);
        } else {
            d.drag(53.0, 95.0, 38.0, 16.0, from, 0, 999_999);
            d.drag(136.0, 95.0, 38.0, 16.0, to, 0, 999_999);
            d.drag(268.0, 95.0, 34.0, 16.0, width, 1, 9);
        }
        d.group(7.0, 126.0, 302.0, 29.0, "");
        d.check(13.0, 124.0, 70.0, 12.0, auth, "Use authorization");
        d.label_r(13.0, 139.0, 60.0, 10.0, "Login");
        d.edit(77.0, 136.0, 78.0, 14.0, login, *auth);
        d.label_r(160.0, 139.0, 64.0, 10.0, "Password");
        d.password(228.0, 136.0, 75.0, 14.0, pass, *auth);
        let base = if *auth {
            with_login(url, login, pass)
        } else {
            url.clone()
        };
        let range = batch_range(*letters, *from, *to, *width, lf, lt);
        let ok = url.contains('*') && !range.is_empty() && range.len() <= 10_000;
        let gen = |k: usize| base.replacen('*', &range[k], 1);
        let show = |k: usize| {
            if ok && k < range.len() {
                gen(k)
            } else {
                String::new()
            }
        };
        d.label(7.0, 166.0, 58.0, 10.0, "First file:");
        d.readonly(68.0, 165.0, 241.0, 11.0, &show(0));
        d.label(7.0, 179.0, 54.0, 10.0, "Second file:");
        d.readonly(68.0, 178.0, 241.0, 11.0, &show(1));
        d.label(15.0, 189.0, 8.0, 8.0, "...");
        d.label(7.0, 201.0, 54.0, 10.0, "Last file:");
        d.readonly(
            68.0,
            199.0,
            241.0,
            11.0,
            &show(range.len().saturating_sub(1)),
        );
        let add = d.button(182.0, 217.0, 50.0, 14.0, "OK", ok);
        if add {
            let ids = self.add_queued((0..range.len()).map(gen).collect());
            self.pick_queue(ids, true);
        }
        !(add || d.button(247.0, 217.0, 50.0, 14.0, "Cancel", true))
    }
}

/// A Scheduler time as typed: "11:00:00 PM", "11:00 PM", "23:00:00" or "23:00".
pub(in crate::app) fn sched_time(s: &str) -> Option<chrono::NaiveTime> {
    let s = s.trim().to_uppercase();
    ["%I:%M:%S %p", "%I:%M %p", "%H:%M:%S", "%H:%M"]
        .iter()
        .find_map(|f| chrono::NaiveTime::parse_from_str(&s, f).ok())
}

/// "Download limits" node in the Scheduler's queue tree.
const LIMITS: u32 = u32::MAX;

/// Batch download's replacements for `*`: numbers `from..=to` padded to `width`, or letters `lf..=lt`
/// (one letter each, same case). Empty when the range is invalid.
pub(in crate::app) fn batch_range(
    letters: bool,
    from: u32,
    to: u32,
    width: u32,
    lf: &str,
    lt: &str,
) -> Vec<String> {
    let one = |s: &str| {
        let mut c = s.trim().chars();
        c.next()
            .filter(|x| x.is_ascii_alphabetic() && c.next().is_none())
    };
    match (letters, one(lf), one(lt)) {
        (false, ..) => (from..=to)
            .map(|n| format!("{n:0w$}", w = width as usize))
            .collect(),
        (true, Some(a), Some(b)) if a.is_ascii_lowercase() == b.is_ascii_lowercase() => {
            (a..=b).map(String::from).collect()
        }
        _ => vec![],
    }
}

#[test]
fn batch_ranges() {
    assert_eq!(batch_range(false, 8, 10, 3, "", ""), ["008", "009", "010"]);
    assert_eq!(batch_range(true, 0, 0, 1, "x", "z"), ["x", "y", "z"]);
    assert!(batch_range(true, 0, 0, 1, "a", "Z").is_empty());
    assert!(batch_range(false, 5, 1, 1, "", "").is_empty());
}
