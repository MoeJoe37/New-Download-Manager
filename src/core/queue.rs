use crate::core::{Arc, Core, Deserialize, Duration, Instant, Job, Local, Relaxed, Serialize, Settings, Status, engine, thread};

#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Sched {
    pub start_on: bool,
    pub start_at: String,
    pub daily: bool,
    /// Days the daily schedule runs, Sunday first.
    pub days: [bool; 7],
    pub stop_on: bool,
    pub stop_at: String,
    pub on_startup: bool,
    pub exit_done: bool,
    pub off_done: bool,
    /// "Number of retries for each file if downloading failed" (template 306, edit 1681).
    pub retry_on: bool,
    pub retries: u32,
    /// Synchronization queues: "Start again every" hours/min after a pass.
    pub again_on: bool,
    pub again_h: u32,
    pub again_m: u32,
    /// "Open the following file when done".
    pub open_on: bool,
    pub open_file: String,
    /// "Turn off computer when done": 0 Shut down, 1 Sleep, 2 Hibernate; "Force processes to terminate".
    pub off_kind: u8,
    pub off_force: bool,
    /// "Hang up modem when done" (Dial Up / VPN).
    pub hangup: bool,
}

impl Default for Sched {
    fn default() -> Self {
        // NDM's defaults: 11:00 PM / 7:30 AM, 10 retries, start again every 2 hours
        Sched {
            start_on: false,
            start_at: "23:00".into(),
            daily: true,
            days: [true; 7],
            stop_on: false,
            stop_at: "07:30".into(),
            on_startup: false,
            exit_done: false,
            off_done: false,
            retry_on: false,
            retries: 10,
            again_on: false,
            again_h: 2,
            again_m: 0,
            open_on: false,
            open_file: String::new(),
            off_kind: 0,
            off_force: false,
            hangup: false,
        }
    }
}

/// A download queue. Each has its own schedule and "files at the same time".
#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Queue {
    pub id: u32,
    pub name: String,
    pub sched: Sched,
    pub max_active: usize,
    /// "Periodic synchronization": finished files stay in the queue and
    /// each pass downloads again those that changed on the server.
    pub sync: bool,
    /// NDM's built-in Synchronization queue: can't be deleted.
    pub fixed: bool,
    /// Each of the queue's downloads at most this fast while the queue runs (KB/s, 0: no limit), on top of
    /// its own limit and the Speed Limiter: with the queue's start and stop times, a limit by time of day.
    pub limit_kb: u64,
}

impl Default for Queue {
    fn default() -> Self {
        Queue { id: MAIN_QUEUE, name: "Main download queue".into(), sched: Sched::default(), max_active: 3, sync: false, fixed: false, limit_kb: 0 }
    }
}

impl Queue {
    /// NDM's built-in second queue.
    pub(super) fn sync_default(id: u32) -> Queue {
        Queue { id, name: "Synchronization queue".into(), sync: true, fixed: true, ..Default::default() }
    }

    /// Main and synchronization queues are built in: they can't be deleted or change type.
    pub fn builtin(&self) -> bool {
        self.id == MAIN_QUEUE || self.fixed
    }
}

/// NDM's "Main download queue": always there, can't be deleted.
pub const MAIN_QUEUE: u32 = 0;

/// Download-limits bookkeeping: the window start, the bytes counter there, and a stop in force.
#[derive(Default)]
pub struct Quota {
    pub(super) start: Option<(chrono::DateTime<Local>, u64)>,
    /// Downloads stopped by the quota and when they resume.
    pub stopped: Option<(chrono::DateTime<Local>, Vec<u64>)>,
}

/// NDM stops at 98% of the limit: m_MBytes * 1 MiB / 100 * 98 (0x49d4d0).
pub fn quota_bytes(mb: u64) -> u64 {
    mb * 1024 * 1024 / 100 * 98
}

/// Whether `t` is in [from, to); a window that ends before it starts runs past midnight.
pub fn in_window(t: chrono::NaiveTime, from: chrono::NaiveTime, to: chrono::NaiveTime) -> bool {
    if from <= to { from <= t && t < to } else { t >= from || t < to }
}

