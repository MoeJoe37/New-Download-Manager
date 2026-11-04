use super::*;

/// "http://u:p@host/x" -> ("http://host/x", "u", "p"); the File Properties Login/Password fields.
/// `url` with login:password put in (Add URL / Batch "Use authorization").
pub(in crate::app) fn with_login(url: &str, login: &str, pass: &str) -> String {
    match url::Url::parse(url) {
        Ok(mut u) => {
            let _ = u.set_username(login);
            let _ = u.set_password(Some(pass));
            u.to_string()
        }
        Err(_) => url.to_string(),
    }
}

pub(in crate::app) fn split_login(url: &str) -> (String, String, String) {
    match url::Url::parse(url) {
        Ok(mut u) if !u.username().is_empty() => {
            let dec = |s: &str| percent_encoding::percent_decode_str(s).decode_utf8_lossy().into_owned();
            let (l, p) = (dec(u.username()), dec(u.password().unwrap_or("")));
            let _ = u.set_username("");
            let _ = u.set_password(None);
            (u.to_string(), l, p)
        }
        _ => (url.to_string(), String::new(), String::new()),
    }
}

pub(in crate::app) fn join_login(url: &str, login: &str, pass: &str) -> String {
    match url::Url::parse(url.trim()) {
        Ok(mut u) if !login.is_empty() => {
            let _ = u.set_username(login);
            let _ = u.set_password(Some(pass).filter(|p| !p.is_empty()));
            u.to_string()
        }
        _ => url.trim().to_string(),
    }
}

pub(in crate::app) fn clipboard() -> String {
    arboard::Clipboard::new().and_then(|mut c| c.get_text()).unwrap_or_default()
}

pub(in crate::app) fn links(text: &str) -> Vec<String> {
    text.split_whitespace().filter(|w| ["http://", "https://", "ftp://"].iter().any(|p| w.starts_with(p)) || stream::is_media_url(w)).map(str::to_string).collect()
}

