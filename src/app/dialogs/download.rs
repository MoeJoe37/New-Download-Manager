use crate::app::{Align2, App, Arc, BAR, Color32, DUPL, Duration, Incoming, Info, Key, PROG_DX, Path, PathBuf, Rect, Relaxed, RichText, Sense, Settings, Status, Vec2, core, date, discard, dlu, egui, engine, explore, human, human_os, i18n, join_login, open_folder_of, open_with, pct2, size3, size3_os, time_left, tr, tr_os, win, with_login, ytdlp};
use crate::app::dialogs::{ChecksumSource, Dialog, ProbeSlot};

impl App {
    pub(super) fn dialog_add_url(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        url: &mut String,
        auth: &mut bool,
        login: &mut String,
        pass: &mut String,
    ) -> bool {
        let mut d = dlu::D::new(ui);
        d.label(8.0, 9.0, 35.0, 9.0, "Address");
        d.edit(45.0, 7.0, 264.0, 14.0, url, true).request_focus();
        d.group(7.0, 24.0, 302.0, 29.0, "");
        d.check(13.0, 22.0, 80.0, 12.0, auth, "Use authorization");
        d.label_r(13.0, 36.0, 60.0, 9.0, "Login");
        d.edit(77.0, 34.0, 78.0, 14.0, login, *auth);
        d.label_r(160.0, 36.0, 64.0, 9.0, "Password");
        let r = d.rect(228.0, 34.0, 75.0, 14.0);
        d.ui.put(r, |ui: &mut egui::Ui| ui.add_enabled(*auth, egui::TextEdit::singleline(pass).password(true).desired_width(r.width()).margin(egui::Margin::symmetric(3, 2))));
        let enter = d.ui.input(|i| i.key_pressed(Key::Enter));
        let ok = d.button(316.0, 7.0, 50.0, 13.0, "OK", !url.trim().is_empty()) || (enter && !url.trim().is_empty());
        let cancel = d.button(316.0, 24.0, 50.0, 14.0, "Cancel", true);
        if ok {
            let u = if *auth { with_login(url.trim(), login, pass) } else { url.trim().to_string() };
            self.incoming(ctx, Incoming { url: u, force: true, ..Default::default() });
        }
        !(ok || cancel)
    }

