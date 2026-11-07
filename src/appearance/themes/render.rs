use super::{active, color, model::ImageMode, revision, Loaded};
use eframe::egui::{self, Color32, FontFamily, Style, TextureHandle};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Clone, Default)]
struct Images {
    revision: u64,
    textures: BTreeMap<String, TextureHandle>,
}

pub(crate) fn clear_cache(ctx: &egui::Context) {
    ctx.data_mut(|d| d.remove::<Images>(egui::Id::new("theme-images")));
}

/// Merge object fields only; omitted values keep the selected base. Unknown fields are rejected.
pub(super) fn merge(base: &mut Value, patch: &Value, at: &str) -> Result<(), String> {
    if let (Value::Object(b), Value::Object(p)) = (&mut *base, patch) {
        for (k, v) in p {
            let target = b
                .get_mut(k)
                .ok_or_else(|| format!("Unknown style field {at}.{k}"))?;
            merge(target, v, &format!("{at}.{k}"))?;
        }
    } else if let Value::String(s) = patch {
        if s.starts_with('#') && (base.is_null() || base.as_array().is_some_and(|a| a.len() == 4)) {
            *base = serde_json::to_value(color(s)?.to_array()).map_err(|e| e.to_string())?;
        } else {
            *base = patch.clone();
        }
    } else {
        *base = patch.clone();
    }
    Ok(())
}

fn known_fields(input: &Value, parsed: &Value, at: &str) -> Result<(), String> {
    if let (Value::Object(input), Value::Object(parsed)) = (input, parsed) {
        for (key, value) in input {
            let next = parsed
                .get(key)
                .ok_or_else(|| format!("Unknown style field {at}.{key}"))?;
            known_fields(value, next, &format!("{at}.{key}"))?;
        }
    }
    if input
        .as_f64()
        .is_some_and(|v| !v.is_finite() || v.abs() > 8192.0)
    {
        return Err(format!("Style number out of range at {at}"));
    }
    Ok(())
}

pub(super) fn patch(style: &mut Style, values: &BTreeMap<String, Value>) -> Result<(), String> {
    if values.is_empty() {
        return Ok(());
    }
    let mut value = serde_json::to_value(&*style).map_err(|e| e.to_string())?;
    merge(
        &mut value,
        &serde_json::to_value(values).map_err(|e| e.to_string())?,
        "style",
    )?;
    let parsed: Style =
        serde_json::from_value(value.clone()).map_err(|e| format!("Invalid style: {e}"))?;
    known_fields(
        &value,
        &serde_json::to_value(&parsed).map_err(|e| e.to_string())?,
        "style",
    )?;
    *style = parsed;
    Ok(())
}

pub(crate) fn palette(dark: bool, mut apply: impl FnMut(&str, Color32)) {
    if let Some(t) = active() {
        for l in t.layers(dark) {
            for (name, c) in &l.palette {
                apply(name, color(c).expect("validated color"));
            }
        }
    }
}

pub(crate) fn number(name: &str, dark: bool, fallback: f32) -> f32 {
    active()
        .and_then(|t| {
            t.layers(dark)
                .into_iter()
                .filter_map(|l| l.metrics.get(name).copied())
                .next_back()
        })
        .unwrap_or(fallback)
}

pub(crate) fn role(name: &str, dark: bool, mut fallback: Color32) -> Color32 {
    palette(dark, |key, c| {
        if key == name {
            fallback = c;
        }
    });
    fallback
}