/// A queued download's rate in bytes/s: its own limit and its queue's, the lower one (KB/s, 0: none).
pub(super) fn queue_rate(own_kb: u64, queue_kb: u64) -> u64 {
    [own_kb, queue_kb].into_iter().filter(|rate| *rate > 0).min().unwrap_or(0).saturating_mul(1024)
}

#[test]
fn queue_speed_limit() {
    assert_eq!(queue_rate(0, 0), 0);
    assert_eq!(queue_rate(100, 0), 100 * 1024);
    assert_eq!(queue_rate(0, 50), 50 * 1024);
    assert_eq!(queue_rate(100, 50), 50 * 1024);
    assert_eq!(queue_rate(20, 50), 20 * 1024);
}

impl Settings {
    pub fn queue(&self, id: u32) -> Option<&Queue> {
        self.queues.iter().find(|q| q.id == id)
    }
}

impl Core {
    pub fn set_job_limit(&self, job: &Job, kb: u64) {
        job.info().limit_kb = kb;
        self.update_job_limit(job);
    }

    pub(super) fn update_job_limit(&self, job: &Job) {
        let (own, queued, queue) = {
            let info = job.info();
            (info.limit_kb, info.queued, info.queue)
        };
        let queue_limit = if queued && self.queue_running(queue) {
            self.settings().queue(queue).map_or(0, |q| q.limit_kb)
        } else {
            0
        };
        job.limiter.rate.store(queue_rate(own, queue_limit), Relaxed);
    }

    pub fn queue_running(&self, q: u32) -> bool {
        self.running.lock().unwrap().contains(&q)
    }

    /// Play the sound chosen for event `ev` (index into `SOUND_EVENTS`), if any.
    pub fn sound(&self, ev: usize) {
        let (on, path) = self.settings().sounds[ev].clone();
        if on {
            crate::win::play_sound(&path);
        }
    }

    pub fn start_queue(&self, q: u32) {
        if self.running.lock().unwrap().insert(q) {
            self.synced.lock().unwrap().retain(|_, x| *x != q); // a new pass checks every file again
            self.sound(2);
        }
    }

    pub fn stop_queue(&self, q: u32) {
        if self.running.lock().unwrap().remove(&q) {
            self.sound(3);
        }
        for j in self.jobs() {
            let i = j.info();
            if i.queued && i.queue == q {
                j.stop();
            }
        }
    }

    /// "Stop All": every queue and every download.
    pub fn stop_all(&self) {
        self.running.lock().unwrap().clear();
        self.jobs().iter().for_each(|j| j.stop());
    }

    /// Scheduler "New queue": NDM names it "Queue # <id>" before the user renames it.
    pub fn add_queue(&self, name: &str) -> u32 {
        let mut s = self.settings();
        let id = s.queues.iter().map(|q| q.id).max().unwrap_or(0) + 1;
        let name = if name.trim().is_empty() { format!("Queue # {id}") } else { name.trim().to_string() };
        s.queues.push(Queue { id, name, ..Default::default() });
        id
    }

    /// Delete a queue; its files stay in the list but leave the queue. The main queue can't be deleted.
    pub fn delete_queue(&self, q: u32) {
        if self.settings().queue(q).is_none_or(Queue::builtin) {
            return;
        }
        self.stop_queue(q);
        for j in self.jobs() {
            let mut i = j.info();
            if i.queue == q {
                (i.queued, i.queue) = (false, MAIN_QUEUE);
            }
        }
        self.settings().queues.retain(|x| x.id != q);
    }

    /// Put downloads in queue `q`, or take them out of their queue (None).
    pub fn set_queue(&self, ids: &[u64], q: Option<u32>) {
        let sync = q.is_some_and(|q| self.settings().queue(q).is_some_and(|x| x.sync));
        for j in ids.iter().filter_map(|id| self.find(*id)) {
            let mut i = j.info();
            i.queued = q.is_some() && (sync || i.status != Status::Complete);
            i.queue = q.unwrap_or(MAIN_QUEUE);
            drop(i);
            self.update_job_limit(&j);
        }
    }

