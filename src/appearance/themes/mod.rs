//! Partial JSON themes. Selection is transactional: decoding and validation precede activation.
mod load;
mod model;
mod render;

use eframe::egui;
pub(crate) use load::{create, install, list, load_selected};
pub(crate) use model::{Base, ThemeFile};
use model::{Layer, Region};
pub(crate) use render::{
    apply_fonts, apply_style, asset_dir, background, clear_cache, frame, number, paint, palette,
    region_style, role,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};

pub(crate) struct Loaded {
    pub file: ThemeFile,
    root: PathBuf,
    images: BTreeMap<String, egui::ColorImage>,
    fonts: BTreeMap<String, Vec<u8>>,
}

struct Active {
    theme: Option<Arc<Loaded>>,
    revision: u64,
}
fn state() -> &'static RwLock<Active> {
    static ACTIVE: OnceLock<RwLock<Active>> = OnceLock::new();
    ACTIVE.get_or_init(|| {
        RwLock::new(Active {
            theme: None,
            revision: 0,
        })
    })
}
pub(crate) fn active() -> Option<Arc<Loaded>> {
    state().read().unwrap().theme.clone()
}
pub(crate) fn revision() -> u64 {
    state().read().unwrap().revision
}
pub(crate) fn activate(theme: Option<Loaded>) {
    let mut a = state().write().unwrap();
    a.theme = theme.map(Arc::new);
    a.revision = a.revision.wrapping_add(1);
}

impl Loaded {
    fn layers(&self, dark: bool) -> [&Layer; 2] {
        [
            &self.file.common,
            if dark {
                &self.file.dark
            } else {
                &self.file.light
            },
        ]
    }
    fn regions(&self, name: &str, dark: bool) -> Vec<&Region> {
        self.layers(dark)
            .into_iter()
            .filter_map(|l| l.regions.get(name))
            .collect()
    }
}

pub(crate) fn color(hex: &str) -> Result<egui::Color32, String> {
    let s = hex
        .strip_prefix('#')
        .ok_or_else(|| format!("Expected #RRGGBB or #RRGGBBAA: {hex}"))?;
    if !matches!(s.len(), 6 | 8) || !s.is_ascii() {
        return Err(format!("Invalid color: {hex}"));
    }
    let byte =
        |i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| format!("Invalid color: {hex}"));
    Ok(egui::Color32::from_rgba_unmultiplied(
        byte(0)?,
        byte(2)?,
        byte(4)?,
        if s.len() == 8 { byte(6)? } else { 255 },
    ))
}

/// Assets must be inside the theme directory, including after resolving symlinks.
fn asset(root: &Path, name: &str) -> Result<PathBuf, String> {
    let p = Path::new(name);
    if p.as_os_str().is_empty()
        || p.components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return Err(format!(
            "Theme asset must be a relative path inside its folder: {name}"
        ));
    }
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let p = root
        .join(p)
        .canonicalize()
        .map_err(|e| format!("{name}: {e}"))?;
    if !p.starts_with(root) {
        return Err(format!("Theme asset leaves its folder: {name}"));
    }
    Ok(p)
}

#[cfg(test)]
mod tests;