pub(crate) fn apply_style(style: &mut Style, dark: bool) {
    if let Some(t) = active() {
        palette(dark, |name, c| {
            let v = &mut style.visuals;
            match name {
                "bg" => v.panel_fill = c,
                "surface" => {
                    v.window_fill = c;
                    v.extreme_bg_color = c;
                }
                "raised" => {
                    v.faint_bg_color = c;
                    v.widgets.inactive.bg_fill = c;
                    v.widgets.inactive.weak_bg_fill = c;
                }
                "text" => {
                    v.override_text_color = Some(c);
                    for w in [
                        &mut v.widgets.noninteractive,
                        &mut v.widgets.inactive,
                        &mut v.widgets.hovered,
                        &mut v.widgets.active,
                        &mut v.widgets.open,
                    ] {
                        w.fg_stroke.color = c;
                    }
                }
                "border" => {
                    v.window_stroke.color = c;
                    for w in [
                        &mut v.widgets.noninteractive,
                        &mut v.widgets.inactive,
                        &mut v.widgets.hovered,
                        &mut v.widgets.active,
                        &mut v.widgets.open,
                    ] {
                        w.bg_stroke.color = c;
                    }
                }
                "accent" => {
                    v.selection.stroke.color = c;
                    v.hyperlink_color = c;
                }
                "accent_soft" => {
                    v.selection.bg_fill = c;
                    v.widgets.hovered.bg_fill = c;
                    v.widgets.hovered.weak_bg_fill = c;
                }
                "accent_hot" => {
                    v.widgets.active.bg_fill = c;
                    v.widgets.active.weak_bg_fill = c;
                }
                "danger" => v.error_fg_color = c,
                _ => (),
            }
        });
        for l in t.layers(dark) {
            patch(style, &l.style).expect("validated theme style");
        }
    }
}

pub(crate) fn apply_fonts(fonts: &mut egui::FontDefinitions) {
    let Some(t) = active() else {
        return;
    };
    for (name, bytes) in &t.fonts {
        let id = format!("theme:{name}");
        fonts.font_data.insert(
            id.clone(),
            Arc::new(egui::FontData::from_owned(bytes.clone())),
        );
        let family = match name.as_str() {
            "proportional" => FontFamily::Proportional,
            "monospace" => FontFamily::Monospace,
            _ => FontFamily::Name(name.clone().into()),
        };
        fonts
            .families
            .entry(family)
            .or_default()
            .insert(0, id.clone());
        if name == "proportional" {
            for family in ["segoe", "list"] {
                fonts
                    .families
                    .entry(FontFamily::Name(family.into()))
                    .or_default()
                    .insert(0, id.clone());
            }
        }
    }
}

pub(crate) fn asset_dir(kind: &str) -> Option<PathBuf> {
    let t = active()?;
    let name = if kind == "toolbar" {
        t.file.toolbar_icons.as_ref()?
    } else {
        t.file.category_icons.as_ref()?
    };
    Some(t.root.join(name))
}

pub(crate) fn frame(name: &str, mut frame: egui::Frame, dark: bool) -> egui::Frame {
    if let Some(t) = active() {
        for r in t.regions(name, dark) {
            if let Some(c) = &r.fill {
                frame.fill = color(c).expect("validated color");
            }
            if let Some(c) = &r.border {
                frame.stroke.color = color(c).expect("validated color");
            }
            if let Some(w) = r.border_width {
                frame.stroke.width = w;
            }
            if let Some(radius) = r.corner_radius {
                frame.corner_radius = egui::CornerRadius::same(radius as u8);
            }
            if let Some([left, right, top, bottom]) = r.margin {
                frame.inner_margin = egui::Margin {
                    left,
                    right,
                    top,
                    bottom,
                };
            }
        }
    }
    frame
}

