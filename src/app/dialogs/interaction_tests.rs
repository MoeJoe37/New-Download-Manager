use super::{App, Dialog};
use crate::app::resources::apply_style;
use crate::app::{dlu, egui, i18n, Core, Incoming, Info, Status};
use egui::{Event, Modifiers, PointerButton, Pos2, Rect, Vec2};
use std::collections::HashMap;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::{mpsc::channel, Arc, Mutex};
use std::time::{Duration, Instant};

struct Desktop {
    app: App,
    ctx: egui::Context,
    time: f64,
}

impl Desktop {
    fn new(core: Arc<Core>, scale: f32) -> Self {
        let ctx = egui::Context::default();
        ctx.set_pixels_per_point(scale);
        let mut fonts = egui::FontDefinitions::default();
        for family in ["segoe", "list"] {
            fonts.families.insert(
                egui::FontFamily::Name(family.into()),
                fonts.families[&egui::FontFamily::Proportional].clone(),
            );
        }
        ctx.set_fonts(fonts);
        apply_style(&ctx, false);
        let (_, rx) = channel();
        let app = App::build(&ctx, core, Arc::default(), rx, None, false);
        Self {
            app,
            ctx,
            time: 0.0,
        }
    }

    fn frame(&mut self, dialog: &mut Dialog, events: Vec<Event>) -> (HashMap<String, Rect>, bool) {
        self.time += 1.0 / 60.0;
        let mut alive = true;
        let output = self.ctx.run(
            egui::RawInput {
                time: Some(self.time),
                events,
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(760.0, 520.0))),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    alive = self.app.dialog_body(ctx, ui, dialog);
                });
            },
        );
        fn labels(shape: &egui::Shape, out: &mut HashMap<String, Rect>) {
            match shape {
                egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| labels(s, out)),
                egui::Shape::Text(text) => {
                    out.insert(
                        text.galley.text().to_string(),
                        text.galley.rect.translate(text.pos.to_vec2()),
                    );
                }
                _ => {}
            }
        }
        let mut text = HashMap::new();
        for shape in output.shapes {
            labels(&shape.shape, &mut text);
        }
        (text, alive)
    }

    fn click_at(&mut self, dialog: &mut Dialog, pos: Pos2) -> bool {
        self.frame(
            dialog,
            vec![
                Event::PointerMoved(pos),
                Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed: true,
                    modifiers: Modifiers::NONE,
                },
            ],
        );
        self.frame(
            dialog,
            vec![Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::NONE,
            }],
        )
        .1
    }

    fn click(&mut self, dialog: &mut Dialog, label: &str) -> bool {
        let (labels, _) = self.frame(dialog, vec![]);
        let translated = i18n::tr(label);
        let rect = labels
            .get(&translated)
            .unwrap_or_else(|| panic!("Missing {label:?}: {:?}", labels.keys()));
        self.click_at(dialog, rect.center())
    }
}

fn file_info(core: &Core, folder: &std::path::Path, url: String, video: bool) -> Dialog {
    Dialog::FileInfo {
        info: Info {
            name: "fixture.mp4".into(),
            folder: folder.display().to_string(),
            ytdlp: video,
            probed: true,
            ..core.new_info(&url)
        },
        path: folder.join("fixture.mp4").display().to_string(),
        probe: Arc::new(Mutex::new(None)),
        applied: true,
        error: String::new(),
        hint: String::new(),
        remember: false,
        replace: None,
        auto: false,
        early: None,
        dir: String::new(),
    }
}

