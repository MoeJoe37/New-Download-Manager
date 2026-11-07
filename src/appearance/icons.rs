use eframe::egui;
use image::{DynamicImage, ImageFormat, ImageReader, RgbaImage};
use sha2::{Digest, Sha256};
use std::io::Cursor;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};

pub(crate) type Pixels = (usize, usize, Vec<u8>);

#[derive(Clone)]
struct Icons {
    app: Pixels,
    tray: Option<Pixels>,
    revision: u64,
}

fn state() -> &'static RwLock<Icons> {
    static ICONS: OnceLock<RwLock<Icons>> = OnceLock::new();
    ICONS.get_or_init(|| {
        RwLock::new(Icons {
            app: default_app(),
            tray: None,
            revision: 0,
        })
    })
}

pub(crate) fn default_app() -> Pixels {
    let i = eframe::icon_data::from_png_bytes(include_bytes!("../../assets/ndm.png"))
        .expect("bundled icon");
    (i.width as usize, i.height as usize, i.rgba)
}

pub(crate) fn app() -> Pixels {
    state().read().unwrap().app.clone()
}
pub(crate) fn revision() -> u64 {
    state().read().unwrap().revision
}

pub(crate) fn viewport() -> Arc<egui::IconData> {
    let (w, h, rgba) = app();
    Arc::new(egui::IconData {
        width: w as u32,
        height: h as u32,
        rgba,
    })
}

pub(crate) fn tray(custom: bool) -> Pixels {
    if custom {
        if let Some(p) = &state().read().unwrap().tray {
            return p.clone();
        }
    }
    let i = eframe::icon_data::from_png_bytes(include_bytes!("../../assets/ndm2d.png"))
        .expect("bundled tray icon");
    (i.width as usize, i.height as usize, i.rgba)
}

pub(crate) fn stored(root: &Path, name: &str) -> Option<PathBuf> {
    if name.is_empty()
        || Path::new(name).components().count() != 1
        || !matches!(
            Path::new(name).components().next(),
            Some(Component::Normal(_))
        )
    {
        return None;
    }
    Some(root.join("icons").join(name))
}

pub(crate) fn initialize(root: &Path, app_name: &str, tray_name: &str) {
    let read = |name| {
        stored(root, name)
            .and_then(|p| read_image(&p, 256).ok())
            .map(pixels)
    };
    let mut s = state().write().unwrap();
    s.app = read(app_name).unwrap_or_else(default_app);
    s.tray = read(tray_name);
    s.revision = s.revision.wrapping_add(1);
}

pub(crate) fn initialize_store(store: &Path) {
    let saved = std::fs::read(store)
        .ok()
        .and_then(|s| serde_json::from_slice::<serde_json::Value>(&s).ok());
    let s = saved.as_ref().and_then(|s| s.get("settings"));
    let field = |key| {
        s.and_then(|s| s.get(key))
            .and_then(|s| s.as_str())
            .unwrap_or("")
    };
    initialize(
        store.parent().unwrap_or(Path::new(".")),
        field("app_icon"),
        field("tray_custom_icon"),
    );
}

fn pixels(i: RgbaImage) -> Pixels {
    (i.width() as usize, i.height() as usize, i.into_raw())
}

