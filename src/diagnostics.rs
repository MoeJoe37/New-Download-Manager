use crate::core;
use std::path::Path;

/// A problem report's title and text for GitHub (in English, for NDM's developers): what to fill in, NDM's
/// version, the system, and the language.
pub(crate) fn issue_text(version: &str, lang: &str) -> (String, String) {
    let lang = if lang.is_empty() {
        "en (English)".to_string()
    } else {
        lang.to_string()
    };
    let text = format!(
        "### What happened?\n\n\n### What did you expect?\n\n\n### Steps to reproduce\n1. \n\n### System\n- NDM {version}\n- {} {}\n- Language: {lang}\n",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    (format!("[{version}] "), text)
}

const CRASH_MODULES: &[&str] = &[
    "app",
    "app::browser_setup",
    "app::dialogs",
    "app::dialogs::browser",
    "app::dialogs::checksum",
    "app::dialogs::customize",
    "app::dialogs::download",
    "app::dialogs::grabber",
    "app::dialogs::grabber_projects",
    "app::dialogs::grabber_schedule",
    "app::dialogs::grabber_session",
    "app::dialogs::limits",
    "app::dialogs::mac",
    "app::dialogs::misc",
    "app::dialogs::options",
    "app::dialogs::options::network",
    "app::dialogs::options::pages",
    "app::dialogs::playlist",
    "app::dialogs::progress",
    "app::dialogs::report",
    "app::dialogs::recovery",
    "app::dialogs::scheduler",
    "app::display",
    "app::download_list",
    "app::downloads",
    "app::events",
    "app::grabber_window",
    "app::help",
    "app::resources",
    "app::shell",
    "app::window",
    "app::window::categories",
    "app::window::menu",
    "app::window::table",
    "app::window::toolbar",
    "browsers",
    "checksum",
    "core",
    "core::browser_bridge",
    "core::queue",
    "core::routing",
    "core::settings",
    "core::state",
    "diagnostics",
    "dlu",
    "dlu::widgets",
    "engine",
    "engine::ftp",
    "engine::job",
    "engine::names",
    "engine::recovery",
    "engine::probe",
    "engine::range",
    "file_ops",
    "grabber",
    "i18n",
    "linux",
    "mac",
    "mac_capture",
    "main",
    "media",
    "net",
    "platform::linux::desktop",
    "platform::linux::drag_drop",
    "platform::linux::network",
    "platform::macos::desktop",
    "platform::macos::drag_drop",
    "platform::macos::network",
    "platform::windows::desktop",
    "platform::windows::drag_drop",
    "platform::windows::network",
    "posix",
    "remux",
    "remux::audio",
    "remux::bits",
    "remux::mp4",
    "remux::mp4_boxes",
    "remux::regroup",
    "remux::ts",
    "remux::video",
    "remux::webm",
    "startup",
    "stream",
    "stream::dash",
    "stream::download",
    "stream::fmp4",
    "stream::hls",
    "transport",
    "transport::auth",
    "transport::client",
    "update",
    "win",
    "ytdlp",
    "ytdlp::formats",
    "ytdlp::install",
    "ytdlp::playlist",
];

fn crash_location(file: &str, line: u32) -> String {
    let module = file
        .strip_prefix("src/")
        .or_else(|| file.strip_prefix("src\\"))
        .and_then(|name| name.strip_suffix(".rs"))
        .map(|name| {
            name.strip_suffix("/mod")
                .or_else(|| name.strip_suffix("\\mod"))
                .unwrap_or(name)
        })
        .map(|name| name.replace(['/', '\\'], "::"));
    match module.filter(|name| CRASH_MODULES.contains(&name.as_str())) {
        Some(name) => format!("{name}:{line}"),
        None => "unavailable".into(),
    }
}

pub(crate) fn crash_diagnostics(store: &Path) -> String {
    use std::io::Read;
    let mut report = String::new();
    for (file, label) in [
        ("crash.log", "Application crash"),
        ("renderer-crash.log", "Renderer crash"),
    ] {
        let path = store.with_file_name(file);
        if !path.is_file() {
            continue;
        }
        let Ok(file) = std::fs::File::open(path) else {
            continue;
        };
        let mut prefix = Vec::new();
        if file.take(128).read_to_end(&mut prefix).is_err() {
            continue;
        }
        let header = prefix
            .split(|byte| *byte == b'\n')
            .next()
            .and_then(|line| std::str::from_utf8(line).ok())
            .and_then(|line| line.strip_prefix("NDM crash location "))
            .and_then(|line| line.rsplit_once(':'))
            .and_then(|(module, line)| {
                CRASH_MODULES.contains(&module).then_some(())?;
                Some((module, line.parse::<u32>().ok()?))
            });
        if report.is_empty() {
            report.push_str("\n### Local diagnostics\n");
        }
        if let Some((module, line)) = header {
            report.push_str(&format!(
                "- {label} recorded (module {module}, line {line}).\n"
            ));
        } else {
            report.push_str(&format!("- {label} recorded.\n"));
        }
    }
    report
}

#[test]
fn problem_report_excludes_raw_crash_data() {
    let dir = std::env::temp_dir().join(format!("ndm-crash-report-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let store = dir.join("state.json");
    assert!(crash_diagnostics(&store).is_empty());
    assert_eq!(crash_location("src/engine.rs", 42), "engine:42");
    assert_eq!(crash_location("src/app/help.rs", 9), "app::help:9");
    assert_eq!(crash_location(r"src\app\help.rs", 9), "app::help:9");
    assert_eq!(crash_location("src/core/mod.rs", 10), "core:10");
    assert_eq!(crash_location("src/remux/webm.rs", 14), "remux::webm:14");
    assert_eq!(
        crash_location(r"src\app\dialogs\options\mod.rs", 20),
        "app::dialogs::options:20"
    );
    assert_eq!(
        crash_location("src/transport/client.rs", 12),
        "transport::client:12"
    );
    assert_eq!(
        crash_location("src/platform/linux/drag_drop.rs", 15),
        "platform::linux::drag_drop:15"
    );
    assert_eq!(crash_location("src/app/private.rs", 9), "unavailable");
    assert_eq!(crash_location(r"C:\private\engine.rs", 42), "unavailable");
    std::fs::write(dir.join("crash.log"),
        "NDM crash location engine:42\nhttps://user:secret@example.com/private?token=123\nCookie: session=private\n/home/user/file").unwrap();
    std::fs::write(
        dir.join("renderer-crash.log"),
        "NDM crash location private:15\nAuthorization: Bearer secret",
    )
    .unwrap();
    assert_eq!(crash_diagnostics(&store),
        "\n### Local diagnostics\n- Application crash recorded (module engine, line 42).\n- Renderer crash recorded.\n");
    std::fs::write(
        dir.join("renderer-crash.log"),
        "NDM crash location app::help:9\nprivate renderer trace",
    )
    .unwrap();
    assert!(
        crash_diagnostics(&store).contains("- Renderer crash recorded (module app::help, line 9).")
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn problem_report() {
    let (title, text) = issue_text("2.9.0", "ndm_ar");
    assert_eq!(title, "[2.9.0] ");
    assert!(
        text.contains("- NDM 2.9.0\n")
            && text.contains("Language: ndm_ar")
            && text.contains(std::env::consts::OS)
    );
    assert!(issue_text("2.9.0", "").1.contains("Language: en (English)"));
}

pub(crate) fn install_hook() {
    // no console: a panic would vanish without a trace, so it goes to crash.log next to state.json
    std::panic::set_hook(Box::new(|p| {
        let bt = std::backtrace::Backtrace::force_capture();
        let location = p.location().map_or_else(
            || "unavailable".into(),
            |loc| crash_location(loc.file(), loc.line()),
        );
        let _ = std::fs::write(
            core::state::store_path().with_file_name("crash.log"),
            format!("NDM crash location {location}\n{p}\n{bt}"),
        );
    }));
}
