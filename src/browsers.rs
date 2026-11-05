//! Installed cookie sources and the profile locations of Firefox derivatives.
use std::path::{Path, PathBuf};

#[cfg(test)]
#[path = "browser_tests.rs"]
mod tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CookieBrowser {
    pub id: &'static str,
    pub label: &'static str,
}

const BROWSERS: &[CookieBrowser] = &[
    CookieBrowser {
        id: "firefox",
        label: "Firefox",
    },
    CookieBrowser {
        id: "zen",
        label: "Zen",
    },
    CookieBrowser {
        id: "waterfox",
        label: "Waterfox",
    },
    CookieBrowser {
        id: "librewolf",
        label: "LibreWolf",
    },
    CookieBrowser {
        id: "floorp",
        label: "Floorp",
    },
    CookieBrowser {
        id: "chrome",
        label: "Chrome",
    },
    CookieBrowser {
        id: "edge",
        label: "Edge",
    },
    CookieBrowser {
        id: "chromium",
        label: "Chromium",
    },
    CookieBrowser {
        id: "brave",
        label: "Brave",
    },
    CookieBrowser {
        id: "vivaldi",
        label: "Vivaldi",
    },
    CookieBrowser {
        id: "opera",
        label: "Opera",
    },
    CookieBrowser {
        id: "whale",
        label: "Whale",
    },
    CookieBrowser {
        id: "safari",
        label: "Safari (macOS)",
    },
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Platform {
    Windows,
    Mac,
    Linux,
}

impl Platform {
    fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::Mac
        } else {
            Self::Linux
        }
    }
}

pub(crate) fn executable_exists(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn identify(name: &str, executable: &str) -> Option<&'static str> {
    let name = name.to_ascii_lowercase();
    // Product names take precedence over a derivative's generic firefox executable.
    let words: Vec<_> = name.split(|c: char| !c.is_ascii_alphanumeric()).collect();
    for id in [
        "zen",
        "waterfox",
        "librewolf",
        "floorp",
        "whale",
        "chromium",
        "brave",
        "vivaldi",
        "opera",
        "edge",
        "firefox",
        "chrome",
        "safari",
    ] {
        if words.contains(&id) {
            return Some(id);
        }
    }
    let file = executable.rsplit(['/', '\\']).next()?.to_ascii_lowercase();
    let stem = file.strip_suffix(".exe").unwrap_or(&file);
    match stem {
        "zen" | "zen-browser" => Some("zen"),
        "waterfox" | "librewolf" | "floorp" | "firefox" | "vivaldi" | "opera" | "safari"
        | "whale" => BROWSERS.iter().find(|b| b.id == stem).map(|b| b.id),
        "chrome" | "google-chrome" | "google-chrome-stable" => Some("chrome"),
        "chromium" | "chromium-browser" => Some("chromium"),
        "msedge" | "microsoft-edge" | "microsoft-edge-stable" => Some("edge"),
        "brave" | "brave-browser" => Some("brave"),
        _ => None,
    }
}

/// Fresh native inventory, never the saved browser integration preferences or leftover profiles.
pub(crate) fn installed(records: &[(String, String)]) -> Vec<CookieBrowser> {
    installed_for(records, Platform::current())
}

