use super::*;

#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
pub(super) struct Saved {
    pub(super) settings: Settings,
    pub(super) items: Vec<Info>,
    pub(super) next_id: u64,
}

pub fn store_path() -> PathBuf {
    // %APPDATA% on Windows, ~/Library/Application Support on macOS, ~/.config elsewhere
    #[cfg(target_os = "macos")]
    if let Some(home) = std::env::var_os("HOME") {
        return Path::new(&home).join("Library/Application Support/NewDownloadManager/state.json");
    }
    let base = std::env::var("APPDATA").or_else(|_| std::env::var("XDG_CONFIG_HOME")).or_else(|_| std::env::var("HOME").map(|h| format!("{h}/.config"))).unwrap_or_else(|_| ".".into());
    Path::new(&base).join("NewDownloadManager").join("state.json")
}

impl Core {
    pub fn load() -> Arc<Core> {
        let store = store_path();
        let text = std::fs::read_to_string(&store).ok();
        let saved: Saved = text.as_deref().map_or(Ok(Saved::default()), serde_json::from_str).unwrap_or_else(|e| {
            // keep the unreadable list instead of letting autosave overwrite it with an empty one
            eprintln!("state.json unreadable ({e}), kept as state.json.bad");
            let _ = std::fs::copy(&store, store.with_extension("json.bad"));
            Saved::default()
        });
        Core::from_saved(saved, store)
    }

    #[cfg(test)]
    #[cfg(target_os = "linux")]
    pub(crate) fn for_dialog_snapshots(store: PathBuf, items: Vec<Info>) -> Arc<Self> {
        Self::from_saved(Saved { items, next_id: 4, ..Default::default() }, store)
    }

    pub(super) fn from_saved(mut saved: Saved, store: PathBuf) -> Arc<Core> {
        let s = &mut saved.settings;
        #[cfg(target_os = "macos")]
        {
            // The old macOS integration recorded this snapshot without changing system preferences.
            s.saved_proxy = None;
        }
        s.limit_on |= s.limit_startup;
        if !s.queues.iter().any(|q| q.id == MAIN_QUEUE) {
            s.queues.insert(0, Queue::default());
        }
        if !s.queues.iter().any(|q| q.fixed) {
            let id = s.queues.iter().map(|q| q.id).max().unwrap_or(0) + 1;
            s.queues.insert(1, Queue::sync_default(id));
        }
        if let Some(old) = s.old_sched.take() {
            s.queues[0].sched = old;
        }
        if s.grab_recent.is_empty() {
            // lists saved before Recent Projects was kept: the newest saved projects
            s.grab_recent = s.grab_projects.iter().rev().take(4).map(|p| p.opts.name.clone()).collect();
        }
        if let Some(old) = s.old_max_active.take() {
            s.queues[0].max_active = old;
        }
        let on_startup: BTreeSet<u32> = s.queues.iter().filter(|q| q.sched.on_startup).map(|q| q.id).collect();
        let jobs = saved
            .items
            .into_iter()
            .map(|mut i| {
                if matches!(i.status, Status::Connecting | Status::Downloading) {
                    i.status = Status::Stopped;
                }
                Job::new(i)
            })
            .collect();
        let core = Arc::new(Core {
            jobs: Mutex::new(jobs),
            agents: Mutex::default(),
            #[cfg(target_os = "macos")]
            proxy_ready: Default::default(),
            settings: Mutex::new(saved.settings),
            running: Mutex::new(on_startup),
            queue_finished: Mutex::default(),
            quota: Mutex::default(),
            quota_msg: Mutex::default(),
            limiter: Arc::new(Limiter::default()),
            next_id: AtomicU64::new(saved.next_id.max(1)),
            fired: Mutex::default(),
            limit_window: Mutex::default(),
            fed: Mutex::default(),
            synced: Mutex::default(),
            again: Mutex::default(),
            checking: Arc::default(),
            store,
            grab_fired: Mutex::default(),
            grab_again: Mutex::default(),
            grab_running: Mutex::default(),
            grab_watch: Mutex::default(),
            grab_convert: Mutex::default(),
            grab_dl_ids: Mutex::default(),
            login_wait: Mutex::default(),
            login_cookies: Mutex::default(),
            sched_done: Mutex::default(),
            dial_lock: Mutex::default(),
            dial_text: Mutex::default(),
            dial_cancel: Default::default(),
            dial_now: Default::default(),
        });
        core.apply_settings();
        core
    }

    pub fn save(&self) {
        let saved = Saved {
            settings: self.settings().clone(),
            items: self.jobs().iter().map(|j| j.info().clone()).collect(),
            next_id: self.next_id.load(Relaxed),
        };
        let Ok(json) = serde_json::to_string_pretty(&saved) else { return };
        let tmp = self.store.with_extension("tmp");
        let r = std::fs::create_dir_all(self.store.parent().unwrap())
            .and_then(|_| std::fs::write(&tmp, json))
            .and_then(|_| std::fs::rename(&tmp, &self.store));
        if let Err(e) = r {
            eprintln!("save failed: {e}");
        }
    }
}
