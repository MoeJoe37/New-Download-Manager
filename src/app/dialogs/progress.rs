use crate::app::dialogs::Dialog;
use crate::app::{
    core, dlu, egui, explore, human_os, i18n, open_folder_of, open_with, pct2, size3, size3_os,
    time_left, tr, tr_os, Align2, App, Color32, Duration, Info, Sense, Settings, Status, Vec2, BAR,
    PROG_DX,
};

impl App {
    pub(super) fn dialog_progress(
        &mut self,
        ui: &mut egui::Ui,
        id: &mut u64,
        tab: &mut u8,
        details: &mut bool,
    ) -> bool {
        let Some(job) = self.core.find(*id) else {
            return false;
        };
        let speed = job.speed();
        let i = job.info().clone();
        let running = job.running();
        let done = i.done();
        let frac = i.size.filter(|s| *s > 0).map(|s| done as f64 / s as f64);
        // after the download, the processing (joining video and audio, moving the file...) fills the
        // same bar again; a step that can't tell how far it is runs to and fro
        let post = i.post.clone().filter(|_| running);
        let bar = match &post {
            Some((_, f)) => f.max(0.0),
            None => frac.unwrap_or(0.0) as f32,
        };
        // the step's time left, from how fast it has gone so far
        let post_left = post.as_ref().zip(i.post_since).and_then(|((_, f), t)| {
            let gone = t.elapsed().as_secs_f32();
            (*f > 0.02 && gone > 1.0).then(|| (gone * (1.0 - f) / f) as u64)
        });
        if post.is_some() {
            ui.ctx().request_repaint_after(Duration::from_millis(200));
        }
        let mut d = dlu::D::new(ui).with_dx(PROG_DX);
        // tabs the user hid (template 367 / "Hide tab", 0x4a46d0) are left out; `tab` indexes the shown ones
        let (sl_tab, c_tab, btns) = {
            let s = self.core.settings();
            (s.prog_sl_tab, s.prog_c_tab, s.prog_tab_btns)
        };
        let pages: Vec<(u8, &str)> = [
            (0, "Download status"),
            (1, "Speed Limiter"),
            (2, "Options on completion"),
        ]
        .into_iter()
        .filter(|(p, _)| *p == 0 || (*p == 1 && sl_tab) || (*p == 2 && c_tab))
        .collect();
        *tab = (*tab).min(pages.len() as u8 - 1);
        let names: Vec<&str> = pages.iter().map(|p| p.1).collect();
        let (px, py, _, _) = d.tabs(7.0, 5.0, 338.0, 101.0, tab, &names);
        let (ox, oy) = (px - 7.0, py - 3.0); // page templates are laid out from (7, 3)
        let page = pages[*tab as usize].0;
        // "Hide tab" (templates 364/365, 0x4a41a0 clears bHideSLTabOnDwnlDlg)
        if page > 0 && btns && d.button(ox + 252.0, oy + 64.0, 65.0, 14.0, "Hide tab", true) {
            let mut s = self.core.settings();
            if page == 1 {
                s.prog_sl_tab = false
            } else {
                s.prog_c_tab = false
            }
            drop(s);
            self.core.save();
            *tab = 0;
        }
        match page {
            0 => {
                let status = match &i.status {
                    _ if post.is_some() => {
                        let (label, f) = post.clone().unwrap();
                        if f >= 0.0 {
                            i18n::line(&format!("{} ({}%)", tr_os(&label), (f * 100.0) as u32))
                        } else {
                            tr(&label)
                        }
                    }
                    Status::Error(e) => {
                        i18n::line(&format!("{}: {}", tr_os("Error"), i18n::tr_msg_os(e)))
                    }
                    Status::Connecting => tr("Connecting..."),
                    Status::Downloading => tr("Receiving data..."),
                    Status::Complete => tr("Complete"),
                    Status::Stopped => tr("Stopped"),
                };
                let left = i
                    .size
                    .filter(|_| speed > 0.0)
                    .map(|s| time_left((s.saturating_sub(done) as f64 / speed) as u64));
                // NDM's formats: "%s ( %d%s%02d %% )" until 100 %, "%s/%s" and " (Limited)" for the rate
                let downloaded = match frac {
                    Some(f) if f < 1.0 => format!("{}  ( {} % )", size3(done), pct2(f * 100.0)),
                    _ => size3(done),
                };
                let limited = i.limit_kb > 0 || self.core.settings().limit_on;
                // processing: nothing is coming in, so no rate or time left
                let left = if post.is_some() {
                    post_left.map(time_left)
                } else {
                    left
                };
                let speed_text = if running && post.is_none() {
                    i18n::line(&format!(
                        "{}/{}{}",
                        size3_os(speed as u64),
                        tr_os("sec"),
                        if limited {
                            format!(" {}", tr_os("(Limited)"))
                        } else {
                            String::new()
                        }
                    ))
                } else {
                    String::new()
                };
                d.label(ox + 7.0, oy + 4.0, 329.0, 9.0, i.url.as_str());
                for (y, k, vx, v) in [
                    (15.0, "Status", 53.0, status),
                    (
                        28.0,
                        "File size",
                        75.0,
                        i.size.map_or(tr("(unknown)"), size3),
                    ),
                    (38.0, "Downloaded", 75.0, downloaded),
                    (48.0, "Transfer rate", 75.0, speed_text),
                    (
                        58.0,
                        "Time left",
                        75.0,
                        left.unwrap_or_else(|| {
                            if running && post.is_none() {
                                "(unknown)".into()
                            } else {
                                String::new()
                            }
                        }),
                    ),
                    (
                        68.0,
                        "Resume capability",
                        127.0,
                        if !i.probed {
                            "(unknown)".into()
                        } else if i.ranges {
                            "Yes".into()
                        } else {
                            "No".into()
                        },
                    ),
                ] {
                    d.label(ox + 7.0, oy + y, vx - 9.0, 9.0, k);
                    d.label(ox + vx, oy + y, 260.0, 9.0, v);
                }
                // spread over several networks: each one's speed now and its share of this run
                let nets = job.net_bytes.lock().unwrap().clone();
                if nets.keys().any(|n| !n.is_empty()) {
                    let total = nets.values().sum::<u64>().max(1);
                    let mut names: Vec<&String> = nets.keys().filter(|n| !n.is_empty()).collect();
                    names.sort();
                    let line = names
                        .iter()
                        .map(|n| {
                            let rate: f64 = if running {
                                i.segs
                                    .iter()
                                    .filter(|s| s.owned && s.net == **n)
                                    .map(|s| s.rate)
                                    .sum()
                            } else {
                                0.0
                            };
                            i18n::line(&format!(
                                "{n} {}/{} ({}%)",
                                size3_os(rate as u64),
                                tr_os("sec"),
                                nets[*n] * 100 / total
                            ))
                        })
                        .collect::<Vec<_>>()
                        .join("   ");
                    d.label(ox + 7.0, oy + 80.0, 66.0, 9.0, "Networks");
                    d.at(ox + 75.0, oy + 80.0, 260.0, 9.0, |ui| ui.label(line));
                }
            }
            1 => {
                d.label(ox + 7.0, oy + 7.0, 63.0, 9.0, "Transfer rate");
                d.label(
                    ox + 75.0,
                    oy + 7.0,
                    202.0,
                    9.0,
                    if running {
                        i18n::line(&format!("{}/{}", size3_os(speed as u64), tr_os("sec")))
                    } else {
                        String::new()
                    },
                );
                let mut on = i.limit_kb > 0;
                let mut kb = if on {
                    i.limit_kb.to_string()
                } else {
                    "100".into()
                };
                let toggled = d
                    .check(
                        ox + 7.0,
                        oy + 20.0,
                        154.0,
                        10.0,
                        &mut on,
                        "Use Speed Limiter",
                    )
                    .changed();
                d.label(ox + 7.0, oy + 32.0, 170.0, 9.0, "Maximum download speed:");
                let edited = d
                    .edit(ox + 7.0, oy + 43.0, 51.0, 14.0, &mut kb, on)
                    .changed();
                d.label(ox + 64.0, oy + 45.0, 65.0, 9.0, "KBytes/sec");
                if toggled || edited {
                    self.core.set_job_limit(
                        &job,
                        if on {
                            kb.trim().parse().unwrap_or(100).clamp(1, 1_000_000)
                        } else {
                            0
                        },
                    );
                }
            }
            _ => {
                d.label(
                    ox + 7.0,
                    oy + 3.0,
                    316.0,
                    9.0,
                    format!(
                        "{} {}",
                        tr_os("Save To:"),
                        i18n::ltr(&i.path().display().to_string())
                    ),
                );
                let mut show = self.core.settings().show_complete;
                if d.check(
                    ox + 7.0,
                    oy + 15.0,
                    169.0,
                    10.0,
                    &mut show,
                    "Show download complete dialog",
                )
                .changed()
                {
                    self.core.settings().show_complete = show;
                }
                d.hline(ox + 7.0, oy + 27.0, 316.0);
                d.label(ox + 7.0, oy + 30.0, 316.0, 9.0, "Power options");
                let flags = self.on_done.entry(*id).or_default();
                d.check(
                    ox + 7.0,
                    oy + 41.0,
                    225.0,
                    10.0,
                    &mut flags.0,
                    "Exit New Download Manager when done",
                );
                d.check(
                    ox + 7.0,
                    oy + 52.0,
                    225.0,
                    10.0,
                    &mut flags.1,
                    "Turn off computer when done",
                );
            }
        }
        d.progress(7.0, 111.0, 338.0, 10.0, bar);
        if d.button(
            15.0,
            126.0,
            93.0,
            15.0,
            if *details {
                "<< Hide details"
            } else {
                "Show details >>"
            },
            true,
        ) {
            *details = !*details;
            self.core.settings().prog_details = *details; // NDM saves dPrDtVis (0x4a3490)
            self.core.save();
        }
        let finished = i.status == Status::Complete;
        let failed = matches!(i.status, Status::Error(_));
        let pause_label = if running {
            "Pause"
        } else if failed {
            "Recover..."
        } else {
            "Resume"
        };
        if d.button(203.0, 126.0, 56.0, 15.0, pause_label, running || !finished) {
            if running {
                job.stop()
            } else if failed {
                self.open(Dialog::Recovery { id: *id })
            } else {
                self.core.start(&job)
            }
        }
        let cancel = d.button(277.0, 126.0, 50.0, 15.0, "Cancel", true);
        if cancel {
            job.stop();
        }
        if *details {
            let r = d.rect(21.0, 143.0, 306.0, 9.0);
            d.ui.put(
                r,
                egui::Label::new(tr("Start positions and download progress by connections")),
            );
            let r = d.rect(7.0, 153.0, 338.0, 10.0);
            let bars = self.res.bars.as_ref().map(|b| b.each_ref().map(|t| t.id()));
            d.ui.scope_builder(egui::UiBuilder::new().max_rect(r), |ui| {
                segment_bar(ui, &i, r.height(), bars)
            });
            // NDM's connections list (id 1177): N. / Downloaded / Info and an empty fourth column, a row
            // for every connection the download may open
            let cols = [
                ("N.", 18.5),
                ("Downloaded", 62.5),
                ("Info", 197.0),
                ("", 58.0),
            ];
            let rows = d.listview(7.0, 168.0, 338.0, 79.0, &cols);
            let dx = d.dx();
            let font = egui::TextStyle::Body.resolve(d.ui.style());
            let ink = d.ui.visuals().text_color();
            let n = i.conns.max(i.segs.len()).max(1);
            d.ui.scope_builder(egui::UiBuilder::new().max_rect(rows), |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink(false)
                    .show(ui, |ui| {
                        for k in 0..n {
                            let (row, _) = ui.allocate_exact_size(
                                Vec2::new(ui.available_width(), 16.0),
                                Sense::hover(),
                            );
                            let sg = i.segs.get(k);
                            let got = sg.map_or(0, |s| s.next - s.start);
                            let info = match sg {
                                Some(s) if s.done() => tr("Disconnect."),
                                Some(s) if s.net.is_empty() => tr(&s.info),
                                Some(s) => i18n::line(&format!(
                                    "{} ({})",
                                    i18n::tr_msg_os(&s.info),
                                    i18n::ltr(&s.net)
                                )),
                                None => String::new(),
                            };
                            let rtl = i18n::is_rtl();
                            let mut x = if rtl { row.right() } else { row.left() };
                            for (c, text) in [
                                format!("{:2}", k + 1),
                                if got > 0 { size3(got) } else { String::new() },
                                info,
                            ]
                            .into_iter()
                            .enumerate()
                            {
                                let w = cols[c].1 * dx;
                                let clip = egui::Rect::from_min_size(
                                    egui::pos2(if rtl { x - w + 4.0 } else { x }, row.top()),
                                    Vec2::new(w - 4.0, row.height()),
                                );
                                let (at, align) = if rtl {
                                    (x - 6.0, Align2::RIGHT_CENTER)
                                } else {
                                    (x + 6.0, Align2::LEFT_CENTER)
                                };
                                ui.painter().with_clip_rect(clip).text(
                                    egui::pos2(at, row.center().y),
                                    align,
                                    text,
                                    font.clone(),
                                    ink,
                                );
                                x += if rtl { -w } else { w };
                            }
                        }
                    });
            });
        }
        !cancel
    }

    pub(super) fn dialog_prog_custom(
        &mut self,
        ui: &mut egui::Ui,
        view: &mut u8,
        sl: &mut bool,
        c: &mut bool,
        btns: &mut bool,
    ) -> bool {
        // template 367, OK = 0x435710
        let mut d = dlu::D::new(ui);
        d.label(7.0, 6.0, 178.0, 10.0, "Start view:");
        let views = ["Show normally sized", "Show minimized", "Don't show"].map(tr);
        let mut v = views[*view as usize % 3].clone();
        if d.combo(
            7.0,
            18.0,
            90.0,
            14.0,
            "pview",
            &mut v,
            &views.each_ref().map(String::as_str),
            true,
        ) {
            *view = views.iter().position(|x| *x == v).unwrap_or(0) as u8;
        }
        d.text(7.0, 34.0, 254.0, 27.0, "Note: \"Start view\" option doesn't relate to queue processing. When NDM is processing queues,  \"Download progress\" dialog always starts minimized in system tray");
        d.hline(7.0, 66.0, 254.0);
        d.check(7.0, 74.0, 254.0, 10.0, sl, "Show \"Speed Limiter\" tab");
        d.check(
            7.0,
            88.0,
            189.0,
            10.0,
            c,
            "Show \"Options on completion\" tab",
        );
        d.check(7.0, 101.0, 254.0, 10.0, btns, "Show \"Hide tab\" buttons");
        let ok = d.button(77.0, 124.0, 50.0, 14.0, "OK", true);
        if ok {
            let v = (*view, *sl, *c, *btns);
            self.opt_patch.push(Box::new(move |s: &mut Settings| {
                (s.prog_view, s.prog_sl_tab, s.prog_c_tab, s.prog_tab_btns) = v
            }));
        }
        !(ok || d.button(139.0, 124.0, 50.0, 14.0, "Cancel", true))
    }

    pub(super) fn dialog_complete(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        id: &mut u64,
        dont_show: &mut bool,
    ) -> bool {
        let Some(job) = self.core.find(*id) else {
            return false;
        };
        let i = job.info().clone();
        let path = i.path();
        let icon = self.icon(ctx, &core::ext(&i.name));
        let mut d = dlu::D::new(ui);
        if let Some(t) = icon {
            let r = d.rect(7.0, 7.0, 21.0, 20.0);
            d.ui.put(r, egui::Image::new((t, Vec2::splat(32.0))));
        }
        d.text(
            34.0,
            7.0,
            262.0,
            32.0,
            format!(
                "{}\n{}",
                tr_os("Download complete"),
                i18n::trf_os(
                    "Downloaded %s (%I64d Bytes)",
                    &[&human_os(i.done()), &i.done()]
                )
            ),
        );
        d.label(7.0, 38.0, 289.0, 9.0, "Address");
        d.readonly(7.0, 48.0, 289.0, 14.0, &i.url);
        d.label(7.0, 66.0, 289.0, 9.0, "The file saved as");
        d.readonly(7.0, 76.0, 289.0, 14.0, &path.display().to_string());
        let mut close = false;
        if d.button(7.0, 96.0, 61.0, 14.0, "Open", true) {
            explore(&path);
            close = true;
        }
        if d.button(76.0, 96.0, 61.0, 14.0, "Open with...", true) {
            open_with(&path);
            close = true;
        }
        if d.button(145.0, 96.0, 61.0, 14.0, "Open folder", true) {
            open_folder_of(&path);
            close = true;
        }
        close |= d.button(235.0, 96.0, 61.0, 14.0, "Close", true);
        d.check(
            16.0,
            117.0,
            238.0,
            10.0,
            dont_show,
            "Don't show this dialog again",
        );
        if close && *dont_show {
            self.core.settings().show_complete = false;
        }
        !close
    }
}

