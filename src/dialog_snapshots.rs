use super::*;
use std::fmt::Write;

#[test]
#[ignore = "uses global UI settings; run by itself with TZ=UTC"]
fn dialog_snapshots() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let store = std::env::temp_dir().join(format!("ndm-dialog-snapshots-{}", std::process::id()));
    let url = || "https://example.test/file.zip".to_string();
    let items = [
        (1, "stopped.zip", Status::Stopped),
        (2, "failed.zip", Status::Error("Connection closed".into())),
        (3, "complete.zip", Status::Complete),
    ].map(|(id, name, status)| Info {
        id, name: name.into(), status, url: url(), folder: "/downloads".into(), size: Some(1024),
        added: 1_700_000_000, last_try: 1_700_000_100, ..Default::default()
    });
    let core = Core::for_dialog_snapshots(store.join("state.json"), items.to_vec());
    i18n::load("");
    dlu::NDM_STYLE.store(false, Relaxed);

    // Adding a Linux dialog variant requires adding its constructor here for the exhaustive match.
    macro_rules! cases {
        ($($variant:ident : $pattern:pat => $value:expr),+ $(,)?) => {{
            fn name(dialog: &Dialog) -> &'static str {
                match dialog {
                    $($pattern => stringify!($variant),)+
                }
            }
            [$(($value, stringify!($variant)),)+].map(|(dialog, expected)| {
                assert_eq!(name(&dialog), expected);
                (dialog, expected)
            })
        }};
    }

    let cases = cases! {
        AddUrl: Dialog::AddUrl { .. } => Dialog::AddUrl { url: url(), auth: false, login: String::new(), pass: String::new(), clip: false },
        FileInfo: Dialog::FileInfo { .. } => Dialog::FileInfo {
            info: items[0].clone(),
            path: "/downloads/stopped.zip".into(),
            probe: Default::default(),
            applied: false,
            error: String::new(),
            hint: String::new(),
            remember: false,
            replace: None,
            auto: false,
            early: None,
            dir: String::new(),
        },
        Cancelled: Dialog::Cancelled { .. } => Dialog::Cancelled {
            site: "example.test".into(),
            url: url(),
            by_site: false,
            by_addr: false,
            dont_show: false,
        },
        Exceptions: Dialog::Exceptions { .. } => Dialog::Exceptions { list: vec![], sel: None, ask: false },
        ExcAdd: Dialog::ExcAdd { .. } => Dialog::ExcAdd { addr: String::new() },
        BrowserDetails: Dialog::BrowserDetails { .. } => Dialog::BrowserDetails { name: "Firefox".into() },
        Integrate: Dialog::Integrate { .. } => Dialog::Integrate { b: core::Browser { name: "Firefox".into(), ..Default::default() } },
        Keys: Dialog::Keys { .. } => Dialog::Keys { k: Default::default() },
        MenuItems: Dialog::MenuItems { .. } => Dialog::MenuItems { dl: false, all: false },
        Panel: Dialog::Panel { .. } => Dialog::Panel {
            tab: 0,
            types: vec![("MP4".into(), true)],
            min: Default::default(),
            mini: false,
            sel: None,
            nocap: false,
            drm: false,
            smode: 0,
            sites: vec![],
            smini: false,
            ssel: None,
        },
        PanelSite: Dialog::PanelSite { .. } => Dialog::PanelSite { name: String::new() },
        PanelType: Dialog::PanelType { .. } => Dialog::PanelType { name: String::new() },
        PanelMin: Dialog::PanelMin { .. } => Dialog::PanelMin { ext: "MP4".into(), kb: "100".into() },
        PanelExc: Dialog::PanelExc { .. } => Dialog::PanelExc { text: String::new() },
        Progress: Dialog::Progress { .. } => Dialog::Progress { id: 1, tab: 0, details: true, min: false },
        ProgCustom: Dialog::ProgCustom { .. } => Dialog::ProgCustom { view: 0, sl: false, c: false, btns: false },
        Complete: Dialog::Complete { .. } => Dialog::Complete { id: 3, dont_show: false },
        Delete: Dialog::Delete { .. } => Dialog::Delete { ids: vec![1, 2], files: false, dont_show: false },
        Options: Dialog::Options { .. } => Dialog::Options { tab: 0, s: Box::new(core.settings().clone()) },
        Scheduler: Dialog::Scheduler { .. } => Dialog::Scheduler { tab: 0, queue: Default::default(), qsel: None, error: String::new() },
        Duplicate: Dialog::Duplicate { .. } => Dialog::Duplicate {
            inc: Incoming { url: url(), ..Default::default() },
            old: 1,
            choice: 0,
            remember: false,
        },
        FileProps: Dialog::FileProps { .. } => Dialog::FileProps {
            id: 3,
            url: url(),
            desc: String::new(),
            referer: String::new(),
            login: String::new(),
            pass: String::new(),
        },
        Checksum: Dialog::Checksum { .. } => Dialog::Checksum {
            name: "complete.zip".into(),
            run: Arc::new(checksum::Run { done: 512.into(), total: 1024, result: Mutex::new(None) }),
            expect: String::new(),
            source: Default::default(),
        },
        Playlist: Dialog::Playlist (_) => Dialog::Playlist(Box::default()),
        ProblemReport: Dialog::ProblemReport { .. } => Dialog::ProblemReport { text: "Problem report".into() },
        Http1Hosts: Dialog::Http1Hosts { .. } => Dialog::Http1Hosts { hosts: String::new() },
        YtDlp: Dialog::YtDlp { .. } => Dialog::YtDlp { got: Default::default(), total: Default::default(), done: Default::default() },
        Category: Dialog::Category { .. } => Dialog::Category { orig: None, cat: Default::default(), folder: String::new(), error: String::new() },
        CatDelete: Dialog::CatDelete { .. } => Dialog::CatDelete { name: String::new() },
        AdvProxy: Dialog::AdvProxy { .. } => Dialog::AdvProxy { adv: Default::default() },
        Info: Dialog::Info { .. } => Dialog::Info { title: "Example message".into(), text: "This is an example.".into() },
        Networks: Dialog::Networks { .. } => Dialog::Networks { nics: vec![], off: vec![] },
        QuotaHit: Dialog::QuotaHit { .. } => Dialog::QuotaHit { text: "Quota reached".into() },
        QueueName: Dialog::QueueName { .. } => Dialog::QueueName { name: String::new(), files: vec![] },
        Batch: Dialog::Batch { .. } => Dialog::Batch {
            url: String::new(),
            from: 0,
            to: 0,
            width: 0,
            letters: false,
            lf: String::new(),
            lt: String::new(),
            auth: false,
            login: String::new(),
            pass: String::new(),
        },
        QueuePick: Dialog::QueuePick { .. } => Dialog::QueuePick {
            ids: vec![1],
            q: "Main download queue".into(),
            start: false,
            never: false,
            batch: false,
        },
        GrabSave: Dialog::GrabSave { .. } => Dialog::GrabSave { name: String::new(), orig: None, quiet: false, proj: None, sched: false },
        GrabSchedAsk: Dialog::GrabSchedAsk { .. } => Dialog::GrabSchedAsk { proj: Default::default() },
        GrabSched: Dialog::GrabSched { .. } => Dialog::GrabSched { p: Default::default() },
        GrabLogin: Dialog::GrabLogin { .. } => Dialog::GrabLogin {
            realm: "Test realm".into(),
            site: "example.test".into(),
            user: String::new(),
            pass: String::new(),
            save: false,
        },
        Export: Dialog::Export { .. } => Dialog::Export { ef2: false, mode: 0 },
        Virus: Dialog::Virus { .. } => Dialog::Virus { exe: String::new(), args: String::new() },
        WaitLink: Dialog::WaitLink { .. } => Dialog::WaitLink { name: "stopped.zip".into() },
        Help: Dialog::Help { .. } => Dialog::Help { page: "index.htm".into(), anchor: None, back: vec![], fwd: vec![], search: String::new() },
        DialUp: Dialog::DialUp => Dialog::DialUp,
        Grabber: Dialog::Grabber { .. } => Dialog::Grabber {
            step: 0,
            adv: false,
            node: 0,
            opts: Default::default(),
            save_mode: 0,
            save_cat: String::new(),
            folder: String::new(),
            crawl: None,
        },
        GrabStats: Dialog::GrabStats { .. } => Dialog::GrabStats { crawl: Default::default() },
        GrabTpl: Dialog::GrabTpl { .. } => Dialog::GrabTpl { name: String::new(), proj: Default::default(), replace: false },
        GrabLoad: Dialog::GrabLoad { .. } => Dialog::GrabLoad { sel: None },
        GrabSettings: Dialog::GrabSettings { .. } => Dialog::GrabSettings {
            explore: String::new(),
            dl: String::new(),
            link: false,
            iec: false,
            adv: false,
        },
        GrabManual: Dialog::GrabManual { .. } => Dialog::GrabManual { url: url() },
        About: Dialog::About => Dialog::About,
        Scale: Dialog::Scale { .. } => Dialog::Scale { orig: [1.0; 3] },
        TbCustomize: Dialog::TbCustomize { .. } => Dialog::TbCustomize { layout: vec![0, 1, -1, 2], avail: None, cur: None },
        Tip: Dialog::Tip { .. } => Dialog::Tip { text: "Use the clipboard to add a download.".into() },
        Limiter: Dialog::Limiter { .. } => Dialog::Limiter {
            kb: "100".into(),
            startup: false,
            sched: false,
            from: "08:00".into(),
            to: "18:00".into(),
        },
        SpeedLimit: Dialog::SpeedLimit { .. } => Dialog::SpeedLimit { ids: vec![1], on: false, kb: String::new() },
        Columns: Dialog::Columns { .. } => Dialog::Columns { list: vec![(0, true, 160)], sel: 0, width: "160".into() },
        ServerConn: Dialog::ServerConn { .. } => Dialog::ServerConn { orig: None, server: String::new(), n: String::new() },
        Update: Dialog::Update { .. } => Dialog::Update { slot: Default::default(), quiet: false },
        NewVersion: Dialog::NewVersion { .. } => Dialog::NewVersion {
            rel: Release { version: "9.0.0".into(), notes: "Example release".into(), exe: url(), page: url() },
            get: None,
        },
        Find: Dialog::Find { .. } => Dialog::Find { text: String::new(), flags: 0 },
    };

    let mut snapshot = String::new();
    for (mut dialog, name) in cases {
        let ctx = egui::Context::default();
        let mut fonts = egui::FontDefinitions::default();
        for family in ["segoe", "list"] {
            fonts.families.insert(egui::FontFamily::Name(family.into()), fonts.families[&egui::FontFamily::Proportional].clone());
        }
        ctx.set_fonts(fonts);
        apply_style(&ctx, false);
        let (_, rx) = channel();
        let mut app = App::build(&ctx, core.clone(), Arc::default(), rx, None, false);
        let (title, fixed, size) = app.dialog_frame(&dialog);
        writeln!(snapshot, "{name} {:?} {:.1}x{:.1}", i18n::tr_os(&title), size.x, size.y).unwrap();
        let output = ctx.run(egui::RawInput {
            time: Some(0.0),
            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, size)),
            ..Default::default()
        }, |ctx| {
            egui::CentralPanel::default().frame(dialog_panel_frame(ctx, &dialog, fixed)).show(ctx, |ui| {
                app.dialog_body(ctx, ui, &mut dialog);
            });
        });
        for entry in output.shapes {
            write_shape(&mut snapshot, &entry.shape);
        }
        snapshot.push('\n');
    }

    let path = root.join("tools/snapshots/dialogs.txt");
    if std::env::var_os("NDM_BLESS").as_deref() == Some(std::ffi::OsStr::new("1")) {
        std::fs::write(path, snapshot).unwrap();
    } else {
        let expected = std::fs::read_to_string(&path).unwrap();
        if expected != snapshot {
            let line = expected.lines().zip(snapshot.lines()).position(|(a, b)| a != b)
                .unwrap_or_else(|| expected.lines().count().min(snapshot.lines().count()));
            panic!("Dialog snapshot changed at line {}\nexpected {:?}\nactual   {:?}",
                line + 1, expected.lines().nth(line), snapshot.lines().nth(line));
        }
    }
}

fn write_shape(out: &mut String, shape: &egui::Shape) {
    let kind = match shape {
        egui::Shape::Noop => return,
        egui::Shape::Vec(shapes) => {
            for shape in shapes { write_shape(out, shape); }
            return;
        }
        egui::Shape::Text(_) => "text",
        egui::Shape::Rect(_) => "rect",
        egui::Shape::Circle(_) => "circle",
        egui::Shape::Ellipse(_) => "ellipse",
        egui::Shape::LineSegment { .. } => "line",
        egui::Shape::Path(_) => "path",
        egui::Shape::Mesh(_) => "mesh",
        egui::Shape::QuadraticBezier(_) => "quadratic",
        egui::Shape::CubicBezier(_) => "cubic",
        egui::Shape::Callback(_) => "callback",
    };
    let rect = shape.visual_bounding_rect();
    let half = |v: f32| (v * 2.0).round() / 2.0 + 0.0;
    write!(out, "  {kind} {:.1},{:.1} {:.1},{:.1}", half(rect.min.x), half(rect.min.y), half(rect.max.x), half(rect.max.y)).unwrap();
    if let egui::Shape::Text(text) = shape {
        write!(out, " {:?}", text.galley.text()).unwrap();
    }
    out.push('\n');
}
