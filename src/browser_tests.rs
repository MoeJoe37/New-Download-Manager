use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "ndm-browser-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn file(&self, path: &str, bytes: &[u8]) -> PathBuf {
        let path = self.0.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bytes).unwrap();
        path
    }
    fn browser(&self, name: &str, file: &str) -> (String, String) {
        let path = self.file(file, b"fixture executable");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        (name.into(), path.display().to_string())
    }
    fn locations(&self, platform: Platform) -> Locations {
        Locations {
            platform,
            home: self.0.clone(),
            config: self.0.join("custom config"),
            roaming: self.0.join("Roaming"),
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn installed_sources_exclude_missing_browsers_and_deduplicate_inventory() {
    let f = Fixture::new();
    let records = vec![
        f.browser("Google Chrome", "bin/chrome.exe"),
        f.browser("Google Chrome", "bin/other-chrome.exe"),
        f.browser("Zen Browser", "bin/zen.exe"),
        f.browser("Waterfox", "bin/waterfox.exe"),
        f.browser("LibreWolf", "bin/librewolf.exe"),
        f.browser("Floorp", "bin/floorp.exe"),
        (
            "Firefox".into(),
            f.0.join("missing/firefox.exe").display().to_string(),
        ),
        f.browser("Unknown Browser", "bin/unknown"),
    ];
    f.file(".mozilla/firefox/stale/cookies.sqlite", b"leftover profile");
    let choices = installed_for(&records, Platform::Windows);
    assert_eq!(
        choices.iter().map(|b| b.id).collect::<Vec<_>>(),
        ["zen", "waterfox", "librewolf", "floorp", "chrome"]
    );
    assert!(installed_for(&[], Platform::Linux).is_empty());
    assert_eq!(identify("Zen Browser", "firefox.exe"), Some("zen"));
    assert_eq!(identify("Browser", r"C:\Apps\msedge.exe"), Some("edge"));
}

#[test]
fn safari_is_only_an_installed_macos_source() {
    let f = Fixture::new();
    let records = [f.browser("Safari", "Safari.app/Contents/MacOS/Safari")];
    assert!(installed_for(&records, Platform::Windows).is_empty());
    assert!(installed_for(&records, Platform::Linux).is_empty());
    assert_eq!(installed_for(&records, Platform::Mac)[0].id, "safari");
}

#[test]
fn firefox_derivatives_use_their_own_default_profile_on_every_platform() {
    for platform in [Platform::Windows, Platform::Mac, Platform::Linux] {
        for id in ["zen", "waterfox", "librewolf", "floorp"] {
            let f = Fixture::new();
            let locations = f.locations(platform);
            let root = locations.roots(id).remove(0);
            std::fs::create_dir_all(root.join("Profiles/default user أ")).unwrap();
            std::fs::write(
                root.join("profiles.ini"),
                "[Profile0]\nName=default\nIsRelative=1\nPath=Profiles/default user أ\nDefault=1\n",
            )
            .unwrap();
            let source = cookie_source_in(id, &locations).unwrap();
            assert_eq!(
                source,
                format!("firefox:{}", root.join("Profiles/default user أ").display())
            );
            assert_ne!(source, "firefox");
        }
    }
}

#[test]
fn fork_profiles_support_flatpak_legacy_and_absolute_custom_locations() {
    for id in ["zen", "waterfox", "librewolf", "floorp"] {
        let f = Fixture::new();
        let locations = f.locations(Platform::Linux);
        let root = locations.roots(id).pop().unwrap();
        std::fs::create_dir_all(root.join("profile")).unwrap();
        std::fs::write(root.join("profile/cookies.sqlite"), []).unwrap();
        assert_eq!(
            cookie_source_in(id, &locations).unwrap(),
            format!("firefox:{}", root.display())
        );
    }
    let f = Fixture::new();
    let locations = f.locations(Platform::Windows);
    let root = locations.roots("zen").remove(0);
    let custom = f.0.join("other disk/custom profile");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&custom).unwrap();
    std::fs::write(
        root.join("profiles.ini"),
        format!(
            "[Profile0]\nIsRelative=0\nPath={}\nDefault=1\n",
            custom.display()
        ),
    )
    .unwrap();
    assert_eq!(
        cookie_source_in("zen", &locations).unwrap(),
        format!("firefox:{}", custom.display())
    );
    std::fs::write(
        root.join("profiles.ini"),
        "[Install123]\nDefault=Profiles/current\n",
    )
    .unwrap();
    std::fs::create_dir_all(root.join("Profiles/current")).unwrap();
    assert_eq!(
        cookie_source_in("zen", &locations).unwrap(),
        format!("firefox:{}", root.join("Profiles/current").display())
    );
}

#[test]
fn missing_fork_profiles_never_fall_back_to_firefox_and_legacy_ids_stay_valid() {
    let f = Fixture::new();
    let locations = f.locations(Platform::Linux);
    f.file(
        ".mozilla/firefox/default/cookies.sqlite",
        b"another browser's cookies",
    );
    assert_eq!(
        cookie_source_in("zen", &locations),
        Err(CookieSourceError::MissingProfile)
    );
    for id in [
        "", "firefox", "chrome", "edge", "chromium", "brave", "vivaldi", "opera", "safari", "whale",
    ] {
        assert_eq!(cookie_source_in(id, &locations).unwrap(), id);
    }
    for id in ["unknown", "firefox:/arbitrary/profile", "--exec=command"] {
        assert_eq!(
            cookie_source_in(id, &locations),
            Err(CookieSourceError::Unsupported)
        );
    }
}
