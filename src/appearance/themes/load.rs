use super::{
    asset, color,
    model::{Base, ThemeFile, METRICS, PALETTE, REGIONS},
    render::patch,
    Loaded,
};
use eframe::egui::{ColorImage, FontFamily, Style};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

fn read(path: &Path, max: u64) -> Result<Vec<u8>, String> {
    let size = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    if size > max {
        return Err(format!("{} is too large", path.display()));
    }
    std::fs::read(path).map_err(|e| e.to_string())
}

pub(crate) fn load(path: &Path) -> Result<Loaded, String> {
    let file: ThemeFile = serde_json::from_slice(&read(path, 1024 * 1024)?)
        .map_err(|e| format!("Invalid theme JSON: {e}"))?;
    if file.version != 1 {
        return Err(format!("Unsupported theme version: {}", file.version));
    }
    if file.name.trim().is_empty()
        || file.name.chars().count() > 100
        || file.name.chars().any(char::is_control)
    {
        return Err("Theme name must contain 1–100 characters".into());
    }
    let root = path
        .parent()
        .unwrap_or(Path::new("."))
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let mut images = BTreeMap::new();
    let mut fonts = BTreeMap::new();
    let mut asset_bytes = 0usize;
    for (name, file) in &file.fonts {
        if name.trim().is_empty() {
            return Err("Font family must have a name".into());
        }
        let bytes = read(&asset(&root, file)?, 16 * 1024 * 1024)?;
        ab_glyph::FontArc::try_from_vec(bytes.clone())
            .map_err(|_| format!("Invalid TTF/OTF font: {file}"))?;
        asset_bytes += bytes.len();
        if asset_bytes > 128 * 1024 * 1024 {
            return Err("Decoded theme assets exceed 128 MiB".into());
        }
        fonts.insert(name.clone(), bytes);
    }
    for l in [&file.common, &file.light, &file.dark] {
        for (name, c) in &l.palette {
            if !PALETTE.contains(&name.as_str()) && !toolbar_color(name) {
                return Err(format!("Unknown palette color: {name}"));
            }
            color(c)?;
        }
        for (name, v) in &l.metrics {
            if !METRICS.contains(&name.as_str()) {
                return Err(format!("Unknown metric: {name}"));
            }
            let range = if name.ends_with("_font") {
                6.0..=96.0
            } else if name == "toolbar_icon_size" {
                0.25..=4.0
            } else {
                0.0..=256.0
            };
            if !v.is_finite() || !range.contains(v) {
                return Err(format!("Invalid metric: {name}"));
            }
        }
        for (name, r) in &l.regions {
            if !REGIONS.contains(&name.as_str()) {
                return Err(format!("Unknown theme region: {name}"));
            }
            for c in [&r.fill, &r.border].into_iter().flatten() {
                color(c)?;
            }
            if r.image_opacity
                .is_some_and(|v| !v.is_finite() || !(0.0..=1.0).contains(&v))
            {
                return Err("Image opacity must be between 0 and 1".into());
            }
            for v in [r.border_width, r.corner_radius].into_iter().flatten() {
                if !v.is_finite() || !(0.0..=64.0).contains(&v) {
                    return Err("Borders and corner radii must be between 0 and 64".into());
                }
            }
            if let Some(name) = &r.image {
                if images.contains_key(name) {
                    continue;
                }
                let i = crate::appearance::icons::read_image(&asset(&root, name)?, 2048)?;
                asset_bytes += i.as_raw().len();
                if asset_bytes > 128 * 1024 * 1024 {
                    return Err("Decoded theme assets exceed 128 MiB".into());
                }
                images.insert(
                    name.clone(),
                    ColorImage::from_rgba_unmultiplied(
                        [i.width() as usize, i.height() as usize],
                        i.as_raw(),
                    ),
                );
            }
        }
    }
    for directory in [&file.toolbar_icons, &file.category_icons]
        .into_iter()
        .flatten()
    {
        let dir = asset(&root, directory)?;
        if !dir.is_dir() {
            return Err(format!("Not an icon directory: {directory}"));
        }
        for entry in std::fs::read_dir(&dir).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let relative = Path::new(directory).join(entry.file_name());
            let name = relative.to_str().ok_or("Theme asset name must be UTF-8")?;
            let path = asset(&root, name)?;
            if path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("png"))
            {
                crate::appearance::icons::read_image(&path, 256)?;
            } else {
                return Err(format!("Icon folders contain PNG files only: {name}"));
            }
        }
    }
    for dark in [false, true] {
        let mut s = Style::default();
        for l in [&file.common, if dark { &file.dark } else { &file.light }] {
            patch(&mut s, &l.style)?;
            for r in l.regions.values() {
                let mut region = s.clone();
                patch(&mut region, &r.style)?;
                validate_fonts(&region, &fonts)?;
            }
        }
        validate_fonts(&s, &fonts)?;
    }
    Ok(Loaded {
        file,
        root,
        images,
        fonts,
    })
}