    pub(super) fn dialog_file_info(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        info: &mut Info,
        path: &mut String,
        probe: &mut ProbeSlot,
        applied: &mut bool,
        error: &mut String,
        hint: &str,
        remember: &mut bool,
        replace: &mut Option<u64>,
        auto: &mut bool,
        early: &mut Option<Arc<engine::Job>>,
        dir: &str,
    ) -> bool {
        if let Some(j) = early.as_ref().filter(|_| !*applied) {
            // the running download answers what the probe would
            let i = j.info();
            let res = match &i.status {
                Status::Error(e) => Some(Err(e.clone())),
                _ if i.probed => Some(Ok(engine::probe::Probe { name: i.name.clone(), size: i.size, ranges: i.ranges, modified: i.modified.clone(), etag: i.etag.clone(), stream: i.stream, page: i.page, ytdlp: i.ytdlp })),
                _ => None,
            };
            drop(i);
            if let Some(r) = res {
                *probe.lock().unwrap() = Some(r);
            }
        }
        if !*applied {
            if let Some(res) = probe.lock().unwrap().take() {
                *applied = true;
                match res {
                    Ok(p) => {
                        let name = if hint.is_empty() { p.name.clone() } else { hint.to_owned() };
                        info.category = self.core.settings().category_for(&name, &info.url);
                        info.folder = if dir.is_empty() { self.core.settings().folder_for(&info.category) } else { dir.to_owned() };
                        if replace.is_none() {
                            let name = ytdlp::formats::named(&name, &info.yt.quality);
                            let name = self.core.unique_name(&info.folder, &name);
                            *path = Path::new(&info.folder).join(name).display().to_string();
                        }
                        (info.size, info.ranges, info.probed, info.stream) = (p.size, p.ranges, true, p.stream);
                        info.page = p.page;
                        info.modified = p.modified;
                        info.etag = p.etag;
                        info.ytdlp |= p.ytdlp;
                    }
                    Err(e) => {
                        // yt-dlp's own message says what is missing or wrong (Set yt-dlp, a sign-in...)
                        if info.ytdlp || ytdlp::is_video_page(&info.url) {
                            self.open(Dialog::Info { title: "Download File Info".into(), text: i18n::tr_msg_os(&e) });
                        }
                        *error = e;
                    }
                }
            }
        }
        let icon = self.icon(ctx, &core::ext(path));
        let mut d = dlu::D::new(ui);
        d.label_r(5.0, 5.0, 54.0, 10.0, "URL");
        d.readonly(64.0, 3.0, 244.0, 14.0, &info.url);
        d.label_r(5.0, 22.0, 54.0, 10.0, "Category");
        let cat_names = self.core.settings().category_names();
        let cats: Vec<&str> = cat_names.iter().map(String::as_str).collect();
        let changed = d.combo(64.0, 22.0, 116.0, 14.0, "cat", &mut info.category, &cats, true);
        if d.button(183.0, 21.0, 16.0, 14.0, "+", true) {
            self.edit_category(None);
        }
        if changed {
            let name = Path::new(path.as_str()).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            *path = Path::new(&self.core.settings().folder_for(&info.category)).join(name).display().to_string();
        }
        let (pv, vq) = file_info_rows(info, path);
        if pv > 0.0 {
            // the name as it reads; the edit box below shows it in the order it is typed
            let name = Path::new(path.as_str()).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            d.label(66.0, 39.0, 240.0, 10.0, file_info_name(&name));
        }
        d.label_r(5.0, 41.0 + pv, 54.0, 10.0, "Save As");
        d.edit(64.0, 39.0 + pv, 223.0, 14.0, path, true);
        if d.button(292.0, 39.0 + pv, 16.0, 14.0, "...", true) {
            let p = Path::new(path.as_str());
            let mut fd = rfd::FileDialog::new();
            if let Some(dir) = p.parent().filter(|d| d.exists()) {
                fd = fd.set_directory(dir);
            }
            if let Some(n) = p.file_name() {
                fd = fd.set_file_name(n.to_string_lossy());
            }
            if let Some(chosen) = fd.save_file() {
                *path = chosen.display().to_string();
            }
        }
        d.group(64.0, 56.0 + pv, 244.0, 32.0, "");
        d.check(69.0, 53.0 + pv, 200.0, 13.0, remember, &i18n::trf_os("Remember this path for \"%s\" category", &[&tr_os(&info.category)]));
        let folder = Path::new(path.as_str()).parent().map(|f| f.display().to_string()).unwrap_or_default();
        d.readonly(69.0, 69.0 + pv, 232.0, 14.0, &folder);
        d.label_r(5.0, 93.0 + pv, 54.0, 10.0, "Description");
        d.edit(64.0, 91.0 + pv, 244.0, 14.0, &mut info.description, true);
        if vq > 0.0 {
            let y = 108.0 + pv;
            d.label_r(5.0, y + 2.0, 54.0, 10.0, "Quality");
            let mut q = QUALITIES.iter().find(|q| q.1 == info.yt.quality).unwrap_or(&QUALITIES[0]).0.to_string();
            let names: Vec<&str> = QUALITIES.iter().map(|q| q.0).collect();
            if d.combo(64.0, y, 92.0, 14.0, "quality", &mut q, &names, true) {
                info.yt.quality = QUALITIES.iter().find(|c| c.0 == q).map_or("", |c| c.1).to_string();
                let p = Path::new(path.as_str());
                if let Some(n) = p.file_name() {
                    *path = p.with_file_name(ytdlp::formats::named(&n.to_string_lossy(), &info.yt.quality)).display().to_string();
                }
            }
            d.check(162.0, y + 1.0, 66.0, 12.0, &mut info.yt.subs, "Subtitles");
            d.check(230.0, y + 1.0, 70.0, 12.0, &mut info.yt.thumb, "Thumbnail");
        }
        if let Some(t) = icon {
            let r = d.rect(333.0, 28.0, 20.0, 20.0);
            d.ui.put(Rect::from_center_size(r.center(), Vec2::splat(32.0)), egui::Image::new((t, Vec2::splat(32.0))));
        }
        let size_text = if !error.is_empty() { RichText::new(tr("Error")).color(Color32::RED) } else if !*applied { RichText::new(tr("")) } else { RichText::new(info.size.map_or(tr("Unknown"), human)) };
        let r = d.rect(311.0, 57.0, 64.0, 10.0);
        d.ui.put(r, egui::Label::new(size_text)).on_hover_text(error.as_str());
        if !*applied {
            let r = d.rect(333.0, 72.0, 20.0, 12.0);
            d.ui.put(r, egui::Spinner::new());
        }
        let by = 110.0 + pv + vq;
        let later = d.button(64.0, by, 74.0, 14.0, "Download Later", true);
        let now = d.button(149.0, by, 74.0, 14.0, "Start Download", true);
        let cancel = d.button(234.0, by, 74.0, 14.0, "Cancel", true);
        if later || now {
            let p = PathBuf::from(path.trim());
            let mut i = info.clone();
            i.folder = p.parent().map(|f| f.display().to_string()).filter(|f| !f.is_empty()).unwrap_or_else(|| info.folder.clone());
            i.name = p.file_name().map(|n| engine::names::sanitize(&n.to_string_lossy())).unwrap_or_default();
            if let Some(old) = *replace {
                // overwrite: the new download takes over the old one's file name (its .part is recreated)
                self.core.detach(old);
                self.sel.remove(&old);
            } else if !i.name.is_empty() {
                i.name = self.core.unique_name(&i.folder, &i.name);
            }
            let auto = { let s = self.core.settings(); s.categories.iter().find(|c| c.name == i.category).map_or(s.remember_general, |c| c.remember) };
            if *remember || auto {
                self.core.settings().cat_dirs.insert(i.category.clone(), i.folder.clone());
            }
            (i.queued, i.queue) = (later, core::queue::MAIN_QUEUE);
            self.cancels.remove(&i.url);
            if i.yt != engine::YtOpts::default() {
                // the early start fetched the default formats: start over with the chosen ones
                if let Some(j) = early.take() {
                    discard(&j);
                }
            }
            let job = match early.take() {
                Some(j) => {
                    // keep what was downloaded: stop, move the .part to the chosen name, continue
                    j.stop();
                    while j.running() {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    let mut ji = j.info();
                    let from = ji.part();
                    (ji.folder, ji.name, ji.category, ji.description, ji.queued, ji.queue) = (i.folder.clone(), i.name.clone(), i.category.clone(), i.description.clone(), i.queued, i.queue);
                    if from != ji.part() {
                        let _ = std::fs::create_dir_all(&ji.folder);
                        if std::fs::rename(&from, ji.part()).is_err() {
                            // Leave the partial at its original location if the chosen folder
                            // cannot take it. It can still be recovered from that location.
                            ji.segs.clear();
                        }
                    }
                    drop(ji);
                    self.core.adopt(j.clone());
                    j
                }
                None => self.core.add(i.clone()),
            };
            let id = i.id;
            if later {
                self.pick_queue(vec![id], false);
            }
            if now {
                self.core.start(&job);
                if self.core.settings().show_progress {
                    self.auto_progress(id);
                }
            }
        }
        if let Some(j) = if cancel { early.take() } else { None } {
            discard(&j);
        }
        if cancel && *auto {
            let n = self.cancels.entry(info.url.clone()).or_default();
            *n += 1;
            if *n >= 2 && self.core.settings().ask_exception {
                self.cancels.remove(&info.url);
                let site = url::Url::parse(&info.url).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default();
                self.open(Dialog::Cancelled { site, url: info.url.clone(), by_site: false, by_addr: true, dont_show: false });
            }
        }
        !(later || now || cancel)
    }

    pub(super) fn dialog_progress(&mut self, ui: &mut egui::Ui, id: &mut u64, tab: &mut u8, details: &mut bool) -> bool {
        let Some(job) = self.core.find(*id) else { return false };
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
        let (sl_tab, c_tab, btns) = { let s = self.core.settings(); (s.prog_sl_tab, s.prog_c_tab, s.prog_tab_btns) };
        let pages: Vec<(u8, &str)> = [(0, "Download status"), (1, "Speed Limiter"), (2, "Options on completion")].into_iter().filter(|(p, _)| *p == 0 || (*p == 1 && sl_tab) || (*p == 2 && c_tab)).collect();
        *tab = (*tab).min(pages.len() as u8 - 1);
        let names: Vec<&str> = pages.iter().map(|p| p.1).collect();
        let (px, py, _, _) = d.tabs(7.0, 5.0, 338.0, 101.0, tab, &names);
        let (ox, oy) = (px - 7.0, py - 3.0); // page templates are laid out from (7, 3)
        let page = pages[*tab as usize].0;
        // "Hide tab" (templates 364/365, 0x4a41a0 clears bHideSLTabOnDwnlDlg)
        if page > 0 && btns && d.button(ox + 252.0, oy + 64.0, 65.0, 14.0, "Hide tab", true) {
            let mut s = self.core.settings();
            if page == 1 { s.prog_sl_tab = false } else { s.prog_c_tab = false }
            drop(s);
            self.core.save();
            *tab = 0;
        }
        match page {
            0 => {
                let status = match &i.status {
                    _ if post.is_some() => {
                        let (label, f) = post.clone().unwrap();
                        if f >= 0.0 { i18n::line(&format!("{} ({}%)", tr_os(&label), (f * 100.0) as u32)) } else { tr(&label) }
                    }
                    Status::Error(e) => i18n::line(&format!("{}: {}", tr_os("Error"), i18n::tr_msg_os(e))),
                    Status::Connecting => tr("Connecting..."),
                    Status::Downloading => tr("Receiving data..."),
                    Status::Complete => tr("Complete"),
                    Status::Stopped => tr("Stopped"),
                };
                let left = i.size.filter(|_| speed > 0.0).map(|s| time_left((s.saturating_sub(done) as f64 / speed) as u64));
                // NDM's formats: "%s ( %d%s%02d %% )" until 100 %, "%s/%s" and " (Limited)" for the rate
                let downloaded = match frac {
                    Some(f) if f < 1.0 => format!("{}  ( {} % )", size3(done), pct2(f * 100.0)),
                    _ => size3(done),
                };
                let limited = i.limit_kb > 0 || self.core.settings().limit_on;
                // processing: nothing is coming in, so no rate or time left
                let left = if post.is_some() { post_left.map(time_left) } else { left };
                let speed_text = if running && post.is_none() { i18n::line(&format!("{}/{}{}", size3_os(speed as u64), tr_os("sec"), if limited { format!(" {}", tr_os("(Limited)")) } else { String::new() })) } else { String::new() };
                d.label(ox + 7.0, oy + 4.0, 329.0, 9.0, i.url.as_str());
                for (y, k, vx, v) in [
                    (15.0, "Status", 53.0, status),
                    (28.0, "File size", 75.0, i.size.map_or(tr("(unknown)"), size3)),
                    (38.0, "Downloaded", 75.0, downloaded),
                    (48.0, "Transfer rate", 75.0, speed_text),
                    (58.0, "Time left", 75.0, left.unwrap_or_else(|| if running && post.is_none() { "(unknown)".into() } else { String::new() })),
                    (68.0, "Resume capability", 127.0, if !i.probed { "(unknown)".into() } else if i.ranges { "Yes".into() } else { "No".into() }),
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
                            let rate: f64 = if running { i.segs.iter().filter(|s| s.owned && s.net == **n).map(|s| s.rate).sum() } else { 0.0 };
                            i18n::line(&format!("{n} {}/{} ({}%)", size3_os(rate as u64), tr_os("sec"), nets[*n] * 100 / total))
                        })
                        .collect::<Vec<_>>()
                        .join("   ");
                    d.label(ox + 7.0, oy + 80.0, 66.0, 9.0, "Networks");
                    d.at(ox + 75.0, oy + 80.0, 260.0, 9.0, |ui| ui.label(line));
                }
            }
            1 => {
                d.label(ox + 7.0, oy + 7.0, 63.0, 9.0, "Transfer rate");
                d.label(ox + 75.0, oy + 7.0, 202.0, 9.0, if running { i18n::line(&format!("{}/{}", size3_os(speed as u64), tr_os("sec"))) } else { String::new() });
                let mut on = i.limit_kb > 0;
                let mut kb = if on { i.limit_kb.to_string() } else { "100".into() };
                let toggled = d.check(ox + 7.0, oy + 20.0, 154.0, 10.0, &mut on, "Use Speed Limiter").changed();
                d.label(ox + 7.0, oy + 32.0, 170.0, 9.0, "Maximum download speed:");
                let edited = d.edit(ox + 7.0, oy + 43.0, 51.0, 14.0, &mut kb, on).changed();
                d.label(ox + 64.0, oy + 45.0, 65.0, 9.0, "KBytes/sec");
                if toggled || edited {
                    self.core.set_job_limit(&job, if on { kb.trim().parse().unwrap_or(100).clamp(1, 1_000_000) } else { 0 });
                }
            }
            _ => {
                d.label(ox + 7.0, oy + 3.0, 316.0, 9.0, format!("{} {}", tr_os("Save To:"), i18n::ltr(&i.path().display().to_string())));
                let mut show = self.core.settings().show_complete;
                if d.check(ox + 7.0, oy + 15.0, 169.0, 10.0, &mut show, "Show download complete dialog").changed() {
                    self.core.settings().show_complete = show;
                }
                d.hline(ox + 7.0, oy + 27.0, 316.0);
                d.label(ox + 7.0, oy + 30.0, 316.0, 9.0, "Power options");
                let flags = self.on_done.entry(*id).or_default();
                d.check(ox + 7.0, oy + 41.0, 225.0, 10.0, &mut flags.0, "Exit New Download Manager when done");
                d.check(ox + 7.0, oy + 52.0, 225.0, 10.0, &mut flags.1, "Turn off computer when done");
            }
        }
        d.progress(7.0, 111.0, 338.0, 10.0, bar);
        if d.button(15.0, 126.0, 93.0, 15.0, if *details { "<< Hide details" } else { "Show details >>" }, true) {
            *details = !*details;
            self.core.settings().prog_details = *details; // NDM saves dPrDtVis (0x4a3490)
            self.core.save();
        }
        let finished = i.status == Status::Complete;
        let pause_label = ["Resume", "Pause"][running as usize];
        if d.button(203.0, 126.0, 56.0, 15.0, pause_label, running || !finished) {
            if running { job.stop() } else { self.core.start(&job) }
        }
        let cancel = d.button(277.0, 126.0, 50.0, 15.0, "Cancel", true);
        if cancel {
            job.stop();
        }
        if *details {
            let r = d.rect(21.0, 143.0, 306.0, 9.0);
            d.ui.put(r, egui::Label::new(tr("Start positions and download progress by connections")));
            let r = d.rect(7.0, 153.0, 338.0, 10.0);
            let bars = self.res.bars.as_ref().map(|b| b.each_ref().map(|t| t.id()));
            d.ui.scope_builder(egui::UiBuilder::new().max_rect(r), |ui| segment_bar(ui, &i, r.height(), bars));
            // NDM's connections list (id 1177): N. / Downloaded / Info and an empty fourth column, a row
            // for every connection the download may open
            let cols = [("N.", 18.5), ("Downloaded", 62.5), ("Info", 197.0), ("", 58.0)];
            let rows = d.listview(7.0, 168.0, 338.0, 79.0, &cols);
            let dx = d.dx();
            let font = egui::TextStyle::Body.resolve(d.ui.style());
            let ink = d.ui.visuals().text_color();
            let n = i.conns.max(i.segs.len()).max(1);
            d.ui.scope_builder(egui::UiBuilder::new().max_rect(rows), |ui| {
                egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                    for k in 0..n {
                        let (row, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 16.0), Sense::hover());
                        let sg = i.segs.get(k);
                        let got = sg.map_or(0, |s| s.next - s.start);
                        let info = match sg {
                            Some(s) if s.done() => tr("Disconnect."),
                            Some(s) if s.net.is_empty() => tr(&s.info),
                            Some(s) => i18n::line(&format!("{} ({})", i18n::tr_msg_os(&s.info), i18n::ltr(&s.net))),
                            None => String::new(),
                        };
                        let rtl = i18n::is_rtl();
                        let mut x = if rtl { row.right() } else { row.left() };
                        for (c, text) in [format!("{:2}", k + 1), if got > 0 { size3(got) } else { String::new() }, info].into_iter().enumerate() {
                            let w = cols[c].1 * dx;
                            let clip = egui::Rect::from_min_size(egui::pos2(if rtl { x - w + 4.0 } else { x }, row.top()), Vec2::new(w - 4.0, row.height()));
                            let (at, align) = if rtl { (x - 6.0, Align2::RIGHT_CENTER) } else { (x + 6.0, Align2::LEFT_CENTER) };
                            ui.painter().with_clip_rect(clip).text(egui::pos2(at, row.center().y), align, text, font.clone(), ink);
                            x += if rtl { -w } else { w };
                        }
                    }
                });
            });
        }
        !cancel
    }

    pub(super) fn dialog_prog_custom(&mut self, ui: &mut egui::Ui, view: &mut u8, sl: &mut bool, c: &mut bool, btns: &mut bool) -> bool {
        // template 367, OK = 0x435710
        let mut d = dlu::D::new(ui);
        d.label(7.0, 6.0, 178.0, 10.0, "Start view:");
        let views = ["Show normally sized", "Show minimized", "Don't show"].map(tr);
        let mut v = views[*view as usize % 3].clone();
        if d.combo(7.0, 18.0, 90.0, 14.0, "pview", &mut v, &views.each_ref().map(String::as_str), true) {
            *view = views.iter().position(|x| *x == v).unwrap_or(0) as u8;
        }
        d.text(7.0, 34.0, 254.0, 27.0, "Note: \"Start view\" option doesn't relate to queue processing. When NDM is processing queues,  \"Download progress\" dialog always starts minimized in system tray");
        d.hline(7.0, 66.0, 254.0);
        d.check(7.0, 74.0, 254.0, 10.0, sl, "Show \"Speed Limiter\" tab");
        d.check(7.0, 88.0, 189.0, 10.0, c, "Show \"Options on completion\" tab");
        d.check(7.0, 101.0, 254.0, 10.0, btns, "Show \"Hide tab\" buttons");
        let ok = d.button(77.0, 124.0, 50.0, 14.0, "OK", true);
        if ok {
            let v = (*view, *sl, *c, *btns);
            self.opt_patch.push(Box::new(move |s: &mut Settings| (s.prog_view, s.prog_sl_tab, s.prog_c_tab, s.prog_tab_btns) = v));
        }
        !(ok || d.button(139.0, 124.0, 50.0, 14.0, "Cancel", true))
    }

    pub(super) fn dialog_complete(&mut self, ctx: &egui::Context, ui: &mut egui::Ui, id: &mut u64, dont_show: &mut bool) -> bool {
        let Some(job) = self.core.find(*id) else { return false };
        let i = job.info().clone();
        let path = i.path();
        let icon = self.icon(ctx, &core::ext(&i.name));
        let mut d = dlu::D::new(ui);
        if let Some(t) = icon {
            let r = d.rect(7.0, 7.0, 21.0, 20.0);
            d.ui.put(r, egui::Image::new((t, Vec2::splat(32.0))));
        }
        d.text(34.0, 7.0, 262.0, 32.0, format!("{}\n{}", tr_os("Download complete"), i18n::trf_os("Downloaded %s (%I64d Bytes)", &[&human_os(i.done()), &i.done()])));
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
        d.check(16.0, 117.0, 238.0, 10.0, dont_show, "Don't show this dialog again");
        if close && *dont_show {
            self.core.settings().show_complete = false;
        }
        !close
    }

    pub(super) fn dialog_delete(&mut self, ui: &mut egui::Ui, ids: &[u64], files: &mut bool, dont_show: &mut bool) -> bool {
        let mut d = dlu::D::new(ui);
        d.text(7.0, 6.0, 264.0, 18.0, "Are you sure you want to delete selected downloads from New Download Manager list of downloads?");
        d.check(12.0, 26.0, 259.0, 18.0, files, "Delete completely downloaded files from your hard disk as well. Be careful !");
        d.check(12.0, 47.0, 247.0, 10.0, dont_show, "Don't show this dialog again");
        let yes = d.button(76.0, 65.0, 50.0, 14.0, "Yes", true);
        let no = d.button(151.0, 65.0, 50.0, 14.0, "No", true);
        if yes {
            let (ids, files) = (ids.to_owned(), *files);
            if *dont_show {
                self.core.settings().confirm_delete = false;
            }
            self.remove(&ids, files);
        }
        !(yes || no)
    }

    pub(super) fn dialog_duplicate(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        inc: &mut Incoming,
        old: &mut u64,
        choice: &mut u8,
        remember: &mut bool,
    ) -> bool {
        let mut d = dlu::D::new(ui);
        d.readonly(5.0, 7.0, 274.0, 14.0, &inc.url);
        d.text(7.0, 26.0, 272.0, 18.0, "This file already exists in your download list. Would you like to add the duplicate?");
        for (k, y, h) in [(1u8, 56.0, 10.0), (2, 73.0, 10.0), (0, 86.0, 18.0)] {
            let label = DUPL[[3, 1, 2][k as usize]];
            if d.radio(7.0, y, 272.0, h, *choice == k, label) {
                *choice = k;
            }
        }
        let ok = d.button(84.0, 112.0, 50.0, 14.0, "OK", true);
        let cancel = d.button(152.0, 112.0, 50.0, 14.0, "Cancel", true);
        d.check(11.0, 132.0, 260.0, 18.0, remember, "Remember my selection and don't show this dialog again.\n You may change it in New Download Manager Options->Downloads at a later time.");
        if ok {
            {
                let mut s = self.core.settings();
                s.dupl_action = *choice;
                s.dupl_ask = !*remember;
            }
            let (inc, old, c) = (inc.clone(), *old, *choice);
            self.duplicate(ctx, inc, old, c);
        }
        !(ok || cancel)
    }

    pub(super) fn dialog_file_props(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        id: &mut u64,
        url: &mut String,
        desc: &mut String,
        referer: &mut String,
        login: &mut String,
        pass: &mut String,
    ) -> bool {
        let Some(j) = self.core.find(*id) else { return false };
        let i = j.info().clone();
        let icon = self.icon(ctx, &core::ext(&i.name));
        let mut d = dlu::D::new(ui);
        if let Some(t) = icon {
            let r = d.rect(7.0, 7.0, 20.0, 20.0);
            d.ui.put(Rect::from_min_size(r.min, Vec2::splat(32.0)), egui::Image::new((t, Vec2::splat(32.0))));
        }
        d.label(58.0, 13.0, 253.0, 9.0, i.name.as_str());
        d.hline(7.0, 31.0, 304.0);
        d.label(7.0, 36.0, 44.0, 9.0, "Type:");
        d.label(58.0, 36.0, 253.0, 9.0, i18n::line(&win::type_name(&core::ext(&i.name))));
        d.label(7.0, 48.0, 43.0, 9.0, "Status:");
        d.label(58.0, 48.0, 253.0, 9.0, "Complete");
        d.label(7.0, 60.0, 43.0, 9.0, "Size:");
        d.label(58.0, 60.0, 172.0, 9.0, i.size.map(|s| i18n::line(&format!("{} ({s} {})", human_os(s), tr_os("Bytes")))).unwrap_or_default());
        // the file's checksums, to compare with the one the site publishes
        let file = i.path();
        if d.button(234.0, 57.0, 77.0, 14.0, "Verify checksum...", file.is_file()) {
            self.open(Dialog::Checksum { name: i.name.clone(), run: crate::checksum::start(file), expect: String::new(), source: ChecksumSource::default() });
        }
        d.label(7.0, 77.0, 45.0, 9.0, "Save To:");
        d.readonly(58.0, 74.0, 197.0, 14.0, &i.path().display().to_string());
        d.button(261.0, 74.0, 50.0, 14.0, "Browse...", false);
        d.label(7.0, 96.0, 44.0, 9.0, "Address:");
        d.edit(58.0, 93.0, 253.0, 14.0, url, true);
        d.label(7.0, 115.0, 47.0, 9.0, "Description:");
        d.edit(58.0, 112.0, 253.0, 14.0, desc, true);
        d.hline(7.0, 131.0, 304.0);
        d.label(7.0, 136.0, 304.0, 9.0, "The web page which this file was obtained from:");
        d.label(7.0, 149.0, 304.0, 9.0, if i.parent.is_empty() { "Unknown" } else { i.parent.as_str() });
        d.hline(7.0, 161.0, 304.0);
        d.label(7.0, 171.0, 47.0, 9.0, "Referer:");
        d.edit(58.0, 167.0, 253.0, 14.0, referer, true);
        d.label_r(23.0, 188.0, 50.0, 9.0, "Login");
        d.edit(80.0, 185.0, 126.0, 13.0, login, true);
        d.label_r(21.0, 205.0, 52.0, 9.0, "Password");
        d.password(80.0, 203.0, 126.0, 13.0, pass, true);
        d.hline(7.0, 220.0, 304.0);
        d.label(7.0, 225.0, 160.0, 9.0, i18n::line(&format!("{} {}", tr_os("Last try date:").trim_end(), i18n::ltr(&date(i.last_try)))));
        d.label(7.0, 237.0, 160.0, 8.0, i18n::line(&format!("{} {}", tr_os("Result: ").trim_end(), tr_os("Complete"))));
        if d.button(172.0, 227.0, 50.0, 14.0, "Open", true) {
            explore(i.path());
        }
        let ok = d.button(261.0, 227.0, 50.0, 14.0, "OK", !url.trim().is_empty());
        if ok {
            let mut n = j.info();
            (n.url, n.description, n.referrer) = (join_login(url, login, pass), desc.clone(), referer.clone());
        }
        !ok
    }

    pub(super) fn dialog_dial_up(&mut self, ui: &mut egui::Ui) -> bool {
        let Some(text) = self.core.dial_text.lock().unwrap().clone() else { return false };
        let mut d = dlu::D::new(ui);
        d.text(7.0, 7.0, 172.0, 22.0, text.as_str());
        // Redial shows while waiting between attempts
        if text.contains('\n') && d.button(6.0, 34.0, 50.0, 14.0, "Redial", true) {
            self.core.dial_now.store(true, Relaxed);
        }
        if d.button(63.0, 34.0, 50.0, 14.0, "Cancel", true) {
            self.core.dial_cancel.store(true, Relaxed);
        }
        ui.ctx().request_repaint_after(Duration::from_millis(250));
        true
    }

    pub(super) fn dialog_wait_link(&mut self, ui: &mut egui::Ui, name: &mut String) -> bool {
        let mut d = dlu::D::new(ui);
        d.text(7.0, 5.0, 226.0, 17.0, format!("{}\n{name}", tr_os("NDM is waiting for the new address to resume the file:")));
        if d.button(66.0, 25.0, 107.0, 14.0, "Stop waiting", true) {
            self.refreshing = None;
        }
        self.refreshing.is_some()
    }
}

