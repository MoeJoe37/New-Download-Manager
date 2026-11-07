use crate::core::{
    Arc, AtomicU64, BTreeSet, Core, Deserialize, Info, Job, Limiter, Mutex, Path, PathBuf, Queue,
    Relaxed, Serialize, Settings, Status, MAIN_QUEUE,
};
use crate::file_ops::atomic_write;
use std::{fs, io};

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
    let base = std::env::var("APPDATA")
        .or_else(|_| std::env::var("XDG_CONFIG_HOME"))
        .or_else(|_| std::env::var("HOME").map(|h| format!("{h}/.config")))
        .unwrap_or_else(|_| ".".into());
    Path::new(&base)
        .join("NewDownloadManager")
        .join("state.json")
}

impl Core {
    pub fn load() -> Arc<Core> {
        let store = store_path();
        let (saved, notice) = load_saved(&store);
        let core = Core::from_saved(saved, store);
        *core.state_notice.lock().unwrap() = notice;
        core
    }

    #[cfg(test)]
    #[cfg(target_os = "linux")]
    pub(crate) fn for_dialog_snapshots(store: PathBuf, items: Vec<Info>) -> Arc<Self> {
        Self::from_saved(
            Saved {
                items,
                next_id: 4,
                ..Default::default()
            },
            store,
        )
    }

    pub(super) fn from_saved(mut saved: Saved, store: PathBuf) -> Arc<Core> {
        let s = &mut saved.settings;
        #[cfg(target_os = "macos")]
        {
            // The old macOS integration recorded this snapshot without changing system preferences.
            s.saved_proxy = None;
        }
        if s.tray_icon == 1 {
            s.tray_icon = 2;
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
            s.grab_recent = s
                .grab_projects
                .iter()
                .rev()
                .take(4)
                .map(|p| p.opts.name.clone())
                .collect();
        }
        if let Some(old) = s.old_max_active.take() {
            s.queues[0].max_active = old;
        }
        let on_startup: BTreeSet<u32> = s
            .queues
            .iter()
            .filter(|q| q.sched.on_startup)
            .map(|q| q.id)
            .collect();
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
            save_lock: Mutex::default(),
            state_notice: Mutex::default(),
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
        // Take the lock before taking the snapshot, so a delayed save cannot publish older state.
        let mut last_error = self.save_lock.lock().unwrap();
        let saved = Saved {
            settings: self.settings().clone(),
            items: self.jobs().iter().map(|j| j.info().clone()).collect(),
            next_id: self.next_id.load(Relaxed),
        };
        let result = serde_json::to_vec_pretty(&saved)
            .map_err(io::Error::other)
            .and_then(|bytes| persist(&self.store, &bytes));
        match result {
            Ok(()) => *last_error = None,
            Err(error) => {
                let message = error.to_string();
                if last_error.as_ref() != Some(&message) {
                    *self.state_notice.lock().unwrap() = Some(crate::i18n::trf_os(
                        "Could not save the download list. Check the available disk space and folder permissions, then try again.\n%s",
                        &[&message],
                    ));
                }
                *last_error = Some(message);
            }
        }
    }
}

fn read_saved(path: &Path) -> io::Result<Saved> {
    serde_json::from_slice(&fs::read(path)?)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn preserve_unreadable(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut bad = path.as_os_str().to_os_string();
    bad.push(".bad");
    let mut bad = PathBuf::from(bad);
    if bad.exists() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        bad.set_extension(format!("bad.{stamp}"));
    }
    atomic_write(&bad, bytes)
}

fn load_saved(path: &Path) -> (Saved, Option<String>) {
    let backup = path.with_extension("json.bak");
    let primary = read_saved(path);
    if let Ok(saved) = primary {
        return (saved, None);
    }
    for candidate in [path, backup.as_path()] {
        if let Ok(bytes) = fs::read(candidate) {
            if serde_json::from_slice::<Saved>(&bytes).is_err() {
                if let Err(error) = preserve_unreadable(candidate, &bytes) {
                    eprintln!("could not preserve unreadable state: {error}");
                }
            }
        }
    }
    if let Ok(saved) = read_saved(&backup) {
        return (
            saved,
            Some(crate::i18n::tr_os(
                "The download list was recovered from its backup.",
            )),
        );
    }
    let fresh = primary
        .as_ref()
        .err()
        .is_some_and(|e| e.kind() == io::ErrorKind::NotFound)
        && !backup.exists();
    let notice = (!fresh).then(|| crate::i18n::tr_os(
        "The saved download list could not be read. Check state.json and state.json.bak in NDM's settings folder. Unreadable files are preserved before a new list is saved.",
    ));
    (Saved::default(), notice)
}