fn installed_for(records: &[(String, String)], platform: Platform) -> Vec<CookieBrowser> {
    BROWSERS
        .iter()
        .copied()
        .filter(|browser| {
            (browser.id != "safari" || platform == Platform::Mac)
                && records.iter().any(|(name, exe)| {
                    executable_exists(Path::new(exe)) && identify(name, exe) == Some(browser.id)
                })
        })
        .collect()
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CookieSourceError {
    Unsupported,
    MissingProfile,
}

struct Locations {
    platform: Platform,
    home: PathBuf,
    config: PathBuf,
    roaming: PathBuf,
}

impl Locations {
    fn current() -> Self {
        let platform = Platform::current();
        let home = std::env::var_os(if platform == Platform::Windows {
            "USERPROFILE"
        } else {
            "HOME"
        })
        .map(PathBuf::from)
        .unwrap_or_default();
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| home.join(".config"));
        let roaming = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| home.join("AppData/Roaming"));
        Self {
            platform,
            home,
            config,
            roaming,
        }
    }

    fn roots(&self, id: &str) -> Vec<PathBuf> {
        let product = match id {
            "waterfox" => "Waterfox",
            "floorp" => "Floorp",
            _ => id,
        };
        match self.platform {
            Platform::Windows => vec![self.roaming.join(product)],
            Platform::Mac => {
                let base = self.home.join("Library/Application Support");
                vec![
                    base.join(product),
                    base.join(if id == "librewolf" { "LibreWolf" } else { id }),
                ]
            }
            Platform::Linux => {
                let app = match id {
                    "zen" => "app.zen_browser.zen",
                    "waterfox" => "net.waterfox.waterfox",
                    "librewolf" => "io.gitlab.librewolf-community",
                    "floorp" => "one.ablaze.floorp",
                    _ => return vec![],
                };
                let flatpak = self.home.join(".var/app").join(app);
                vec![
                    self.config.join(id),
                    self.config.join(id).join(id),
                    self.home.join(format!(".{id}")),
                    flatpak.join("config").join(id),
                    flatpak.join("config").join(id).join(id),
                    flatpak.join(id),
                    flatpak.join(format!(".{id}")),
                ]
            }
        }
    }
}

/// Keep saved browser IDs portable; resolve a derivative's own profile only when invoking yt-dlp.
pub(crate) fn cookie_source(id: &str) -> Result<String, CookieSourceError> {
    cookie_source_in(id, &Locations::current())
}

fn cookie_source_in(id: &str, locations: &Locations) -> Result<String, CookieSourceError> {
    if id.is_empty() {
        return Ok(String::new());
    }
    if !BROWSERS.iter().any(|b| b.id == id) {
        return Err(CookieSourceError::Unsupported);
    }
    if !matches!(id, "zen" | "waterfox" | "librewolf" | "floorp") {
        return Ok(id.into());
    }
    let roots = locations.roots(id);
    for root in &roots {
        if let Some(profile) = default_profile(root) {
            return Ok(format!("firefox:{}", profile.display()));
        }
        if has_cookies(root) {
            return Ok(format!("firefox:{}", root.display()));
        }
    }
    Err(CookieSourceError::MissingProfile)
}

fn has_cookies(root: &Path) -> bool {
    let children = |dir: &Path| {
        std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .any(|e| e.path().join("cookies.sqlite").is_file())
    };
    root.join("cookies.sqlite").is_file() || children(root) || children(&root.join("Profiles"))
}

/// Firefox-family profiles.ini supports both relative and custom absolute profile directories.
fn default_profile(root: &Path) -> Option<PathBuf> {
    let source = std::fs::read_to_string(root.join("profiles.ini")).ok()?;
    let mut sections: Vec<(String, Vec<(&str, &str)>)> = vec![];
    for line in source.trim_start_matches('\u{feff}').lines().map(str::trim) {
        if let Some(name) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            sections.push((name.into(), vec![]));
        } else if let Some((key, value)) = line.split_once('=') {
            if let Some((_, fields)) = sections.last_mut() {
                fields.push((key.trim(), value.trim()));
            }
        }
    }
    let mut profiles = vec![];
    let mut defaults = vec![];
    for (name, fields) in &sections {
        let field = |key| fields.iter().find(|(k, _)| *k == key).map(|(_, v)| *v);
        if name.starts_with("Install") {
            if let Some(path) = field("Default") {
                defaults.push(root.join(path));
            }
        } else if name.starts_with("Profile") {
            if let Some(path) = field("Path") {
                let path = if field("IsRelative") == Some("0") {
                    PathBuf::from(path)
                } else {
                    root.join(path)
                };
                if path.is_absolute() && path.is_dir() {
                    if field("Default") == Some("1") {
                        defaults.push(path.clone());
                    }
                    profiles.push(path);
                }
            }
        }
    }
    defaults
        .into_iter()
        .find(|p| p.is_absolute() && p.is_dir())
        .or_else(|| (profiles.len() == 1).then(|| profiles.remove(0)))
}