    /// Options -> Connection "Download limits". The window starts with the first counted byte and lasts
    /// `quota_hours`; reaching 98% of the quota stops every download until the window ends, then the
    /// stopped downloads resume.
    pub(super) fn tick_quota(self: &Arc<Self>) {
        let (on, mb, hours, warn) = {
            let s = self.settings();
            (s.quota_on, s.quota_mb, s.quota_hours.clamp(1, 23), s.quota_warn)
        };
        let now = Local::now();
        let got = self.limiter.received.load(Relaxed);
        let mut q = self.quota.lock().unwrap();
        if !on {
            // turning the limit off lifts a stop in force, like NDM's "change settings... and press Resume"
            q.start = None;
            q.stopped = None;
            return;
        }
        let (start, base) = *q.start.get_or_insert((now, got));
        let end = start + chrono::Duration::hours(hours as i64);
        if now >= end {
            q.start = Some((now, got));
            if let Some((_, ids)) = q.stopped.take() {
                drop(q);
                ids.iter().filter_map(|id| self.find(*id)).for_each(|j| self.start(&j));
            }
            return;
        }
        if q.stopped.is_some() {
            // keep everything stopped until the window ends (a manual resume is stopped again)
            self.jobs().iter().filter(|j| j.running()).for_each(|j| j.stop());
            return;
        }
        if got.saturating_sub(base) >= quota_bytes(mb) {
            let ids: Vec<u64> = self.jobs().iter().filter(|j| j.running()).map(|j| j.info().id).collect();
            ids.iter().filter_map(|id| self.find(*id)).for_each(|j| j.stop());
            q.stopped = Some((end, ids));
            if warn {
                let hm = |t: chrono::DateTime<Local>| t.format("%H:%M").to_string();
                // the message window wraps it: reading order
                *self.quota_msg.lock().unwrap() = Some(crate::i18n::trf_os(
                    "From %s to %s you downloaded %s MB. All downloads have been stopped at %s because you had exceeded your download limits set in 'Options->Connection'!\r\n\r\nAll stopped downloads will be resumed automatically at %s. If you want to resume downloads immediately, change settings in Download Limits and press on Resume.",
                    &[&hm(start), &hm(now), &((got - base) / (1024 * 1024)), &hm(now), &hm(end)],
                ));
            }
        }
    }

    pub fn quota_blocked(&self) -> bool {
        self.quota.lock().unwrap().stopped.is_some()
    }

    pub(super) fn tick_queue(self: &Arc<Self>) {
        if self.quota_blocked() {
            return;
        }
        let running: Vec<u32> = self.running.lock().unwrap().iter().copied().collect();
        let jobs = self.jobs();
        for q in running {
            let (max, retries, sync, again, limit) = self.settings().queue(q).map_or((1, 0, false, None, 0), |x| {
                let again = Some(x.sched.again_h as u64 * 3600 + x.sched.again_m as u64 * 60).filter(|s| x.sync && x.sched.again_on && *s > 0);
                (x.max_active.max(1), if x.sched.retry_on { x.sched.retries } else { 0 }, x.sync, again, x.limit_kb)
            });
            let mine: Vec<&Arc<Job>> = jobs.iter().filter(|j| {
                let i = j.info();
                i.queued && i.queue == q
            }).collect();
            for j in &mine {
                let info = j.info();
                if info.queued && info.queue == q {
                    j.limiter.rate.store(queue_rate(info.limit_kb, limit), Relaxed);
                }
            }
            let mut active = mine.iter().filter(|j| j.running()).count();
            let mut pending = false;
            for j in &mine {
                let status = j.info().status.clone();
                // failed after the queue's retries: the user decides
                let failed = matches!(&status, Status::Error(error) if engine::range::permanent_error(error) || j.fails.load(Relaxed) > retries);
                let id = j.info().id;
                if status == Status::Complete && sync && !self.synced.lock().unwrap().contains_key(&id) {
                    // synchronization: ask the server whether the file changed (size or date); if so it is
                    // downloaded again on a later tick
                    self.synced.lock().unwrap().insert(id, q);
                    self.checking.lock().unwrap().insert(id);
                    let (j, agent, checking) = ((*j).clone(), self.agent(&j.info().url), self.checking.clone());
                    thread::spawn(move || {
                        let (url, referrer, cookies) = {
                            let i = j.info();
                            (i.url.clone(), i.referrer.clone(), i.cookies.clone())
                        };
                        if let Ok(p) = engine::probe::probe(&agent, &url, &referrer, &cookies) {
                            let i = j.info().clone();
                            if p.size != i.size || (!p.etag.is_empty() && !i.etag.is_empty() && p.etag != i.etag) || (!p.modified.is_empty() && p.modified != i.modified) || !i.path().exists() {
                                j.reset();
                            }
                        }
                        checking.lock().unwrap().remove(&id);
                    });
                }
                if self.checking.lock().unwrap().contains(&id) {
                    pending = true;
                    continue;
                }
                if j.running() || failed || status == Status::Complete {
                    continue;
                }
                // a failed one waits before its next try (it still counts as pending)
                let waiting = matches!(status, Status::Error(_)) && j.failed_at.lock().unwrap().is_some_and(|t| t.elapsed() < engine::retry_wait(j.fails.load(Relaxed)));
                if waiting {
                    pending = true;
                    continue;
                }
                pending = true;
                if active < max {
                    self.start(j);
                    active += 1;
                }
            }
            if !pending && active == 0 {
                if let Some(secs) = again {
                    self.again.lock().unwrap().insert(q, Instant::now() + Duration::from_secs(secs));
                }
                self.running.lock().unwrap().remove(&q);
                self.queue_finished.lock().unwrap().push(q);
                self.sound(3);
            }
        }
    }