fn persist(path: &Path, bytes: &[u8]) -> io::Result<()> {
    fs::create_dir_all(path.parent().unwrap_or(Path::new(".")))?;
    let backup = path.with_extension("json.bak");
    let current = match fs::read(path) {
        Ok(current) => Some(current),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    if current.as_deref() == Some(bytes) && read_saved(&backup).is_ok() {
        return Ok(());
    }
    if let Some(current) = current {
        if serde_json::from_slice::<Saved>(&current).is_ok() {
            atomic_write(&backup, &current)?;
        } else {
            preserve_unreadable(path, &current)?;
            if read_saved(&backup).is_err() {
                atomic_write(&backup, bytes)?;
            }
        }
    } else if read_saved(&backup).is_err() {
        atomic_write(&backup, bytes)?;
    }
    atomic_write(path, bytes)
}

#[cfg(test)]
mod persistence_tests {
    use super::{fs, load_saved, persist, read_saved, Core, Info, PathBuf, Saved, Settings};
    use std::sync::{Arc, Barrier};

    fn store(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("ndm-state-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path.join("state.json")
    }

    fn snapshot(name: &str) -> Vec<u8> {
        serde_json::to_vec_pretty(&Saved {
            settings: Settings {
                user_agent: name.into(),
                ..Default::default()
            },
            items: vec![Info {
                id: 42,
                name: "resume.zip".into(),
                etag: "original".into(),
                ..Default::default()
            }],
            next_id: 43,
        })
        .unwrap()
    }

    #[test]
    fn state_recovers_previous_snapshot_and_preserves_corruption() {
        let path = store("recover");
        persist(&path, &snapshot("first")).unwrap();
        persist(&path, &snapshot("second")).unwrap();
        fs::write(&path, b"{interrupted").unwrap();
        let (saved, notice) = load_saved(&path);
        assert!(notice.is_some());
        assert_eq!(saved.settings.user_agent, "first");
        assert_eq!(
            (
                saved.next_id,
                saved.items[0].id,
                saved.items[0].etag.as_str()
            ),
            (43, 42, "original")
        );
        assert_eq!(
            fs::read(path.with_extension("json.bad")).unwrap(),
            b"{interrupted"
        );
        persist(&path, &serde_json::to_vec(&saved).unwrap()).unwrap();
        assert_eq!(read_saved(&path).unwrap().settings.user_agent, "first");
        assert_eq!(
            read_saved(&path.with_extension("json.bak"))
                .unwrap()
                .settings
                .user_agent,
            "first"
        );
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn state_ignores_stale_temporary_files_and_reports_unreadable_lists() {
        let path = store("unreadable");
        fs::write(path.with_extension("tmp"), snapshot("incomplete")).unwrap();
        let (saved, notice) = load_saved(&path);
        assert!(notice.is_none());
        assert!(saved.items.is_empty());
        fs::write(&path, b"invalid primary").unwrap();
        fs::write(path.with_extension("json.bak"), b"invalid backup").unwrap();
        let (_, notice) = load_saved(&path);
        assert!(notice.is_some());
        assert_eq!(
            fs::read(path.with_extension("json.bad")).unwrap(),
            b"invalid primary"
        );
        assert_eq!(
            fs::read(path.with_extension("json.bak.bad")).unwrap(),
            b"invalid backup"
        );
        persist(&path, &snapshot("new")).unwrap();
        assert_eq!(read_saved(&path).unwrap().settings.user_agent, "new");
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn failed_state_publication_keeps_backup_and_cleans_its_temporary_file() {
        let path = store("failure");
        persist(&path, &snapshot("good")).unwrap();
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(persist(&path, &snapshot("replacement")).is_err());
        assert!(crate::file_ops::atomic_write(&path, &snapshot("replacement")).is_err());
        assert_eq!(
            read_saved(&path.with_extension("json.bak"))
                .unwrap()
                .settings
                .user_agent,
            "good"
        );
        assert!(fs::read_dir(path.parent().unwrap()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")
        }));
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn concurrent_state_saves_publish_complete_snapshots() {
        let path = store("concurrent");
        let core = Core::from_saved(Saved::default(), path.clone());
        core.save();
        let barrier = Arc::new(Barrier::new(5));
        let workers: Vec<_> = (0..4)
            .map(|writer| {
                let (core, barrier, path) = (core.clone(), barrier.clone(), path.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    for sequence in 0..8 {
                        core.settings().user_agent = format!("writer-{writer}-{sequence}");
                        core.save();
                        assert!(read_saved(&path).is_ok());
                    }
                })
            })
            .collect();
        barrier.wait();
        for worker in workers {
            worker.join().unwrap();
        }
        assert!(core.state_notice.lock().unwrap().is_none());
        assert!(read_saved(&path).is_ok());
        assert!(read_saved(&path.with_extension("json.bak")).is_ok());
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 2);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}