/// RichText keeps the filename's already-shaped text out of the translation and bidi pass in D::label.
fn file_info_name(name: &str) -> RichText {
    RichText::new(i18n::shown(name)).strong()
}

#[test]
fn file_info_renders_both_logical_hebrew_names() {
    let ctx = egui::Context::default();
    let output = ctx.run(egui::RawInput {
        screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(400.0, 200.0))),
        ..Default::default()
    }, |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            let mut d = dlu::D::new(ui);
            d.label(7.0, 7.0, 200.0, 12.0, file_info_name("שלום"));
            d.label(7.0, 24.0, 200.0, 12.0, file_info_name("םולש"));
        });
    });
    let rendered: Vec<_> = output.shapes.iter().filter_map(|shape| match &shape.shape {
        egui::Shape::Text(text) => Some(text.galley.text()),
        _ => None,
    }).collect();
    assert_eq!(rendered, ["םולש", "שלום"]);
}

/// The rows File Info adds: the name as it reads (`i18n::shown`) when the edit box can't show it so
/// (right-to-left letters), and a video page's Quality, Subtitles and Thumbnail.
pub(in crate::app) fn file_info_rows(info: &Info, path: &str) -> (f32, f32) {
    let name = Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let shown = if i18n::shown(&name) != name { 12.0 } else { 0.0 };
    let video = if info.ytdlp || ytdlp::is_video_page(&info.url) { 17.0 } else { 0.0 };
    (shown, video)
}