    /// Download complete / failed sounds; NDM plays them where it finishes a download (0x4725c0).
    pub(super) fn tick_sounds(&self) {
        for j in self.jobs() {
            if j.ended.swap(false, Relaxed) {
                self.sound(if j.info().status == Status::Complete { 0 } else { 1 });
            }
        }
    }

    /// The Speed Limiter's hours: on when the window opens, off when it closes.
    pub(super) fn tick_limit(&self) {
        let (on, from, to) = {
            let s = self.settings();
            (s.limit_sched, s.limit_from.clone(), s.limit_to.clone())
        };
        let parse = |t: &str| chrono::NaiveTime::parse_from_str(t.trim(), "%H:%M").ok();
        let (Some(a), Some(b), true) = (parse(&from), parse(&to), on) else {
            *self.limit_window.lock().unwrap() = None;
            return;
        };
        let inside = in_window(Local::now().time(), a, b);
        let mut last = self.limit_window.lock().unwrap();
        if *last != Some(inside) {
            *last = Some(inside);
            drop(last);
            self.settings().limit_on = inside;
            self.apply_settings();
        }
    }

    pub(super) fn tick_scheduler(&self) {
        let now = Local::now();
        let (hm, today) = (now.format("%H:%M").to_string(), now.date_naive());
        use chrono::Datelike;
        let (mut start, mut stop) = (vec![], vec![]);
        {
            let mut s = self.settings();
            let mut fired = self.fired.lock().unwrap();
            for q in &mut s.queues {
                let f = fired.entry(q.id).or_default();
                let sc = &mut q.sched;
                let day_ok = !sc.daily || sc.days[now.weekday().num_days_from_sunday() as usize];
                if sc.start_on && day_ok && hm == sc.start_at && f.0 != Some(today) {
                    f.0 = Some(today);
                    sc.start_on = sc.daily;
                    start.push(q.id);
                }
                if sc.stop_on && hm == sc.stop_at && f.1 != Some(today) {
                    f.1 = Some(today);
                    sc.stop_on = sc.daily;
                    stop.push(q.id);
                }
            }
        }
        let due: Vec<u32> = {
            let mut again = self.again.lock().unwrap();
            let due = again.iter().filter(|(_, t)| **t <= Instant::now()).map(|(q, _)| *q).collect::<Vec<_>>();
            due.iter().for_each(|q| _ = again.remove(q));
            due
        };
        start.into_iter().chain(due).for_each(|q| self.start_queue(q));
        stop.into_iter().for_each(|q| self.stop_queue(q));
    }
}
