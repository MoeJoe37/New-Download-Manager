use crate::app::{copy_dir, core, explore, i18n, tr_os, App, Dialog, Path, PathBuf};

mod export;

/// The browser's own extensions page, for the browsers the extension (Chromium MV3) runs in.
pub(in crate::app) fn extension_page(b: &core::settings::Browser) -> Option<String> {
    let k = core::settings::browser_kind(&format!("{} {}", b.name, b.exe));
    if k == "firefox" {
        return Some("about:debugging#/runtime/this-firefox".into());
    }
    matches!(
        k,
        "chrome" | "edge" | "opera" | "vivaldi" | "brave" | "yandex"
    )
    .then(|| {
        format!(
            "{}://extensions",
            if k == "chrome" {
                "chrome"
            } else if k == "yandex" {
                "browser"
            } else {
                k
            }
        )
    })
}

/// The browser-extension window's instructions (English keys): Firefox's temporary add-on, the other
/// browsers' unpacked extension.
pub(in crate::app) const GUIDES: [&str; 2] = [
    "To capture downloads from Firefox, open its extensions page (about:debugging), click \"This Firefox\", press \"Load Temporary Add-on...\" and select manifest.json in this folder. Firefox removes the add-on when it closes, so load it again after a restart:",
    "To capture downloads from this browser, open its extensions page, turn on \"Developer mode\", press \"Load unpacked\" and select this folder:",
];

pub(in crate::app) fn integration(b: &core::settings::Browser) -> &'static str {
    if extension_page(b).is_some() {
        "Extension"
    } else {
        "Not supported"
    }
}

/// The unpacked browser extension: next to the exe or in one of its parent folders (cargo builds).
pub(in crate::app) fn extension_dir() -> Option<PathBuf> {
    let bundled = i18n::beside_exe("extension", "manifest.json")?;
    // an AppImage, and a Mac app (whose bundle the update replaces), give browsers a copy that stays put
    if std::env::var_os("APPIMAGE").is_none() && !cfg!(target_os = "macos") {
        return Some(bundled);
    }
    // inside the AppImage the bundled copy is under /tmp/.mount_*, a new folder every run: browsers get a
    // copy beside state.json that stays put, refreshed when NDM's extension changes
    static STABLE: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    STABLE
        .get_or_init(|| {
            let d = core::state::store_path().with_file_name("extension");
            refresh_copy(&bundled, &d).ok().map(|_| d)
        })
        .clone()
        .or(Some(bundled))
}

/// Makes `to` a copy of the folder `from` unless their manifest.json files are already the same.
fn refresh_copy(from: &Path, to: &Path) -> std::io::Result<()> {
    let manifest = |d: &Path| std::fs::read(d.join("manifest.json")).ok();
    if manifest(to).is_some() && manifest(to) == manifest(from) {
        return Ok(());
    }
    copy_dir(from, to)
}

#[test]
fn extension_copy_refreshes() {
    let base = std::env::temp_dir().join(format!("ndm-test-ext-{}", std::process::id()));
    let (from, to) = (base.join("bundled"), base.join("stable"));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(from.join("icons")).unwrap();
    std::fs::write(from.join("manifest.json"), "v1").unwrap();
    std::fs::write(from.join("icons/a.png"), "png").unwrap();
    refresh_copy(&from, &to).unwrap();
    assert_eq!(
        std::fs::read_to_string(to.join("icons/a.png")).unwrap(),
        "png"
    );
    // same manifest: left alone
    std::fs::write(from.join("background.js"), "new").unwrap();
    refresh_copy(&from, &to).unwrap();
    assert!(!to.join("background.js").exists());
    // a new version: copied again
    std::fs::write(from.join("manifest.json"), "v2").unwrap();
    refresh_copy(&from, &to).unwrap();
    assert_eq!(
        std::fs::read_to_string(to.join("manifest.json")).unwrap(),
        "v2"
    );
    assert!(to.join("background.js").exists());
    let _ = std::fs::remove_dir_all(&base);
}

impl App {
    pub(in crate::app) fn export_extension(&mut self) {
        self.open(Dialog::ExtensionExport { firefox: false });
    }

    pub(in crate::app) fn export_extension_for(&mut self, firefox: bool) {
        let Some(src) = extension_dir() else {
            return self.open(Dialog::Info {
                title: "Export extension".into(),
                text: "The browser extension isn't in NDM's folder: install NDM again to get it."
                    .into(),
            });
        };
        let Some(to) = rfd::FileDialog::new()
            .set_title(tr_os("Export extension"))
            .pick_folder()
        else {
            return;
        };
        let guide = export_guide(firefox);
        let text = match export::write_export(&src, &to, firefox, &guide) {
            Ok(dest) => {
                explore(&dest);
                format!(
                    "{}\n\n{}",
                    i18n::trf_os(
                        "The browser extension is in this folder:\n%s",
                        &[&i18n::ltr(&dest.display().to_string())]
                    ),
                    guide
                )
            }
            Err(e) => i18n::trf_os(
                "The extension couldn't be saved there: %s",
                &[&i18n::tr_msg_os(&e.to_string())],
            ),
        };
        self.open(Dialog::Info {
            title: "Export extension".into(),
            text,
        });
    }
}

fn export_guide(firefox: bool) -> String {
    if firefox {
        tr_os("1. Open about:debugging#/runtime/this-firefox in your browser.\n2. Click \"Load Temporary Add-on...\" and select manifest.json in this folder.\n\nFirefox removes unsigned temporary add-ons when it closes. Load this folder again after restarting the browser. Permanent installation requires a Mozilla-signed add-on.\n\nKeep NDM running to receive downloads.")
    } else {
        tr_os("1. Open your browser's extensions page (chrome://extensions, edge://extensions or the browser's Manage extensions page).\n2. Turn on \"Developer mode\".\n3. Click \"Load unpacked\" and select this folder.\n\nKeep this folder in place and keep NDM running to receive downloads.")
    }
}