fn toolbar_color(name: &str) -> bool {
    name.strip_prefix("toolbar.").is_some_and(|n| {
        [
            "add-url",
            "resume",
            "stop",
            "stop-all",
            "delete",
            "delete-completed",
            "options",
            "scheduler",
            "start-queue",
            "stop-queue",
            "grabber",
            "tell-a-friend",
        ]
        .contains(&n)
    })
}

fn validate_fonts(style: &Style, fonts: &BTreeMap<String, Vec<u8>>) -> Result<(), String> {
    for f in style
        .text_styles
        .values()
        .chain(style.override_font_id.iter())
    {
        if !f.size.is_finite() || !(6.0..=96.0).contains(&f.size) {
            return Err("Theme font sizes must be between 6 and 96".into());
        }
        if let FontFamily::Name(name) = &f.family {
            if !["segoe", "list"].contains(&name.as_ref()) && !fonts.contains_key(name.as_ref()) {
                return Err(format!("Theme uses an undefined font family: {name}"));
            }
        }
    }
    Ok(())
}

pub(crate) fn load_selected(root: &Path, name: &str) -> Result<Loaded, String> {
    if Path::new(name).components().count() != 1 {
        return Err("Invalid theme folder name".into());
    }
    let folder = asset(root, name)?;
    let path = asset(&folder, "theme.json")?;
    load(&path)
}

pub(crate) fn list(root: &Path) -> Vec<(String, String)> {
    let mut themes: Vec<_> = std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().join("theme.json").is_file())
        .filter_map(|e| {
            let folder = e.file_name().into_string().ok()?;
            let name = read(&e.path().join("theme.json"), 1024 * 1024)
                .ok()
                .and_then(|s| serde_json::from_slice::<serde_json::Value>(&s).ok())
                .and_then(|s| s.get("name").and_then(|s| s.as_str()).map(str::to_owned))
                .unwrap_or_else(|| folder.clone());
            Some((folder, name))
        })
        .collect();
    themes.sort_by_key(|t| t.1.to_lowercase());
    themes
}

fn new_folder(root: &Path, stem: &str) -> Result<PathBuf, String> {
    std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
    for k in 1..10000 {
        let folder = root.join(if k == 1 {
            stem.to_string()
        } else {
            format!("{stem}-{k}")
        });
        match std::fs::create_dir(&folder) {
            Ok(()) => return Ok(folder),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(e) => return Err(e.to_string()),
        }
    }
    Err("Too many installed themes".into())
}

/// Copy only validated references, never an entire source directory (which may be Downloads).
pub(crate) fn install(source: &Path, root: &Path) -> Result<(String, Loaded), String> {
    let loaded = load(source)?;
    let dest = new_folder(root, "Imported-theme")?;
    let result = (|| {
        let mut files: BTreeSet<String> = loaded
            .fonts
            .keys()
            .map(|k| loaded.file.fonts[k].clone())
            .collect();
        for l in [&loaded.file.common, &loaded.file.light, &loaded.file.dark] {
            files.extend(l.regions.values().filter_map(|r| r.image.clone()));
        }
        for directory in [&loaded.file.toolbar_icons, &loaded.file.category_icons]
            .into_iter()
            .flatten()
        {
            for f in
                std::fs::read_dir(asset(&loaded.root, directory)?).map_err(|e| e.to_string())?
            {
                let f = f.map_err(|e| e.to_string())?;
                files.insert(format!(
                    "{directory}/{}",
                    f.file_name().to_str().ok_or("Invalid asset name")?
                ));
            }
            std::fs::create_dir_all(dest.join(directory)).map_err(|e| e.to_string())?;
        }
        for name in files {
            let path = dest.join(&name);
            std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
            std::fs::copy(asset(&loaded.root, &name)?, path).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_vec_pretty(&loaded.file).map_err(|e| e.to_string())?;
        crate::file_ops::atomic_write(&dest.join("theme.json"), &json)
            .map_err(|e| e.to_string())?;
        Ok((
            dest.file_name().unwrap().to_str().unwrap().to_string(),
            load(&dest.join("theme.json"))?,
        ))
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&dest);
    }
    result
}

pub(crate) fn create(root: &Path, base: Base, styles: [&Style; 2]) -> Result<PathBuf, String> {
    let dest = new_folder(root, "My-theme")?;
    let file = serde_json::json!({"version":1,"name":dest.file_name().unwrap().to_str().unwrap(),"base":base,
        "common":{"palette":{},"metrics":{},"style":{},"regions":{}},"light":{},"dark":{}});
    for (name, value) in [
        ("theme.json", file),
        (
            "style-reference-light.json",
            serde_json::to_value(styles[0]).map_err(|e| e.to_string())?,
        ),
        (
            "style-reference-dark.json",
            serde_json::to_value(styles[1]).map_err(|e| e.to_string())?,
        ),
    ] {
        std::fs::write(
            dest.join(name),
            serde_json::to_vec_pretty(&value).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(dest)
}