/// Content detection, bounded decoding and conversion; the file extension need not be correct.
pub(crate) fn read_image(path: &Path, max_side: u32) -> Result<RgbaImage, String> {
    if std::fs::metadata(path).map_err(|e| e.to_string())?.len() > 16 * 1024 * 1024 {
        return Err("Image files must be at most 16 MiB".into());
    }
    let data = std::fs::read(path).map_err(|e| e.to_string())?;
    if data.len() > 16 * 1024 * 1024 {
        return Err("Image files must be at most 16 MiB".into());
    }
    let svg = std::str::from_utf8(&data)
        .ok()
        .and_then(|s| roxmltree::Document::parse(s).ok())
        .is_some_and(|d| d.root_element().tag_name().name() == "svg");
    let image = if svg {
        let text = std::str::from_utf8(&data).map_err(|e| e.to_string())?;
        let doc = roxmltree::Document::parse(text).map_err(|e| e.to_string())?;
        // Theme assets are self-contained; no file or network references are followed.
        for a in doc.descendants().flat_map(|n| n.attributes()) {
            if a.name() == "href" && !a.value().starts_with('#') && !a.value().starts_with("data:")
            {
                return Err(
                    "SVG images must embed their images and use no external references".into(),
                );
            }
        }
        let c = egui_extras::image::load_svg_bytes_with_size(
            &data,
            Some(egui::load::SizeHint::Size(max_side, max_side)),
        )?;
        let mut rgba = Vec::with_capacity(c.pixels.len() * 4);
        for c in c.pixels {
            // The SVG loader's source pixmap is premultiplied; undo it for native icons and PNG.
            let [r, g, b, a] = c.to_srgba_unmultiplied();
            let straight = |v: u8| {
                if a == 0 {
                    0
                } else {
                    ((v as u32 * 255 + a as u32 / 2) / a as u32).min(255) as u8
                }
            };
            rgba.extend_from_slice(&[straight(r), straight(g), straight(b), a]);
        }
        RgbaImage::from_raw(c.size[0] as u32, c.size[1] as u32, rgba)
            .ok_or("Invalid SVG dimensions")?
    } else {
        let mut reader = ImageReader::new(Cursor::new(data))
            .with_guessed_format()
            .map_err(|e| e.to_string())?;
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(16384);
        limits.max_image_height = Some(16384);
        limits.max_alloc = Some(128 * 1024 * 1024);
        reader.limits(limits);
        reader
            .decode()
            .map_err(|e| e.to_string())?
            .thumbnail(max_side, max_side)
            .to_rgba8()
    };
    if image.width() == 0 || image.height() == 0 {
        return Err("Empty image".into());
    }
    Ok(image)
}

fn square(i: &RgbaImage, side: u32) -> RgbaImage {
    let resized = DynamicImage::ImageRgba8(i.clone())
        .thumbnail(side, side)
        .to_rgba8();
    let mut out = RgbaImage::new(side, side);
    image::imageops::overlay(
        &mut out,
        &resized,
        ((side - resized.width()) / 2) as i64,
        ((side - resized.height()) / 2) as i64,
    );
    out
}

fn png(i: &RgbaImage) -> Result<Vec<u8>, String> {
    let mut out = Cursor::new(Vec::new());
    i.write_to(&mut out, ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    Ok(out.into_inner())
}

/// A multi-resolution Windows ICO, with PNG frames from 16 through 256 pixels.
fn ico(i: &RgbaImage) -> Result<Vec<u8>, String> {
    let sizes = [16, 24, 32, 48, 64, 128, 256];
    let frames = sizes
        .iter()
        .map(|n| png(&square(i, *n)))
        .collect::<Result<Vec<_>, _>>()?;
    let mut out = vec![0, 0, 1, 0, 7, 0];
    let mut offset = 6 + 16 * sizes.len() as u32;
    for (n, b) in sizes.iter().zip(&frames) {
        out.extend_from_slice(&[if *n == 256 { 0 } else { *n as u8 }; 2]);
        out.extend_from_slice(&[0, 0, 1, 0, 32, 0]);
        out.extend_from_slice(&(b.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += b.len() as u32;
    }
    for f in frames {
        out.extend(f);
    }
    Ok(out)
}

pub(crate) fn import(root: &Path, source: &Path, app: bool) -> Result<String, String> {
    let image = square(&read_image(source, 256)?, 256);
    let bytes = png(&image)?;
    let hash = format!("{:x}", Sha256::digest(&bytes));
    let name = format!("{}-{}.png", if app { "app" } else { "tray" }, &hash[..20]);
    let path = stored(root, &name).ok_or("Invalid icon name")?;
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    if app {
        crate::file_ops::atomic_write(&path.with_extension("ico"), &ico(&image)?)
            .map_err(|e| e.to_string())?;
    }
    crate::file_ops::atomic_write(&path, &bytes).map_err(|e| e.to_string())?;
    Ok(name)
}

pub(crate) fn launcher_icon(root: &Path, name: &str) -> Result<PathBuf, String> {
    if let Some(p) = stored(root, name) {
        let output = p.with_extension("ico");
        if !output.is_file() {
            crate::file_ops::atomic_write(&output, &ico(&square(&read_image(&p, 256)?, 256))?)
                .map_err(|e| e.to_string())?;
        }
        return Ok(output);
    }
    let path = root.join("icons/default-app.ico");
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    crate::file_ops::atomic_write(&path, include_bytes!("../../assets/ndm.ico"))
        .map_err(|e| e.to_string())?;
    Ok(path)
}

#[cfg(test)]
#[path = "icon_tests.rs"]
mod tests;