/// File Info's Quality choices and the `YtOpts::quality` each stands for.
const QUALITIES: [(&str, &str); 9] = [
    ("Best", ""),
    ("2160p", "2160"),
    ("1440p", "1440"),
    ("1080p", "1080"),
    ("720p", "720"),
    ("480p", "480"),
    ("360p", "360"),
    ("Audio only (M4A)", "m4a"),
    ("Audio only (MP3)", "mp3"),
];

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
        let bar = egui::Rect::from_min_max(egui::pos2(snap(rect.left()), top), egui::pos2(snap(rect.right()), top + 15.0 / ppp));
        let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        let col = |t: egui::TextureId, a: f32, b: f32| p.image(t, egui::Rect::from_x_y_ranges(snap(a)..=snap(b).max(snap(a) + px), bar.y_range()), uv, Color32::WHITE);
        col(bg, bar.left(), bar.right());
        col(ends, bar.left(), bar.left() + px);
        col(ends, bar.right() - px, bar.right());
        let Some(size) = info.size.filter(|s| *s > 0) else { return };
        let inner = bar.shrink2(Vec2::new(px, 0.0));
        // the file's start at the right in right-to-left languages, as the progress bar fills
        let rtl = i18n::is_rtl();
        let x = |b: u64| if rtl { inner.right() - inner.width() * (b as f32 / size as f32) } else { inner.left() + inner.width() * (b as f32 / size as f32) };
        let span = |t: egui::TextureId, a: f32, b: f32| col(t, a.min(b), a.max(b));
        let dir = if rtl { -px } else { px };
        for s in &info.segs {
            let done = if info.status == Status::Complete { s.stop.unwrap_or(s.next) } else { s.next };
            if done > s.start {
                span(blue, x(s.start) + dir, x(done));
            }
            span(red, x(s.start), x(s.start) + dir);
        }
        return;
    }
    p.rect_filled(rect, 0.0, c.field);
    p.rect_stroke(rect, 0.0, egui::Stroke::new(1.0_f32, c.edge), egui::StrokeKind::Inside);
    let Some(size) = info.size.filter(|s| *s > 0) else { return };
    let inner = rect.shrink(1.0);
    let rtl = i18n::is_rtl();
    let x = |b: u64| if rtl { inner.right() - inner.width() * (b as f32 / size as f32) } else { inner.left() + inner.width() * (b as f32 / size as f32) };
    for s in &info.segs {
        let done = if info.status == Status::Complete { s.stop.unwrap_or(s.next) } else { s.next };
        let (a, b) = (x(s.start), x(done));
        p.rect_filled(egui::Rect::from_x_y_ranges(a.min(b)..=a.max(b), inner.y_range()), 0.0, BAR);
    }
}