#[test]
#[ignore = "uses global UI settings; run alone with TZ=UTC"]
fn desktop_interactions() {
    let root = std::env::temp_dir().join(format!("ndm-ui-interactions-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    dlu::NDM_STYLE.store(false, Relaxed);
    for (language, scale) in [("", 1.0), ("", 2.0), ("ndm_ar", 1.0), ("ndm_ar", 2.0)] {
        i18n::load(language);
        assert_eq!(i18n::is_rtl(), language == "ndm_ar");
        let core = Core::for_dialog_snapshots(root.join("state.json"), vec![]);
        core.settings().base_dir = root.display().to_string();
        let mut desktop = Desktop::new(core.clone(), scale);
        let mut dialog = file_info(&core, &root, "https://example.test/video".into(), true);
        desktop.frame(&mut dialog, vec![]);
        desktop.click(&mut dialog, "Best");
        assert!(desktop.ctx.memory(|m| m.any_popup_open()));
        desktop.click(&mut dialog, "720p");
        assert!(
            !desktop.ctx.memory(|m| m.any_popup_open()),
            "quality menu stayed open"
        );
        assert!(matches!(&dialog, Dialog::FileInfo { info, .. } if info.yt.quality == "720"));
        desktop.click(&mut dialog, "720p");
        desktop.frame(
            &mut dialog,
            vec![Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            }],
        );
        assert!(!desktop.ctx.memory(|m| m.any_popup_open()));
        desktop.frame(
            &mut dialog,
            vec![Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: false,
                repeat: false,
                modifiers: Modifiers::NONE,
            }],
        );
        desktop.click(&mut dialog, "720p");
        desktop.click(&mut dialog, "Audio only (MP3)");
        assert!(
            !desktop.ctx.memory(|m| m.any_popup_open()),
            "format menu stayed open"
        );
        assert!(
            matches!(&dialog, Dialog::FileInfo { info, path, .. } if info.yt.quality == "mp3" && path.ends_with(".mp3"))
        );
        assert!(!desktop.click(&mut dialog, "Cancel"));
        assert!(core.jobs().is_empty());

        let mut dialog = file_info(&core, &root, "https://example.test/video".into(), true);
        desktop.click(&mut dialog, "Best");
        desktop.click(&mut dialog, "720p");
        assert!(!desktop.click(&mut dialog, "Download Later"));
        let job = core.jobs()[0].clone();
        assert_eq!(job.info().yt.quality, "720");
        assert_eq!(job.info().status, Status::Stopped);
        assert!(!job.running());
        core.settings().dupl_ask = true;
        desktop.app.incoming(
            &desktop.ctx,
            Incoming {
                url: "https://example.test/video".into(),
                force: true,
                ..Default::default()
            },
        );
        let index = desktop
            .app
            .dialogs
            .iter()
            .position(|(_, d)| matches!(d, Dialog::Duplicate { .. }))
            .unwrap();
        let (_, mut duplicate) = desktop.app.dialogs.remove(index);
        assert!(!desktop.click(&mut duplicate, "Cancel"));
        assert_eq!(core.jobs().len(), 1);
    }

    i18n::load("");
    let core = Core::for_dialog_snapshots(root.join("transfer/state.json"), vec![]);
    core.settings().base_dir = root.display().to_string();
    let mut desktop = Desktop::new(core.clone(), 1.0);
    let mut add = Dialog::AddUrl {
        url: "https://example.test/file".into(),
        auth: true,
        login: String::new(),
        pass: String::new(),
        clip: false,
    };
    desktop.frame(&mut add, vec![]);
    desktop.click_at(
        &mut add,
        Pos2::new(8.0 + 100.0 * dlu::DX, 8.0 + 40.0 * dlu::DY),
    );
    desktop.frame(&mut add, vec![Event::Text("alice".into())]);
    assert!(
        matches!(&add, Dialog::AddUrl { login, .. } if login == "alice"),
        "URL field stole login focus"
    );
    desktop.frame(
        &mut add,
        vec![Event::Key {
            key: egui::Key::Tab,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        }],
    );
    desktop.frame(&mut add, vec![Event::Text("secret".into())]);
    assert!(
        matches!(&add, Dialog::AddUrl { pass, .. } if pass == "secret"),
        "Tab did not move focus to the password"
    );
    assert!(!desktop.click(&mut add, "Cancel"));

    let bytes = Arc::new(vec![42; 128 * 1024]);
    let base = crate::test_support::serve(bytes.clone());
    let mut dialog = file_info(&core, &root, format!("{base}r/fixture"), false);
    if let Dialog::FileInfo { info, path, .. } = &mut dialog {
        info.name = "transfer.bin".into();
        info.size = Some(bytes.len() as u64);
        info.ranges = true;
        info.conns = 1;
        *path = root.join("transfer.bin").display().to_string();
    }
    assert!(!desktop.click(&mut dialog, "Start Download"));
    let job = core.jobs()[0].clone();
    let deadline = Instant::now() + Duration::from_secs(10);
    while job.running() {
        assert!(Instant::now() < deadline, "download did not finish");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(job.info().status, Status::Complete);
    assert_eq!(std::fs::read(job.info().path()).unwrap(), *bytes);
    let cancelled = core.add(Info {
        name: "cancel.bin".into(),
        folder: root.display().to_string(),
        size: Some(bytes.len() as u64),
        ranges: true,
        probed: true,
        conns: 1,
        limit_kb: 1,
        ..core.new_info(&format!("{base}r/cancel"))
    });
    core.start(&cancelled);
    let mut progress = Dialog::Progress {
        id: cancelled.info().id,
        tab: 0,
        details: false,
        min: false,
    };
    assert!(!desktop.click(&mut progress, "Cancel"));
    assert!(cancelled.stopping(), "Cancel did not reach the active job");
    let deadline = Instant::now() + Duration::from_secs(5);
    while cancelled.running() {
        assert!(
            Instant::now() < deadline,
            "cancelled transfer did not stop: {:?}",
            cancelled.info()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(cancelled.info().status, Status::Stopped);
    assert!(!cancelled.info().path().exists());
    let recoverable = core.add(Info {
        name: "protected.bin".into(),
        folder: root.display().to_string(),
        status: Status::Error("HTTP 401: authentication required".into()),
        yt: crate::engine::YtOpts {
            quality: "720".into(),
            ..Default::default()
        },
        segs: vec![crate::engine::Seg {
            next: 7,
            stop: Some(100),
            ..Default::default()
        }],
        ..core.new_info("https://example.test/protected.bin")
    });
    let recovery_id = recoverable.info().id;
    let part = recoverable.info().part();
    std::fs::write(&part, b"partial").unwrap();
    let mut failed_progress = Dialog::Progress {
        id: recovery_id,
        tab: 0,
        details: false,
        min: false,
    };
    assert!(desktop.click(&mut failed_progress, "Recover..."));
    let index = desktop
        .app
        .dialogs
        .iter()
        .position(|(_, d)| matches!(d, Dialog::Recovery { id } if *id == recovery_id))
        .unwrap();
    let (_, mut recovery) = desktop.app.dialogs.remove(index);
    assert!(!desktop.click(&mut recovery, "Edit credentials"));
    let index = desktop
        .app
        .dialogs
        .iter()
        .position(|(_, d)| matches!(d, Dialog::FileProps { id, .. } if *id == recovery_id))
        .unwrap();
    let (_, mut properties) = desktop.app.dialogs.remove(index);
    if let Dialog::FileProps { login, pass, .. } = &mut properties {
        *login = "new-user".into();
        *pass = "new-password".into();
    }
    assert!(!desktop.click(&mut properties, "OK"));
    assert!(recoverable.info().url.contains("new-user:new-password@"));
    assert_eq!(recoverable.info().yt.quality, "720");
    assert_eq!(recoverable.info().done(), 7);
    assert_eq!(std::fs::read(&part).unwrap(), b"partial");
    recoverable.info().status = Status::Error(crate::engine::CHANGED.into());
    let mut recovery = Dialog::Recovery { id: recovery_id };
    let (labels, _) = desktop.frame(&mut recovery, vec![]);
    assert!(labels.contains_key("Restart download"));
    assert!(!labels.contains_key("Retry"));
    assert!(!desktop.click(&mut recovery, "Cancel"));
    assert_eq!(std::fs::read(&part).unwrap(), b"partial");
    recoverable.info().status = Status::Error("HTTP 403: expired address".into());
    let mut recovery = Dialog::Recovery { id: recovery_id };
    assert!(!desktop.click(&mut recovery, "Refresh download address"));
    assert_eq!(desktop.app.refreshing, Some(recovery_id));
    assert_eq!(recoverable.info().yt.quality, "720");
    assert_eq!(std::fs::read(&part).unwrap(), b"partial");
    std::fs::remove_dir_all(root).unwrap();
}
