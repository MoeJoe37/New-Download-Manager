//! App state shared by the UI and a background thread that runs the download queue,
//! the scheduler, autosave and the localhost endpoint the browser extension talks to.
pub(crate) mod settings;
pub(crate) mod state;
pub(crate) mod browser_bridge;
pub(crate) mod queue;
pub(crate) mod routing;
#[cfg(test)]
mod tests;

use crate::transport;
use self::queue::{MAIN_QUEUE, Queue, Quota, Sched};
use self::routing::{AdvProxy, PROXY_PORT, pac_route};
use self::settings::{Browser, GENERAL, GrabProject, Login, Settings, browser_kind};
use crate::engine::{self, Info, Job, Limiter, Status};
use chrono::{Local, NaiveDate};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

pub fn ext(name: &str) -> String {
    name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default()
}

impl Settings {

    /// First category whose file types contain the extension (and whose sites match, if restricted).
    pub fn category_for(&self, name: &str, url: &str) -> String {
        let e = ext(name);
        let host = url::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default();
        self.categories
            .iter()
            .find(|c| !e.is_empty() && c.exts.split_whitespace().any(|x| x.trim_start_matches('.').eq_ignore_ascii_case(&e)) && (!c.sites_on || crate::grabber::mask_match(&c.sites, &host)))
            .map_or(GENERAL.into(), |c| c.name.clone())
    }

    /// "General" then the user categories, for combo boxes.
    pub fn category_names(&self) -> Vec<String> {
        std::iter::once(GENERAL.to_string()).chain(self.categories.iter().map(|c| c.name.clone())).collect()
    }

    pub fn folder_for(&self, category: &str) -> String {
        if let Some(d) = self.cat_dirs.get(category) {
            d.clone()
        } else if category == GENERAL {
            self.base_dir.clone()
        } else {
            Path::new(&self.base_dir).join(category).display().to_string()
        }
    }
}

pub struct Core {
    pub quota: Mutex<Quota>,
    /// Text of NDM's "Download limits exceeded!" message (template 194) for the UI to show.
    pub quota_msg: Mutex<Option<String>>,
    /// Queues that ran out of work; the UI applies their "when done" options.
    pub queue_finished: Mutex<Vec<u32>>,
    pub jobs: Mutex<Vec<Arc<Job>>>,
    pub settings: Mutex<Settings>,
    running: Mutex<BTreeSet<u32>>,
    pub limiter: Arc<Limiter>,
    /// One HTTP agent per proxy in use ("" = direct); rebuilt when the settings change.
    agents: Mutex<HashMap<String, transport::Agent>>,
    #[cfg(target_os = "macos")]
    pub(crate) proxy_ready: std::sync::atomic::AtomicBool,
    next_id: AtomicU64,
    /// Per queue: the days its start / stop time last fired.
    fired: Mutex<HashMap<u32, (Option<NaiveDate>, Option<NaiveDate>)>>,
    /// The Speed Limiter's time window as last seen (`tick_limit`): it switches only when this changes.
    limit_window: Mutex<Option<bool>>,
    /// Files the browser extension is handing over from their pages (`feed`): download id -> its .part file.
    fed: Mutex<HashMap<u64, std::fs::File>>,
    /// Synchronization: files checked in the current pass (job id -> queue); next pass per queue.
    synced: Mutex<HashMap<u64, u32>>,
    again: Mutex<HashMap<u32, Instant>>,
    checking: Arc<Mutex<HashSet<u64>>>,
    store: PathBuf,
    /// Grabber projects: the days their start / stop time fired, periodic next runs, running projects,
    /// and the downloads a scheduled run added (for its when-done options).
    grab_fired: Mutex<HashMap<String, (Option<NaiveDate>, Option<NaiveDate>)>>,
    grab_again: Mutex<HashMap<String, Instant>>,
    grab_running: Mutex<HashSet<String>>,
    grab_watch: Mutex<Vec<(Vec<u64>, Sched)>>,
    /// Grabbed batches (job id, address) whose pages get their links converted once all have finished.
    grab_convert: Mutex<Vec<Vec<(u64, String)>>>,
    /// "Start Download checked files": downloads the grabber runs itself, Grabber Settings' N at a time.
    pub grab_dl_ids: Mutex<Vec<u64>>,
    /// Manual login page: the site whose cookies the browser extension should send (`POST /cookies`), and
    /// the latest it sent.
    pub login_wait: Mutex<Option<String>>,
    pub login_cookies: Mutex<String>,
    /// When-done options (exit / power off / open a file) due now, applied by the UI like a finished queue's.
    pub sched_done: Mutex<Vec<Sched>>,
    /// Dial Up / VPN (`ensure_online`): one dial at a time, its status for the "Dial up" box, Cancel / Redial.
    dial_lock: Mutex<()>,
    pub dial_text: Mutex<Option<String>>,
    pub dial_cancel: std::sync::atomic::AtomicBool,
    pub dial_now: std::sync::atomic::AtomicBool,
}