/// Stop an early download and delete its partial data.
pub(in crate::app) fn discard(j: &engine::Job) {
    j.stop();
    while j.running() {
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = std::fs::remove_file(j.info().part());
    let _ = std::fs::remove_dir_all(j.info().part()); // a stream's pieces
}

/// Options -> Downloads "If a duplicate download link is added:" in NDM's combo order.
pub(in crate::app) const DUPL: [&str; 4] = [
    "Show a dialog and ask what to do.",
    "Add the duplicate with a numbered file name",
    "Add the duplicate and overwrite the existing file",
    "If existing file is complete, show download complete dialog; otherwise resume it.",
];

impl App {
    pub(in crate::app) fn add_url(&mut self) {
        let url = links(&clipboard()).into_iter().next().unwrap_or_default();
        self.open(Dialog::AddUrl { url, auth: false, login: String::new(), pass: String::new(), clip: false });
    }

    /// A link that is already in the list is handled as NDM does (Options -> Downloads, dialog 298).
    pub(in crate::app) fn incoming(&mut self, ctx: &egui::Context, inc: Incoming) {
        if !inc.links.is_empty() {
            return self.all_links(inc);
        }
        if inc.start_queue {
            self.core.start_queue(core::MAIN_QUEUE);
        }
        if inc.url.is_empty() {
            return;
        }
        if inc.silent {
            // /n: no questions; /a only adds it to the queue
            let mut i = self.core.new_info(&inc.url);
            i.ytdlp = inc.video;
            i.stream = inc.stream;
            i.media_output = inc.output.clone();
            i.media_variant = inc.variant;
            (i.referrer, i.cookies, i.browser_proxy) = (inc.referrer.clone(), inc.cookies.clone(), inc.proxy.clone());
            if !inc.folder.is_empty() {
                i.folder = inc.folder.clone();
            }
            i.name = inc.filename.clone();
            // Options -> Downloads "Do not start downloading, only add files to the queue": a download
            // handed over without questions goes to the main queue instead of starting
            let later = inc.later || (!inc.cli && self.core.settings().only_queue);
            i.queued = later;
            let id = i.id;
            let job = self.core.add(i);
            if !later {
                self.core.start(&job);
                if inc.quit {
                    self.on_done.insert(id, (true, false));
                }
                // NDM still shows the progress window for /n downloads
                if self.core.settings().show_progress {
                    self.auto_progress(id);
                }
            }
            return;
        }
        let url = self.core.settings().with_login(&inc.url);
        if let Some(j) = self.refreshing.take().and_then(|id| self.core.find(id)) {
            // the first link after "Refresh download address" becomes the download's new address;
            // the next start re-checks size/ranges and the 64-byte resume check catches a different file
            {
                let mut i = j.info();
                (i.url, i.cookies, i.probed) = (url, inc.cookies, false);
                if !inc.referrer.is_empty() {
                    i.referrer = inc.referrer;
                }
            }
            let id = j.info().id;
            j.stop();
            return self.resume(&[id]);
        }
        let Some(old) = self.core.jobs().iter().map(|j| j.info()).find(|i| i.url == url).map(|i| i.id) else {
            return self.file_info(ctx, inc);
        };
        let (ask, action) = {
            let s = self.core.settings();
            (s.dupl_ask, s.dupl_action)
        };
        if ask {
            self.open(Dialog::Duplicate { inc, old, choice: action, remember: false });
        } else {
            self.duplicate(ctx, inc, old, action);
        }
    }

    /// Browser "Download all links": the page's links in the grabber's file list, files of the
    /// Options -> File types list ticked.
    pub(in crate::app) fn all_links(&mut self, inc: Incoming) {
        let types = self.core.settings().file_types.clone();
        let crawl = Arc::new(grabber::Crawl::default());
        {
            let mut st = crawl.state.lock().unwrap();
            for l in inc.links {
                let name = engine::sanitize(&engine::url_name(&l.url));
                let checked = types.split_whitespace().any(|t| t.trim_start_matches('.').eq_ignore_ascii_case(&core::ext(&name)));
                st.files.push(grabber::Found { url: l.url, name, text: l.text, checked, ..Default::default() });
            }
            (st.pages, st.done) = (1, true);
            st.status = format!("{} links", st.files.len());
        }
        let opts = grabber::Opts { name: "Download all links".into(), start: inc.url, ..Default::default() };
        let folder = self.core.settings().base_dir.clone();
        self.open(Dialog::Grabber { step: 3, adv: false, node: 1, opts, save_mode: 0, save_cat: GENERAL.into(), folder, crawl: Some(crawl) });
    }

    /// NDM's clipboard monitor: a copied address (first line, http/https) of a file type from
    /// Options -> File types opens "Enter new address" retitled (0x407900 reads it, 0x407c90 asks).
    /// The clipboard is only read while watching is on; the text already there when it starts isn't taken.
    pub(in crate::app) fn watch_clipboard(&mut self) {
        if !self.core.settings().clip_watch {
            self.clip_seq = None;
            return;
        }
        let seq = win::clip_seq();
        if self.clip_seq.replace(seq).is_none_or(|old| old == seq) {
            return;
        }
        let text = clipboard();
        let url = text.lines().next().unwrap_or("").trim().to_string();
        // NDM also takes mms:// and rtmp://; the engine speaks http(s) and ftp only
        let web = url.len() > 6 && (["http://", "https://", "ftp://"].iter().any(|p| url.get(..p.len()).is_some_and(|h| h.eq_ignore_ascii_case(p))) || stream::is_media_url(&url));
        let ext = core::ext(&engine::url_name(&url));
        let wanted = self.core.settings().file_types.split_whitespace().any(|t| t.trim_start_matches('.').eq_ignore_ascii_case(&ext));
        if web && wanted && !self.dialogs.iter().any(|(_, d)| matches!(d, Dialog::AddUrl { .. })) {
            self.open(Dialog::AddUrl { url, auth: false, login: String::new(), pass: String::new(), clip: true });
        }
    }

    pub(in crate::app) fn duplicate(&mut self, ctx: &egui::Context, inc: Incoming, old: u64, action: u8) {
        match action {
            1 => self.file_info(ctx, inc), // unique_name numbers it
            2 => {
                self.file_info(ctx, inc);
                if let (Some(j), Some((_, Dialog::FileInfo { path, replace, .. }))) = (self.core.find(old), self.dialogs.last_mut()) {
                    *path = j.info().path().display().to_string();
                    *replace = Some(old);
                }
            }
            _ => match self.core.find(old) {
                Some(j) if j.info().status == Status::Complete => self.open(Dialog::Complete { id: old, dont_show: false }),
                Some(_) => self.resume(&[old]),
                None => self.file_info(ctx, inc),
            },
        }
    }

    pub(in crate::app) fn file_info(&mut self, ctx: &egui::Context, inc: Incoming) {
        let mut info = self.core.new_info(&inc.url);
        if inc.quit {
            self.on_done.insert(info.id, (true, false));
        }
        info.parent = inc.referrer.clone(); // the extension sends the tab URL
        (info.referrer, info.cookies, info.browser_proxy) = (inc.referrer, inc.cookies, inc.proxy);
        let slot: ProbeSlot = Arc::default();
        let hint = inc.filename.rsplit(['/', '\\']).next().map(engine::sanitize).filter(|n| n != "download").unwrap_or_default();
        // a video page (YouTube..., or any page from the download panel): the Video category until yt-dlp
        // names the file from the video's title
        info.ytdlp = inc.video;
        info.stream = inc.stream;
        info.media_output = inc.output.clone();
        info.media_variant = inc.variant;
        let video_page = info.ytdlp || ytdlp::is_video_page(&info.url);
        let name = if !hint.is_empty() {
            hint.clone()
        } else if video_page {
            "video.mp4".into()
        } else {
            engine::sanitize(&engine::url_name(&info.url))
        };
        if video_page {
            info.category = self.core.settings().category_for(&name, &info.url);
            info.folder = self.core.settings().folder_for(&info.category);
        }
        if !inc.folder.is_empty() {
            info.folder = inc.folder.clone();
        }
        let path = Path::new(&info.folder).join(&name).display().to_string();
        let at_once = {
            let settings = self.core.settings();
            settings.start_at_once && !settings.only_queue && !video_page
        };
        let early = at_once.then(|| {
            // downloads into the category's folder under a free name until the dialog is answered
            let mut i = info.clone();
            i.category = self.core.settings().category_for(&name, &i.url);
            i.folder = self.core.settings().folder_for(&i.category);
            i.name = self.core.unique_name(&i.folder, &name);
            let job = engine::Job::new(i);
            self.core.start(&job);
            job
        });
        if early.is_none() {
            let (s, c, agent) = (slot.clone(), ctx.clone(), self.core.agent(&info.url));
            let (url, r, k) = (info.url.clone(), info.referrer.clone(), info.cookies.clone());
            std::thread::spawn(move || {
                let found = if video_page { ytdlp::probe(&agent, &url, &r, &k) } else { engine::probe(&agent, &url, &r, &k) };
                // a web page from any of the sites yt-dlp knows (well beyond the ones NDM names): its video
                let found = match found {
                    Ok(p) if p.page => ytdlp::probe(&agent, &url, &r, &k).or(Ok(p)),
                    // Some extractors can read a page whose ordinary HTTP request is rejected.
                    Err(e) if !video_page && (url.starts_with("https://") || url.starts_with("http://")) => ytdlp::probe(&agent, &url, &r, &k).map_err(|_| e),
                    other => other,
                };
                *s.lock().unwrap() = Some(found);
                c.request_repaint();
            });
        }
        self.open(Dialog::FileInfo { info, path, probe: slot, applied: false, error: String::new(), hint, remember: false, replace: None, auto: !inc.force, early, dir: inc.folder });
    }

    pub(in crate::app) fn resume(&mut self, ids: &[u64]) {
        let show = self.core.settings().show_progress;
        for id in ids {
            let Some(j) = self.core.find(*id) else { continue };
            if !j.running() && j.info().status != Status::Complete {
                self.core.start(&j);
                if show {
                    self.auto_progress(*id);
                }
            }
        }
    }

    /// Progress window for a download just started, as "Start view" says (0x5bbd80 reads PrgrDlgVisiblity).
    pub(in crate::app) fn auto_progress(&mut self, id: u64) {
        let view = self.core.settings().prog_view;
        if view != 2 {
            let details = self.core.settings().prog_details;
            self.open(Dialog::Progress { id, tab: 0, details, min: view == 1 });
        }
    }

    pub(in crate::app) fn stop(&self, ids: &[u64]) {
        ids.iter().filter_map(|id| self.core.find(*id)).for_each(|j| j.stop());
    }

    pub(in crate::app) fn redownload(&mut self, ids: &[u64]) {
        for id in ids {
            if let Some(j) = self.core.find(*id).filter(|j| !j.running()) {
                if j.info().status == Status::Complete {
                    let _ = std::fs::remove_file(j.info().path());
                }
                j.reset();
            }
        }
        self.resume(ids);
    }

    pub(in crate::app) fn remove(&mut self, ids: &[u64], delete_files: bool) {
        let mut jobs = self.core.jobs.lock().unwrap();
        let (gone, keep): (Vec<_>, Vec<_>) = jobs.drain(..).partition(|j| ids.contains(&j.info().id));
        *jobs = keep;
        drop(jobs);
        for j in gone {
            j.stop();
            self.sel.remove(&j.info().id);
            std::thread::spawn(move || {
                while j.running() {
                    std::thread::sleep(Duration::from_millis(50));
                }
                let i = j.info();
                let _ = std::fs::remove_file(i.part());
                let _ = std::fs::remove_dir_all(i.part());
                if delete_files && i.status == Status::Complete {
                    let _ = std::fs::remove_file(i.path());
                }
            });
        }
        self.core.save();
    }

    pub(in crate::app) fn add_queued(&self, urls: Vec<String>) -> Vec<u64> {
        let mut ids = Vec::new();
        for u in urls {
            let mut i = self.core.new_info(&u);
            i.queued = true;
            ids.push(i.id);
            self.core.add(i);
        }
        ids
    }

    /// Queue selection panel (template 345) if Options asks for it (Download Later / batch).
    pub(in crate::app) fn pick_queue(&mut self, ids: Vec<u64>, batch: bool) {
        let s = self.core.settings();
        let ask = if batch { s.ask_queue_batch } else { s.ask_queue_later };
        let q = s.queues[0].name.clone();
        drop(s);
        if ask && !ids.is_empty() {
            self.open(Dialog::QueuePick { ids, q, start: false, never: false, batch });
        }
    }

    pub(in crate::app) fn ask_delete(&mut self, ids: Vec<u64>) {
        if ids.is_empty() {
            return;
        }
        if self.core.settings().confirm_delete {
            self.open(Dialog::Delete { ids, files: false, dont_show: false });
        } else {
            self.remove(&ids, false);
        }
    }

    pub(in crate::app) fn remove_completed(&mut self) {
        let ids: Vec<u64> = self.core.jobs().iter().map(|j| j.info()).filter(|i| i.status == Status::Complete).map(|i| i.id).collect();
        self.remove(&ids, false);
    }
}