fn image(ui: &egui::Ui, t: &Loaded, name: &str, mode: ImageMode, opacity: f32, rect: egui::Rect) {
    if !rect.is_finite() || rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    let Some(img) = t.images.get(name) else {
        return;
    };
    let key = egui::Id::new("theme-images");
    let mut cache = ui
        .ctx()
        .data_mut(|d| d.get_temp::<Images>(key))
        .unwrap_or_default();
    let current = revision();
    if cache.revision != current {
        cache = Images {
            revision: current,
            ..Default::default()
        };
    }
    let texture = if let Some(t) = cache.textures.get(name) {
        t.clone()
    } else {
        let t = ui
            .ctx()
            .load_texture(name, img.clone(), egui::TextureOptions::LINEAR);
        cache.textures.insert(name.to_owned(), t.clone());
        t
    };
    ui.ctx().data_mut(|d| d.insert_temp(key, cache));

    let mut uv = egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0));
    let size = texture.size_vec2();
    let mut target = rect;
    match mode {
        ImageMode::Cover => {
            let ratio = (rect.width() / size.x) / (rect.height() / size.y);
            if ratio > 1.0 {
                uv.min.y = (1.0 - 1.0 / ratio) / 2.0;
                uv.max.y = 1.0 - uv.min.y;
            } else {
                uv.min.x = (1.0 - ratio) / 2.0;
                uv.max.x = 1.0 - uv.min.x;
            }
        }
        ImageMode::Contain => {
            target = egui::Rect::from_center_size(
                rect.center(),
                size * (rect.width() / size.x).min(rect.height() / size.y),
            )
        }
        ImageMode::Stretch => (),
        ImageMode::Tile => {
            let p = ui.painter().with_clip_rect(rect);
            // Bound work even when a user supplies a one-pixel tile and a very large window.
            let size = size.max(rect.size() / 64.0);
            for y in 0..(rect.height() / size.y).ceil() as usize {
                for x in 0..(rect.width() / size.x).ceil() as usize {
                    p.image(
                        texture.id(),
                        egui::Rect::from_min_size(
                            rect.min + egui::vec2(x as f32 * size.x, y as f32 * size.y),
                            size,
                        ),
                        uv,
                        Color32::WHITE.gamma_multiply(opacity),
                    );
                }
            }
            return;
        }
    }
    ui.painter().with_clip_rect(rect).image(
        texture.id(),
        target,
        uv,
        Color32::WHITE.gamma_multiply(opacity),
    );
}

pub(crate) fn region_style(style: &mut Style, name: &str, dark: bool) {
    if let Some(t) = active() {
        for r in t.regions(name, dark) {
            patch(style, &r.style).expect("validated region style");
        }
    }
}

pub(crate) fn paint(ui: &mut egui::Ui, name: &str) {
    let dark = ui.visuals().dark_mode;
    region_style(ui.style_mut(), name, dark);
    background(ui, name);
}

pub(crate) fn background(ui: &egui::Ui, name: &str) {
    let Some(t) = active() else {
        return;
    };
    let regions = t.regions(name, ui.visuals().dark_mode);
    let rect = ui.max_rect();
    if !rect.is_finite() || rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    if let Some(fill) = regions.iter().rev().find_map(|r| r.fill.as_ref()) {
        let radius = regions
            .iter()
            .rev()
            .find_map(|r| r.corner_radius)
            .unwrap_or(0.0);
        ui.painter()
            .rect_filled(rect, radius, color(fill).expect("validated color"));
    }
    // A mode-specific image replaces the common image; other omitted properties inherit.
    if let Some(r) = regions.iter().rev().find(|r| r.image.is_some()) {
        let opacity = regions
            .iter()
            .rev()
            .find_map(|r| r.image_opacity)
            .unwrap_or(1.0);
        let mode = regions
            .iter()
            .rev()
            .find_map(|r| r.image_mode)
            .unwrap_or_default();
        image(ui, &t, r.image.as_ref().unwrap(), mode, opacity, rect);
    }
    if name == "popup" {
        let border = regions.iter().rev().find_map(|r| r.border.as_ref());
        let width = regions.iter().rev().find_map(|r| r.border_width);
        if border.is_some() || width.is_some() {
            let color = border
                .map(|s| color(s).expect("validated color"))
                .unwrap_or(ui.visuals().window_stroke.color);
            let radius = regions
                .iter()
                .rev()
                .find_map(|r| r.corner_radius)
                .unwrap_or(0.0);
            ui.painter().rect_stroke(
                rect,
                radius,
                egui::Stroke::new(width.unwrap_or(1.0), color),
                egui::StrokeKind::Inside,
            );
        }
    }
}
