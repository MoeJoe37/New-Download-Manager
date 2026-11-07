use crate::app::dialogs::{ChecksumSource, Dialog, ProbeSlot};
use crate::app::{
    core, date, discard, dlu, egui, engine, explore, human, human_os, i18n, join_login, tr, tr_os,
    win, with_login, ytdlp, App, Arc, Color32, Duration, Incoming, Info, Key, Path, PathBuf, Rect,
    Relaxed, RichText, Status, Vec2, DUPL,
};

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
        let address = d.edit(45.0, 7.0, 264.0, 14.0, url, true);
        if d.ui.memory(|m| m.focused().is_none()) {
            address.request_focus();
        }
        d.group(7.0, 24.0, 302.0, 29.0, "");
        d.check(13.0, 22.0, 80.0, 12.0, auth, "Use authorization");
        d.label_r(13.0, 36.0, 60.0, 9.0, "Login");
        d.edit(77.0, 34.0, 78.0, 14.0, login, *auth);
        d.label_r(160.0, 36.0, 64.0, 9.0, "Password");
        let r = d.rect(228.0, 34.0, 75.0, 14.0);
        d.ui.put(r, |ui: &mut egui::Ui| {
            ui.add_enabled(
                *auth,
                egui::TextEdit::singleline(pass)
                    .password(true)
                    .desired_width(r.width())
                    .margin(egui::Margin::symmetric(3, 2)),
            )
        });
        let enter = d.ui.input(|i| i.key_pressed(Key::Enter));
        let ok = d.button(316.0, 7.0, 50.0, 13.0, "OK", !url.trim().is_empty())
            || (enter && !url.trim().is_empty());
        let cancel = d.button(316.0, 24.0, 50.0, 14.0, "Cancel", true);
        if ok {
            let u = if *auth {
                with_login(url.trim(), login, pass)
            } else {
                url.trim().to_string()
            };
            self.incoming(
                ctx,
                Incoming {
                    url: u,
                    force: true,
                    ..Default::default()
                },
            );
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
                _ if i.probed => Some(Ok(engine::probe::Probe {
                    name: i.name.clone(),
                    size: i.size,
                    ranges: i.ranges,
                    modified: i.modified.clone(),
                    etag: i.etag.clone(),
                    stream: i.stream,
                    page: i.page,
                    ytdlp: i.ytdlp,
                })),
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
                        let name = if hint.is_empty() {
                            p.name.clone()
                        } else {
                            hint.to_owned()
                        };
                        info.category = self.core.settings().category_for(&name, &info.url);
                        info.folder = if dir.is_empty() {
                            self.core.settings().folder_for(&info.category)
                        } else {
                            dir.to_owned()
                        };
                        if replace.is_none() {
                            let name = ytdlp::formats::named(&name, &info.yt.quality);
                            let name = self.core.unique_name(&info.folder, &name);
                            *path = Path::new(&info.folder).join(name).display().to_string();
                        }
                        (info.size, info.ranges, info.probed, info.stream) =
                            (p.size, p.ranges, true, p.stream);
                        info.page = p.page;
                        info.modified = p.modified;
                        info.etag = p.etag;
                        info.ytdlp |= p.ytdlp;
                    }
                    Err(e) => {
                        // yt-dlp's own message says what is missing or wrong (Set yt-dlp, a sign-in...)
                        if info.ytdlp || ytdlp::is_video_page(&info.url) {
                            self.open(Dialog::Info {
                                title: "Download File Info".into(),
                                text: i18n::tr_msg_os(&e),
                            });
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
        let changed = d.combo(
            64.0,
            22.0,
            116.0,
            14.0,
            "cat",
            &mut info.category,
            &cats,
            true,
        );
        if d.button(183.0, 21.0, 16.0, 14.0, "+", true) {
            self.edit_category(None);
        }
        if changed {
            let name = Path::new(path.as_str())
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            *path = Path::new(&self.core.settings().folder_for(&info.category))
                .join(name)
                .display()
                .to_string();
        }
        let (pv, vq) = file_info_rows(info, path);
        if pv > 0.0 {
            // the name as it reads; the edit box below shows it in the order it is typed
            let name = Path::new(path.as_str())
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
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
        d.check(
            69.0,
            53.0 + pv,
            200.0,
            13.0,
            remember,
            &i18n::trf_os(
                "Remember this path for \"%s\" category",
                &[&tr_os(&info.category)],
            ),
        );
        let folder = Path::new(path.as_str())
            .parent()
            .map(|f| f.display().to_string())
            .unwrap_or_default();
        d.readonly(69.0, 69.0 + pv, 232.0, 14.0, &folder);
        d.label_r(5.0, 93.0 + pv, 54.0, 10.0, "Description");
        d.edit(64.0, 91.0 + pv, 244.0, 14.0, &mut info.description, true);
        if vq > 0.0 {
            let y = 108.0 + pv;
            d.label_r(5.0, y + 2.0, 54.0, 10.0, "Quality");
            let mut q = QUALITIES
                .iter()
                .find(|q| q.1 == info.yt.quality)
                .unwrap_or(&QUALITIES[0])
                .0
                .to_string();
            let names: Vec<&str> = QUALITIES.iter().map(|q| q.0).collect();
            if d.combo(64.0, y, 92.0, 14.0, "quality", &mut q, &names, true) {
                info.yt.quality = QUALITIES
                    .iter()
                    .find(|c| c.0 == q)
                    .map_or("", |c| c.1)
                    .to_string();
                let p = Path::new(path.as_str());
                if let Some(n) = p.file_name() {
                    *path = p
                        .with_file_name(ytdlp::formats::named(
                            &n.to_string_lossy(),
                            &info.yt.quality,
                        ))
                        .display()
                        .to_string();
                }
            }
            d.check(162.0, y + 1.0, 66.0, 12.0, &mut info.yt.subs, "Subtitles");
            d.check(230.0, y + 1.0, 70.0, 12.0, &mut info.yt.thumb, "Thumbnail");
        }
        if let Some(t) = icon {
            let r = d.rect(333.0, 28.0, 20.0, 20.0);
            d.ui.put(
                Rect::from_center_size(r.center(), Vec2::splat(32.0)),
                egui::Image::new((t, Vec2::splat(32.0))),
            );
        }
        let size_text = if !error.is_empty() {
            RichText::new(tr("Error")).color(crate::appearance::themes::role(
                "danger",
                d.ui.visuals().dark_mode,
                Color32::RED,
            ))
        } else if !*applied {
            RichText::new(tr(""))
        } else {
            RichText::new(info.size.map_or(tr("Unknown"), human))
        };
        let r = d.rect(311.0, 57.0, 64.0, 10.0);
        d.ui.put(r, egui::Label::new(size_text))
            .on_hover_text(error.as_str());
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
            i.folder = p
                .parent()
                .map(|f| f.display().to_string())
                .filter(|f| !f.is_empty())
                .unwrap_or_else(|| info.folder.clone());
            i.name = p
                .file_name()
                .map(|n| engine::names::sanitize(&n.to_string_lossy()))
                .unwrap_or_default();
            if let Some(old) = *replace {
                // overwrite: the new download takes over the old one's file name (its .part is recreated)
                self.core.detach(old);
                self.sel.remove(&old);
            } else if !i.name.is_empty() {
                i.name = self.core.unique_name(&i.folder, &i.name);
            }
            let auto = {
                let s = self.core.settings();
                s.categories
                    .iter()
                    .find(|c| c.name == i.category)
                    .map_or(s.remember_general, |c| c.remember)
            };
            if *remember || auto {
                self.core
                    .settings()
                    .cat_dirs
                    .insert(i.category.clone(), i.folder.clone());
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
                    (
                        ji.folder,
                        ji.name,
                        ji.category,
                        ji.description,
                        ji.queued,
                        ji.queue,
                    ) = (
                        i.folder.clone(),
                        i.name.clone(),
                        i.category.clone(),
                        i.description.clone(),
                        i.queued,
                        i.queue,
                    );
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
                let site = url::Url::parse(&info.url)
                    .ok()
                    .and_then(|u| u.host_str().map(str::to_string))
                    .unwrap_or_default();
                self.open(Dialog::Cancelled {
                    site,
                    url: info.url.clone(),
                    by_site: false,
                    by_addr: true,
                    dont_show: false,
                });
            }
        }
        !(later || now || cancel)
    }

    pub(super) fn dialog_delete(
        &mut self,
        ui: &mut egui::Ui,
        ids: &[u64],
        files: &mut bool,
        dont_show: &mut bool,
    ) -> bool {
        let mut d = dlu::D::new(ui);
        d.text(7.0, 6.0, 264.0, 18.0, "Are you sure you want to delete selected downloads from New Download Manager list of downloads?");
        d.check(
            12.0,
            26.0,
            259.0,
            18.0,
            files,
            "Delete completely downloaded files from your hard disk as well. Be careful !",
        );
        d.check(
            12.0,
            47.0,
            247.0,
            10.0,
            dont_show,
            "Don't show this dialog again",
        );
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
        d.text(
            7.0,
            26.0,
            272.0,
            18.0,
            "This file already exists in your download list. Would you like to add the duplicate?",
        );
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
        let Some(j) = self.core.find(*id) else {
            return false;
        };
        let i = j.info().clone();
        let icon = self.icon(ctx, &core::ext(&i.name));
        let mut d = dlu::D::new(ui);
        if let Some(t) = icon {
            let r = d.rect(7.0, 7.0, 20.0, 20.0);
            d.ui.put(
                Rect::from_min_size(r.min, Vec2::splat(32.0)),
                egui::Image::new((t, Vec2::splat(32.0))),
            );
        }
        d.label(58.0, 13.0, 253.0, 9.0, i.name.as_str());
        d.hline(7.0, 31.0, 304.0);
        d.label(7.0, 36.0, 44.0, 9.0, "Type:");
        d.label(
            58.0,
            36.0,
            253.0,
            9.0,
            i18n::line(&win::type_name(&core::ext(&i.name))),
        );
        d.label(7.0, 48.0, 43.0, 9.0, "Status:");
        d.label(58.0, 48.0, 253.0, 9.0, "Complete");
        d.label(7.0, 60.0, 43.0, 9.0, "Size:");
        d.label(
            58.0,
            60.0,
            172.0,
            9.0,
            i.size
                .map(|s| i18n::line(&format!("{} ({s} {})", human_os(s), tr_os("Bytes"))))
                .unwrap_or_default(),
        );
        // the file's checksums, to compare with the one the site publishes
        let file = i.path();
        if d.button(
            234.0,
            57.0,
            77.0,
            14.0,
            "Verify checksum...",
            file.is_file(),
        ) {
            self.open(Dialog::Checksum {
                name: i.name.clone(),
                run: crate::checksum::start(file),
                expect: String::new(),
                source: ChecksumSource::default(),
            });
        }
        d.label(7.0, 77.0, 45.0, 9.0, "Save To:");
        d.readonly(58.0, 74.0, 197.0, 14.0, &i.path().display().to_string());
        d.button(261.0, 74.0, 50.0, 14.0, "Browse...", false);
        d.label(7.0, 96.0, 44.0, 9.0, "Address:");
        d.edit(58.0, 93.0, 253.0, 14.0, url, true);
        d.label(7.0, 115.0, 47.0, 9.0, "Description:");
        d.edit(58.0, 112.0, 253.0, 14.0, desc, true);
        d.hline(7.0, 131.0, 304.0);
        d.label(
            7.0,
            136.0,
            304.0,
            9.0,
            "The web page which this file was obtained from:",
        );
        d.label(
            7.0,
            149.0,
            304.0,
            9.0,
            if i.parent.is_empty() {
                "Unknown"
            } else {
                i.parent.as_str()
            },
        );
        d.hline(7.0, 161.0, 304.0);
        d.label(7.0, 171.0, 47.0, 9.0, "Referer:");
        d.edit(58.0, 167.0, 253.0, 14.0, referer, true);
        d.label_r(23.0, 188.0, 50.0, 9.0, "Login");
        d.edit(80.0, 185.0, 126.0, 13.0, login, true);
        d.label_r(21.0, 205.0, 52.0, 9.0, "Password");
        d.password(80.0, 203.0, 126.0, 13.0, pass, true);
        d.hline(7.0, 220.0, 304.0);
        d.label(
            7.0,
            225.0,
            160.0,
            9.0,
            i18n::line(&format!(
                "{} {}",
                tr_os("Last try date:").trim_end(),
                i18n::ltr(&date(i.last_try))
            )),
        );
        d.label(
            7.0,
            237.0,
            160.0,
            8.0,
            i18n::line(&format!(
                "{} {}",
                tr_os("Result: ").trim_end(),
                tr_os("Complete")
            )),
        );
        if d.button(172.0, 227.0, 50.0, 14.0, "Open", true) {
            explore(i.path());
        }
        let ok = d.button(261.0, 227.0, 50.0, 14.0, "OK", !url.trim().is_empty());
        if ok {
            let mut n = j.info();
            (n.url, n.description, n.referrer) =
                (join_login(url, login, pass), desc.clone(), referer.clone());
        }
        !ok
    }

    pub(super) fn dialog_dial_up(&mut self, ui: &mut egui::Ui) -> bool {
        let Some(text) = self.core.dial_text.lock().unwrap().clone() else {
            return false;
        };
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
        d.text(
            7.0,
            5.0,
            226.0,
            17.0,
            format!(
                "{}\n{name}",
                tr_os("NDM is waiting for the new address to resume the file:")
            ),
        );
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
    let output = ctx.run(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                egui::Pos2::ZERO,
                Vec2::new(400.0, 200.0),
            )),
            ..Default::default()
        },
        |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let mut d = dlu::D::new(ui);
                d.label(7.0, 7.0, 200.0, 12.0, file_info_name("שלום"));
                d.label(7.0, 24.0, 200.0, 12.0, file_info_name("םולש"));
            });
        },
    );
    let rendered: Vec<_> = output
        .shapes
        .iter()
        .filter_map(|shape| match &shape.shape {
            egui::Shape::Text(text) => Some(text.galley.text()),
            _ => None,
        })
        .collect();
    assert_eq!(rendered, ["םולש", "שלום"]);
}

/// The rows File Info adds: the name as it reads (`i18n::shown`) when the edit box can't show it so
/// (right-to-left letters), and a video page's Quality, Subtitles and Thumbnail.
pub(in crate::app) fn file_info_rows(info: &Info, path: &str) -> (f32, f32) {
    let name = Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let shown = if i18n::shown(&name) != name {
        12.0
    } else {
        0.0
    };
    let video = if info.ytdlp || ytdlp::is_video_page(&info.url) {
        17.0
    } else {
        0.0
    };
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