impl Core {

    pub fn settings(&self) -> MutexGuard<'_, Settings> {
        self.settings.lock().unwrap()
    }

    pub fn jobs(&self) -> Vec<Arc<Job>> {
        self.jobs.lock().unwrap().clone()
    }

    pub fn find(&self, id: u64) -> Option<Arc<Job>> {
        self.jobs().into_iter().find(|j| j.info().id == id)
    }

    /// Push limiter/proxy settings into the running engine.
    pub fn apply_settings(&self) {
        let s = self.settings().clone();
        self.limiter.rate.store(if s.limit_on { s.limit_kb * 1024 } else { 0 }, Relaxed);
        self.agents.lock().unwrap().clear();
        let pasv = s.ftp_pasv;
        let routes = s;
        *engine::ftp::FTP.lock().unwrap() = engine::ftp::FtpCfg { pasv, route: Some(Arc::new(move |u: &str| routes.proxy_for(u))) };
    }

    pub fn new_info(&self, url: &str) -> Info {
        let s = self.settings();
        Info {
            id: self.next_id.fetch_add(1, Relaxed),
            url: s.with_login(url),
            conns: s.conns,
            category: GENERAL.into(),
            folder: s.base_dir.clone(),
            temp: s.temp_dir.trim().to_string(),
            added: Local::now().timestamp(),
            ..Default::default()
        }
    }

    /// Name not used by any other download heading to the same folder.
    /// A finished download whose name doesn't say what it is (no extension, a script's, one no category
    /// has): its first bytes do. It takes that extension and category, and moves to the category's folder
    /// when it was in the General folder. True when it changed.
    pub fn classify(&self, job: &engine::Job) -> bool {
        let (path, name, folder, url, category) = {
            let i = job.info();
            (i.path(), i.name.clone(), i.folder.clone(), i.url.clone(), i.category.clone())
        };
        let known = self.settings().category_for(&name, &url) != GENERAL;
        if !path.is_file() {
            return false;
        }
        let Some(ext) = engine::names::sniff(&path) else { return false };
        let typed = engine::names::media_name(&name, ext);
        if known && typed == name { return false; }
        let new_cat = self.settings().category_for(&typed, &url);
        if typed == name && new_cat == category {
            return false;
        }
        let general = self.settings().folder_for(GENERAL);
        let in_general = Path::new(&folder) == Path::new(&general);
        let dir = if new_cat != GENERAL && in_general { self.settings().folder_for(&new_cat) } else { folder.clone() };
        let new_name = if typed == name && dir == folder { name.clone() } else { self.unique_name(&dir, &typed) };
        let to = Path::new(&dir).join(&new_name);
        if to != path {
            let moved = std::fs::create_dir_all(&dir).and_then(|_| std::fs::rename(&path, &to));
            if moved.is_err() {
                // another drive: keep the folder, the name still says what it is
                let here = Path::new(&folder).join(self.unique_name(&folder, &typed));
                if std::fs::rename(&path, &here).is_err() {
                    return false;
                }
                let mut i = job.info();
                (i.name, i.category) = (here.file_name().unwrap_or_default().to_string_lossy().to_string(), new_cat);
                return true;
            }
        }
        let mut i = job.info();
        (i.name, i.folder, i.category) = (new_name, dir, new_cat);
        true
    }

    pub fn unique_name(&self, folder: &str, name: &str) -> String {
        let jobs = self.jobs();
        engine::names::unique_name(folder, name, &|n| jobs.iter().any(|j| {
            let i = j.info();
            i.folder == folder && i.name == n && i.status != Status::Complete
        }))
    }

    /// Queue-added downloads with an unknown name get one (and a category) from the URL.
    pub fn add(&self, mut info: Info) -> Arc<Job> {
        if info.name.is_empty() && (info.ytdlp || crate::ytdlp::is_video_page(&info.url)) {
            // a video page: the video's category; yt-dlp names the file from its title when it starts
            info.category = self.settings().category_for("video.mp4", &info.url);
            info.folder = self.settings().folder_for(&info.category);
        } else if info.name.is_empty() {
            let name = engine::names::sanitize(&engine::names::url_name(&info.url));
            if name != "download" {
                info.category = self.settings().category_for(&name, &info.url);
                info.folder = self.settings().folder_for(&info.category);
                info.name = self.unique_name(&info.folder, &name);
            }
        }
        let job = Job::new(info);
        self.jobs.lock().unwrap().push(job.clone());
        job
    }

    /// Put a download that already exists (started from "Download File Info") in the list.
    pub fn adopt(&self, job: Arc<Job>) {
        self.jobs.lock().unwrap().push(job);
    }

    /// Take a download out of the list, leaving its files alone. Waits (briefly) until it stopped writing.
    pub fn detach(&self, id: u64) {
        let Some(j) = self.find(id) else { return };
        j.stop();
        let until = Instant::now() + Duration::from_secs(3);
        while j.running() && Instant::now() < until {
            thread::sleep(Duration::from_millis(20));
        }
        self.jobs.lock().unwrap().retain(|x| !Arc::ptr_eq(x, &j));
    }

    /// "Use Windows Dial Up / VPN Networking" (Options page 185):
    /// before a download connects, the chosen connection is dialled unless one is up, with the
    /// user name and password Windows keeps for it. A failed dial is redialled after "Time between redial
    /// attempts" (attempts 0 = endlessly), shown in NDM's "Dial up" box (184, `dial_text`). Downloads
    /// starting meanwhile wait for the same dial.
    pub fn ensure_online(&self) -> Result<(), String> {
        let (on, entry, tries, wait) = {
            let s = self.settings();
            (s.dial_on, s.dial_entry.clone(), s.dial_redial, s.dial_wait)
        };
        if !on || entry.is_empty() {
            return Ok(());
        }
        let _one = self.dial_lock.lock().unwrap();
        if crate::win::ras_connected() {
            return Ok(());
        }
        self.dial_cancel.store(false, Relaxed);
        let say = |t: Option<String>| *self.dial_text.lock().unwrap() = t;
        let mut n = 1;
        let r = loop {
            say(Some(crate::i18n::tr_os("Connecting ...")));
            let e = match crate::win::ras_dial(&entry) {
                Ok(()) => break Ok(()),
                Err(e) => e,
            };
            if tries > 0 && n >= tries {
                break Err(e);
            }
            n += 1;
            // ponytail: a dial in progress can't be cancelled (RasDial runs synchronously); the wait can
            for left in (1..=wait.max(1)).rev() {
                if self.dial_cancel.load(Relaxed) || self.dial_now.swap(false, Relaxed) {
                    break;
                }
                say(Some(format!("{}\n{}", crate::i18n::tr_msg_os(&e), crate::i18n::trf_os("Redial  %ld", &[&left]))));
                thread::sleep(Duration::from_secs(1));
            }
            if self.dial_cancel.load(Relaxed) {
                break Err(e);
            }
        };
        say(None);
        r
    }

    pub fn start(self: &Arc<Self>, job: &Arc<Job>) {
        if self.settings().dial_on && !crate::win::ras_connected() {
            // dial first, off the UI thread; the download starts once connected
            let (core, job) = (self.clone(), job.clone());
            job.info().status = Status::Connecting;
            thread::spawn(move || match core.ensure_online() {
                Ok(()) => core.start_now(&job),
                Err(e) => job.info().status = Status::Error(e),
            });
            return;
        }
        self.start_now(job);
    }

    fn start_now(&self, job: &Arc<Job>) {
        self.update_job_limit(job);
        let url = job.info().url.clone();
        {
            let s = self.settings();
            job.set_date.store(s.set_date, Relaxed);
            job.ignore_date.store(s.ignore_date, Relaxed);
            let host = url::Url::parse(&url).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default();
            if let Some((_, n)) = s.server_conns.iter().find(|(m, _)| crate::grabber::mask_match(m, &host)) {
                job.info().conns = *n;
            }
        }
        // a download from the browser that failed before goes through the browser's own proxy
        let bp = job.info().browser_proxy.clone();
        let (ua, on) = { let s = self.settings(); (s.user_agent.clone(), s.proxy_browser) }; // one lock: a second one in the same statement deadlocks
        let bp = match bp.strip_prefix("pac:") {
            Some(pac) => pac_route(pac, &url),
            None => bp,
        };
        // ponytail: used from the next start (Resume, queue retries), not by restarting the failed one at once
        let browser = on && !bp.is_empty() && job.fails.load(Relaxed) > 0;
        let agent = if browser { transport::client::agent(&crate::net::agent_proxy(&bp), &ua) } else { self.agent(&url) };
        job.multi_ok.store(!browser, Relaxed);
        *job.lanes.lock().unwrap() = if browser { vec![] } else { self.lanes(&url) };
        job.start(agent, self.limiter.clone());
    }

    pub fn grab_tuning(&self) -> crate::grabber::Tuning {
        let s = self.settings();
        crate::grabber::Tuning { explore: s.grab_explore, adv: s.grab_adv }
    }

    /// Folder of a grabbed file. Save To: 0 = its category's folder, 1 = `save_cat`'s, 2 = `folder`
    /// (+ the site's own folders with "Use original relative subfolders").
    pub fn grab_dir(&self, name: &str, url: &str, save_mode: u8, save_cat: &str, folder: &str, subfolders: bool) -> String {
        let s = self.settings();
        match save_mode {
            1 => s.folder_for(save_cat),
            2 if subfolders => url::Url::parse(url).map_or(folder.into(), |u| Path::new(folder).join(crate::grabber::site_dir(&u)).display().to_string()),
            2 => folder.into(),
            _ => s.folder_for(&s.category_for(name, url)),
        }
    }

    /// Add the grabber's checked files to the main list, unticking them: queued in the main queue, or with
    /// `start` downloaded by the grabber itself (Grabber Settings' N at a time, `tick_grabber`).
    /// Existing names get NDM's grabber numbering (index_2.html) unless "Overwrite existing files".
    pub fn grab_files(&self, files: &mut [crate::grabber::Found], opts: &crate::grabber::Opts, save_mode: u8, save_cat: &str, folder: &str, start: bool) -> Vec<u64> {
        let mut batch = vec![];
        let (link_text, ie_cache) = { let s = self.settings(); (s.grab_link_text, s.grab_ie_cache) };
        for f in files.iter_mut().filter(|f| f.checked) {
            f.checked = false;
            let mut i = self.new_info(&f.url);
            (i.queued, i.cookies, i.ie_cache) = (!start, opts.cookies.clone(), ie_cache);
            if link_text {
                i.description = f.text.clone();
            }
            i.folder = self.grab_dir(&f.name, &f.url, save_mode, save_cat, folder, opts.subfolders);
            i.category = self.settings().category_for(&f.name, &f.url);
            i.name = if opts.overwrite { f.name.clone() } else { crate::grabber::numbered(&f.name, &|n| self.unique_name(&i.folder, n) == n) };
            batch.push((i.id, f.url.clone()));
            self.add(i);
        }
        let ids: Vec<u64> = batch.iter().map(|b| b.0).collect();
        if opts.convert && !batch.is_empty() {
            self.grab_convert.lock().unwrap().push(batch);
        }
        if start {
            self.grab_dl_ids.lock().unwrap().extend(&ids);
        }
        ids
    }

    /// Scheduled grabber projects (template 284): start / stop times like a queue's, "Start again every"
    /// for periodic ones; a run explores and/or downloads, then its when-done options wait for the files.
    fn tick_grabber(self: &Arc<Self>) {
        let now = Local::now();
        let (hm, today) = (now.format("%H:%M").to_string(), now.date_naive());
        use chrono::Datelike;
        let mut due = vec![];
        let mut stop = false;
        {
            let mut s = self.settings();
            let mut fired = self.grab_fired.lock().unwrap();
            let mut again = self.grab_again.lock().unwrap();
            for p in &mut s.grab_projects {
                let f = fired.entry(p.opts.name.clone()).or_default();
                let sc = &p.sched;
                let day_ok = !sc.daily || sc.days[now.weekday().num_days_from_sunday() as usize];
                let timed = sc.start_on && day_ok && hm == sc.start_at && f.0 != Some(today);
                let periodic = again.get(&p.opts.name).is_some_and(|t| *t <= Instant::now());
                if timed || periodic {
                    f.0 = Some(today);
                    p.sched.start_on = p.sched.daily || p.periodic;
                    again.remove(&p.opts.name);
                    due.push(p.clone());
                }
                if p.sched.stop_on && hm == p.sched.stop_at && f.1 != Some(today) {
                    f.1 = Some(today);
                    p.sched.stop_on = p.sched.daily;
                    stop = true;
                }
            }
        }
        if stop {
            self.stop_queue(MAIN_QUEUE);
        }
        for p in due {
            self.run_project(p);
        }
        // when-done options once every file of a scheduled run has finished (or failed)
        let mut watch = self.grab_watch.lock().unwrap();
        watch.retain(|(ids, sc)| {
            let over = ids.iter().filter_map(|id| self.find(*id)).all(|j| matches!(j.info().status, Status::Complete | Status::Error(_)));
            if over {
                self.sched_done.lock().unwrap().push(sc.clone());
            }
            !over
        });
        drop(watch);
        // the grabber's own downloads: N at a time; finished, failed, stopped by the user or deleted leave the list
        let n = self.settings().grab_dl.clamp(1, 10);
        let mut dl = self.grab_dl_ids.lock().unwrap();
        let mut active = 0;
        dl.retain(|id| {
            let Some(j) = self.find(*id) else { return false };
            let (st, started) = { let i = j.info(); (i.status.clone(), i.done() > 0 || j.running()) };
            if j.running() {
                active += 1;
                return true;
            }
            if st != Status::Stopped || started {
                return false;
            }
            if active < n && !self.quota_blocked() {
                self.start(&j);
                active += 1;
            }
            true
        });
        drop(dl);
        // ponytail: pending conversions live in memory only; a restart before the batch ends skips them
        let mut conv = self.grab_convert.lock().unwrap();
        conv.retain(|batch| {
            let infos: Vec<(String, Info)> = batch.iter().filter_map(|(id, u)| Some((u.clone(), self.find(*id)?.info().clone()))).collect();
            if !infos.iter().all(|(_, i)| matches!(i.status, Status::Complete | Status::Error(_))) {
                return true;
            }
            let local: HashMap<String, PathBuf> = infos.iter().filter(|(_, i)| i.status == Status::Complete).map(|(u, i)| (u.clone(), i.path())).collect();
            for (u, p) in &local {
                let ext = p.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
                let (Ok(base), true) = (url::Url::parse(u), ["htm", "html", "shtml", "xhtml", "php", "asp", "aspx", "jsp"].contains(&ext.as_str())) else { continue };
                let Ok(bytes) = std::fs::read(p) else { continue };
                // non-UTF-8 pages go through Latin-1 so every byte comes back unchanged (new links are ASCII)
                let utf8 = std::str::from_utf8(&bytes).is_ok();
                let html: String = if utf8 { String::from_utf8_lossy(&bytes).into() } else { bytes.iter().map(|&b| b as char).collect() };
                let out = crate::grabber::localize(&html, &base, p, &|l| local.get(l.as_str()).cloned());
                let _ = std::fs::write(p, if utf8 { out.into_bytes() } else { out.chars().map(|c| c as u8).collect() });
            }
            false
        });
    }

    /// One scheduled run of a grabber project, in the background.
    pub fn run_project(self: &Arc<Self>, p: GrabProject) {
        if !self.grab_running.lock().unwrap().insert(p.opts.name.clone()) {
            return;
        }
        let core = self.clone();
        thread::spawn(move || {
            let mut files = p.files.clone();
            if p.action != 2 {
                let c = Arc::new(crate::grabber::Crawl::default());
                crate::grabber::run(p.opts.clone(), core.agent(&p.opts.start), c.clone(), core.grab_tuning());
                let st = c.state.lock().unwrap();
                // a periodic project only fetches what is new since its last run
                let old: HashSet<&str> = p.files.iter().map(|f| f.url.as_str()).collect();
                files = st.files.iter().filter(|f| !p.periodic || !old.contains(f.url.as_str())).cloned().collect();
                if let Some(x) = core.settings().grab_projects.iter_mut().find(|x| x.opts.name == p.opts.name) {
                    let mut all = x.files.clone();
                    all.extend(st.files.iter().filter(|f| !old.contains(f.url.as_str())).cloned().map(|mut f| { f.checked = false; f }));
                    (x.files, x.pages) = (all, st.pages);
                }
            }
            if p.action != 0 {
                let ids = core.grab_files(&mut files, &p.opts, p.save_mode, &p.save_cat, &p.folder, true);
                let sc = &p.sched;
                if sc.exit_done || sc.off_done || sc.open_on || sc.hangup {
                    core.grab_watch.lock().unwrap().push((ids, sc.clone()));
                }
            }
            if p.periodic && p.sched.again_on {
                let secs = (p.sched.again_h as u64 * 3600 + p.sched.again_m as u64 * 60).max(60);
                core.grab_again.lock().unwrap().insert(p.opts.name.clone(), Instant::now() + Duration::from_secs(secs));
            }
            core.grab_running.lock().unwrap().remove(&p.opts.name);
            core.save();
        });
    }

    /// Puts the user's proxy back (every exit path comes here), stops the downloads and saves.
    pub fn shutdown(&self) {
        #[cfg(not(target_os = "macos"))]
        self.apply_integration(false);
        #[cfg(target_os = "macos")]
        {
            self.proxy_ready.store(false, std::sync::atomic::Ordering::SeqCst);
            if let Err(error) = crate::mac_capture::restore() {
                eprintln!("NDM: {error}");
            }
        }
        let jobs = self.jobs();
        jobs.iter().for_each(|j| j.stop());
        let until = Instant::now() + Duration::from_secs(5);
        while jobs.iter().any(|j| j.running()) && Instant::now() < until {
            thread::sleep(Duration::from_millis(50));
        }
        self.save();
    }

    pub fn spawn_background(self: &Arc<Self>, notify: impl Fn() + Send + 'static) {
        let core = self.clone();
        thread::spawn(move || {
            let mut last_save = Instant::now();
            loop {
                core.tick_scheduler();
                core.tick_grabber();
                core.tick_quota();
                core.tick_queue();
                core.tick_sounds();
                core.tick_lanes();
                core.tick_limit();
                if last_save.elapsed() > Duration::from_secs(5) {
                    core.save();
                    last_save = Instant::now();
                }
                notify();
                thread::sleep(Duration::from_millis(500));
            }
        });
    }

}