/// "Start positions and download progress by connections" bar: one filled span per segment.
/// "Start positions and download progress by connections" (static 1179). With NDM's bar bitmaps (358 ends,
/// 359 background, 361 downloaded, 362 start marker: 15 px columns stretched across) it is drawn
/// draws it: the background, each connection's downloaded run in blue and a red mark where it started.
fn segment_bar(ui: &mut egui::Ui, info: &Info, h: f32, bars: Option<[egui::TextureId; 4]>) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), h), Sense::hover());
    let c = dlu::pal(ui);
    let p = ui.painter();
    if let Some([ends, bg, blue, red]) = bars {
        let ppp = ui.ctx().pixels_per_point();
        let snap = |v: f32| (v * ppp).round() / ppp;
        let px = 1.0 / ppp;
        let top = snap(rect.center().y - 7.5 / ppp);
        let bar = egui::Rect::from_min_max(
            egui::pos2(snap(rect.left()), top),
            egui::pos2(snap(rect.right()), top + 15.0 / ppp),
        );
        let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        let col = |t: egui::TextureId, a: f32, b: f32| {
            p.image(
                t,
                egui::Rect::from_x_y_ranges(snap(a)..=snap(b).max(snap(a) + px), bar.y_range()),
                uv,
                Color32::WHITE,
            )
        };
        col(bg, bar.left(), bar.right());
        col(ends, bar.left(), bar.left() + px);
        col(ends, bar.right() - px, bar.right());
        let Some(size) = info.size.filter(|s| *s > 0) else {
            return;
        };
        let inner = bar.shrink2(Vec2::new(px, 0.0));
        // the file's start at the right in right-to-left languages, as the progress bar fills
        let rtl = i18n::is_rtl();
        let x = |b: u64| {
            if rtl {
                inner.right() - inner.width() * (b as f32 / size as f32)
            } else {
                inner.left() + inner.width() * (b as f32 / size as f32)
            }
        };
        let span = |t: egui::TextureId, a: f32, b: f32| col(t, a.min(b), a.max(b));
        let dir = if rtl { -px } else { px };
        for s in &info.segs {
            let done = if info.status == Status::Complete {
                s.stop.unwrap_or(s.next)
            } else {
                s.next
            };
            if done > s.start {
                span(blue, x(s.start) + dir, x(done));
            }
            span(red, x(s.start), x(s.start) + dir);
        }
        return;
    }
    p.rect_filled(rect, 0.0, c.field);
    p.rect_stroke(
        rect,
        0.0,
        egui::Stroke::new(1.0_f32, c.edge),
        egui::StrokeKind::Inside,
    );
    let Some(size) = info.size.filter(|s| *s > 0) else {
        return;
    };
    let inner = rect.shrink(1.0);
    let rtl = i18n::is_rtl();
    let x = |b: u64| {
        if rtl {
            inner.right() - inner.width() * (b as f32 / size as f32)
        } else {
            inner.left() + inner.width() * (b as f32 / size as f32)
        }
    };
    for s in &info.segs {
        let done = if info.status == Status::Complete {
            s.stop.unwrap_or(s.next)
        } else {
            s.next
        };
        let (a, b) = (x(s.start), x(done));
        p.rect_filled(
            egui::Rect::from_x_y_ranges(a.min(b)..=a.max(b), inner.y_range()),
            0.0,
            BAR,
        );
    }
}
