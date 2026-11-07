use super::*;

fn assert_assets(source: &Path, exported: &Path) {
    for entry in std::fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let target = exported.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            assert_assets(&entry.path(), &target);
        } else if entry.file_name() != "manifest.json" {
            assert_eq!(
                std::fs::read(entry.path()).unwrap(),
                std::fs::read(target).unwrap()
            );
        }
    }
}

#[test]
fn extension_exports_match_browser_families() {
    let root = std::env::temp_dir().join(format!("ndm-export-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("extension");
    let before = std::fs::read(source.join("manifest.json")).unwrap();
    let original: serde_json::Value = serde_json::from_slice(&before).unwrap();
    for firefox in [false, true] {
        let guide = if firefox {
            "Firefox steps"
        } else {
            "Chromium steps"
        };
        let dest = write_export(&source, &root, firefox, guide).unwrap();
        assert_assets(&source, &dest);
        assert_eq!(
            std::fs::read_to_string(dest.join("INSTALL.txt")).unwrap(),
            guide
        );
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dest.join("manifest.json")).unwrap()).unwrap();
        for key in [
            "version",
            "manifest_version",
            "content_scripts",
            "host_permissions",
            "action",
            "options_ui",
        ] {
            assert_eq!(manifest[key], original[key]);
        }
        if firefox {
            assert!(manifest["background"].get("service_worker").is_none());
            assert_eq!(
                manifest["background"]["scripts"],
                original["background"]["scripts"]
            );
            assert!(manifest.get("key").is_none());
            assert_eq!(
                manifest["browser_specific_settings"],
                original["browser_specific_settings"]
            );
            assert!(!manifest["permissions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|p| p == "downloads.ui"));
        } else {
            assert_eq!(
                manifest["background"]["service_worker"],
                original["background"]["service_worker"]
            );
            assert!(manifest["background"].get("scripts").is_none());
            assert!(manifest.get("browser_specific_settings").is_none());
            assert_eq!(manifest["key"], original["key"]);
            assert_eq!(manifest["permissions"], original["permissions"]);
        }
        // Re-exporting must not replace or mix browser packages already in use.
        std::fs::write(dest.join("keep.txt"), "existing export").unwrap();
        let second = write_export(&source, &root, firefox, guide).unwrap();
        assert_ne!(dest, second);
        assert_eq!(
            std::fs::read_to_string(dest.join("keep.txt")).unwrap(),
            "existing export"
        );
        assert!(!second.join("keep.txt").exists());
    }
    assert_eq!(std::fs::read(source.join("manifest.json")).unwrap(), before);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn extension_export_rejects_recursive_destination() {
    let root = std::env::temp_dir().join(format!("ndm-export-nested-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("manifest.json"), "{}").unwrap();
    assert_eq!(
        write_export(&root, &root, false, "guide")
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
    std::fs::remove_dir_all(root).unwrap();
}
