//! macOS stand-ins for `win.rs`: the same functions, done the Mac way (`open`, a LaunchAgent, the browsers'
//! NativeMessagingHosts folders in ~/Library/Application Support, `scutil` for the proxy and VPNs, `afplay`,
//! `netstat` for the networks, AppleScript for the font and program choosers and for messages). The tray
//! icon is the menu bar item (main.rs, `tray`); what Linux and macOS share is in `posix.rs`.
// The title bars, the dialogs' owner and file type icons are AppKit's: nothing to do for them here.
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool, Imp, Sel};
use objc2::{define_class, msg_send, sel, AllocAnyThread, DefinedClass, MainThreadMarker, MainThreadOnly};
// NSFilenamesPboardType: what winit's main window takes (files), so NDM hands those on
#[allow(deprecated)]
use objc2_app_kit::{
    NSBitmapImageRep, NSColor, NSCompositingOperation, NSDeviceRGBColorSpace, NSDragOperation, NSEvent, NSFilenamesPboardType, NSFloatingWindowLevel, NSGraphicsContext, NSImage, NSPanel, NSPasteboard,
    NSPasteboardTypeString, NSPasteboardTypeURL, NSResponder, NSScreen, NSView, NSWindowCollectionBehavior, NSWindowStyleMask, NSBackingStoreType, NSWorkspace,
};
use objc2_foundation::{NSArray, NSObject, NSPoint, NSRect, NSSize, NSString, NSUInteger};

#[path = "platform/macos/desktop.rs"]
mod desktop;
#[path = "platform/macos/network.rs"]
mod network;
#[path = "platform/macos/drag_drop.rs"]
mod drag_drop;

pub use self::desktop::{install_desktop_entry, installed_browsers, native_host, register_host, set_autostart};
pub use self::network::{nics, ras_connected, ras_dial, ras_entries, ras_hangup_all, ras_save, system_proxy};
pub use self::drag_drop::{accept_drops, drop_target, drop_target_visible, show_main, take_drops, take_show};

pub type Rgba = (usize, usize, Vec<u8>);
pub use crate::posix::{clip_seq, decimal_sep, is_host_launch, pac_proxy, running};

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| ".".into())
}

/// A program's output, None when it can't run or fails.
fn output(program: &str, args: &[&str]) -> Option<String> {
    let o = Command::new(program).args(args).stdin(Stdio::null()).stderr(Stdio::null()).output().ok().filter(|o| o.status.success())?;
    Some(String::from_utf8_lossy(&o.stdout).into_owned())
}

/// An AppleScript's answer (osascript), None on error or Cancel.
fn applescript(lines: &[&str]) -> Option<String> {
    let mut c = Command::new("osascript");
    for l in lines {
        c.args(["-e", l]);
    }
    let o = c.stdin(Stdio::null()).stderr(Stdio::null()).output().ok().filter(|o| o.status.success())?;
    Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// A string for AppleScript, in quotes.
fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

// ---------- programs NDM starts ----------

/// Nothing to take out of a child's environment on macOS.
pub fn clean_env(_c: &mut Command) {}

/// There is no X11 on macOS.
pub fn prefer_x11() -> bool {
    false
}

/// The main window goes to the menu bar on its second frame (main.rs): nothing to hold back here.
pub fn keep_hidden(_window: isize, _on: bool) {}

// ---------- file types and icons ----------

/// The Finder's picture for a file type ("/" = a folder), 32 pixels.
pub fn shell_icon(ext: &str, folder: bool) -> Option<Rgba> {
    let kind = if folder { "public.folder" } else if ext.is_empty() { "public.data" } else { ext };
    #[allow(deprecated)] // iconForContentType needs UniformTypeIdentifiers; this takes extensions and UTIs
    let img = NSWorkspace::sharedWorkspace().iconForFileType(&NSString::from_str(kind));
    image_rgba(&img, 32)
}

/// A program's icon (an .app, or the app bundle a program sits in).
pub fn exe_icon(path: &Path) -> Option<Rgba> {
    let app = path.ancestors().find(|p| p.extension().is_some_and(|e| e == "app")).unwrap_or(path);
    if !app.exists() {
        return None;
    }
    let img = NSWorkspace::sharedWorkspace().iconForFile(&NSString::from_str(&app.to_string_lossy()));
    image_rgba(&img, 32)
}

/// An NSImage drawn at `px` x `px` pixels, as straight (not premultiplied) RGBA rows from the top.
fn image_rgba(img: &NSImage, px: usize) -> Option<Rgba> {
    let rep = unsafe { NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(NSBitmapImageRep::alloc(), std::ptr::null_mut(), px as isize, px as isize, 8, 4, true, false, NSDeviceRGBColorSpace, (px * 4) as isize, 32) }?;
    let ctx = NSGraphicsContext::graphicsContextWithBitmapImageRep(&rep)?;
    NSGraphicsContext::saveGraphicsState_class();
    NSGraphicsContext::setCurrentContext(Some(&ctx));
    let all = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(px as f64, px as f64));
    img.drawInRect_fromRect_operation_fraction(all, NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0)), NSCompositingOperation::Copy, 1.0);
    ctx.flushGraphics();
    NSGraphicsContext::restoreGraphicsState_class();
    let data = rep.bitmapData();
    if data.is_null() {
        return None;
    }
    let row = rep.bytesPerRow() as usize;
    let bytes = unsafe { std::slice::from_raw_parts(data, row * px) };
    let mut out = Vec::with_capacity(px * px * 4);
    for y in 0..px {
        for p in bytes[y * row..y * row + px * 4].as_chunks::<4>().0 {
            let a = p[3] as u32;
            let un = |c: u8| (c as u32 * 255 + a / 2).checked_div(a).unwrap_or(0).min(255) as u8;
            out.extend([un(p[0]), un(p[1]), un(p[2]), p[3]]);
        }
    }
    (out.iter().skip(3).step_by(4).any(|a| *a != 0)).then_some((px, px, out))
}

/// An NSImage of straight RGBA pixels, shown at `size` points.
fn image_of((w, h, px): &Rgba, size: f64) -> Option<Retained<NSImage>> {
    let rep = unsafe { NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(NSBitmapImageRep::alloc(), std::ptr::null_mut(), *w as isize, *h as isize, 8, 4, true, false, NSDeviceRGBColorSpace, (*w * 4) as isize, 32) }?;
    let data = rep.bitmapData();
    if data.is_null() || px.len() < w * h * 4 {
        return None;
    }
    let row = rep.bytesPerRow() as usize;
    for y in 0..*h {
        let dst = unsafe { std::slice::from_raw_parts_mut(data.add(y * row), w * 4) };
        for (d, s) in dst.as_chunks_mut::<4>().0.iter_mut().zip(px[y * w * 4..(y + 1) * w * 4].as_chunks::<4>().0) {
            let a = s[3] as u32;
            let pre = |c: u8| ((c as u32 * a + 127) / 255) as u8;
            d.copy_from_slice(&[pre(s[0]), pre(s[1]), pre(s[2]), s[3]]);
        }
    }
    rep.setSize(NSSize::new(size, size));
    let img = NSImage::initWithSize(NSImage::alloc(), NSSize::new(size, size));
    img.addRepresentation(&rep);
    Some(img)
}

/// "ZIP file" for an extension.
pub fn type_name(ext: &str) -> String {
    if ext.is_empty() {
        return String::new();
    }
    crate::i18n::trf("%s file", &[&ext.to_uppercase()])
}

// ---------- browser cache ----------

/// Firefox's cached copy of `url` (~/Library/Caches/Firefox/Profiles).
pub fn url_cache(url: &str) -> Option<(PathBuf, String)> {
    crate::posix::firefox_url_cache(&[home().join("Library/Caches/Firefox/Profiles")], url)
}

// ---------- windows ----------

// The main window is shown and hidden through egui's viewport commands; there is no HWND (0).
pub static PUMP: AtomicBool = AtomicBool::new(false);
pub fn pump(_hwnd: isize) {}
pub fn wake(_hwnd: isize) {}
pub fn hide(_hwnd: isize) {}
pub fn visible(_hwnd: isize) -> bool {
    true
}
pub fn own_dialogs(_main: isize, _dark: bool, _retitle: bool, _light: &[String]) {}
pub fn dark_title(_hwnd: isize, _dark: bool) {}

/// A sound file through afplay.
pub fn play_sound(path: &str) {
    if Path::new(path).is_file() {
        let _ = Command::new("afplay").arg(path).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
    }
}

/// Dark mode (System Settings -> Appearance), asked again at most every 3 s.
pub fn system_dark() -> bool {
    static LAST: Mutex<Option<(std::time::Instant, bool)>> = Mutex::new(None);
    let mut last = LAST.lock().unwrap();
    if let Some((at, dark)) = *last {
        if at.elapsed() < std::time::Duration::from_secs(3) {
            return dark;
        }
    }
    let dark = output("defaults", &["read", "-g", "AppleInterfaceStyle"]).is_some_and(|s| s.trim().eq_ignore_ascii_case("dark"));
    *last = Some((std::time::Instant::now(), dark));
    dark
}

// ---------- proxy ----------

// ---------- Dial Up / VPN: the system's VPN services through scutil --nc ----------

// ---------- programs and the desktop ----------

/// When NDM can't start at all: an alert.
pub fn fatal(title: &str, text: &str) {
    eprintln!("{text}");
    let _ = applescript(&[&format!("display alert {} message {} as critical", quote(title), quote(text))]);
}

/// "Exit NDM" / "Saving settings...": macOS shows nothing while the app saves (it takes a moment).
pub fn exit_splash(_title: &str, _text: &str) {}

/// The font families installed (from the font files' names).
fn font_families() -> Vec<String> {
    let mut names: Vec<String> = font_dirs()
        .iter()
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flatten()
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            let ext = p.extension()?.to_str()?.to_ascii_lowercase();
            ["ttf", "otf", "ttc"].contains(&ext.as_str()).then(|| p.file_stem()?.to_str().map(|s| s.split('-').next().unwrap_or(s).to_string()))?
        })
        .collect();
    names.sort_by_key(|n| n.to_lowercase());
    names.dedup();
    names
}

fn font_dirs() -> Vec<PathBuf> {
    vec!["/System/Library/Fonts".into(), "/System/Library/Fonts/Supplemental".into(), "/Library/Fonts".into(), home().join("Library/Fonts")]
}

/// A font and its size, picked in two AppleScript choosers. Returns (face, size in logical px) like
/// Windows' Font dialog; blocks like it.
pub fn choose_font(_owner: isize, face: &str, px: f32) -> Option<(String, f32)> {
    let list = font_families().iter().map(|f| quote(f)).collect::<Vec<_>>().join(", ");
    let face = applescript(&[&format!("choose from list {{{list}}} with prompt \"Font\" default items {{{}}}", quote(face))]).filter(|f| f != "false" && !f.is_empty())?;
    let pt = (px * 72.0 / 96.0).round();
    let size = applescript(&[&format!("text returned of (display dialog \"Size\" default answer \"{pt}\")")])?;
    let pt: f32 = size.trim().parse().ok().filter(|p: &f32| *p > 0.0)?;
    Some((face, pt * 96.0 / 72.0))
}

/// The file of an installed font: a face by name ("Helvetica", "Tahoma"), or the generic names NDM asks
/// for: "sans-serif", "sans-serif:lang=ar" (a font with Arabic letters) and "DejaVu Sans" (symbols).
pub fn font_file(face: &str) -> Option<PathBuf> {
    let candidates: &[&str] = match face {
        "sans-serif" => &["/System/Library/Fonts/Helvetica.ttc", "/System/Library/Fonts/SFNS.ttf"],
        "sans-serif:lang=ar" => &["/System/Library/Fonts/GeezaPro.ttc", "/System/Library/Fonts/Supplemental/GeezaPro.ttc", "/System/Library/Fonts/Supplemental/Arial Unicode.ttf"],
        "DejaVu Sans" => &["/System/Library/Fonts/Supplemental/Arial Unicode.ttf", "/Library/Fonts/Arial Unicode.ttf", "/System/Library/Fonts/Apple Symbols.ttf"],
        _ => &[],
    };
    if let Some(f) = candidates.iter().map(PathBuf::from).find(|p| p.is_file()) {
        return Some(f);
    }
    let want = face.to_lowercase().replace(' ', "");
    font_dirs().iter().filter_map(|d| std::fs::read_dir(d).ok()).flatten().flatten().map(|e| e.path()).find(|p| {
        let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        ["ttf", "otf", "ttc"].contains(&ext.as_str()) && p.file_stem().and_then(|s| s.to_str()).is_some_and(|s| s.to_lowercase().replace(' ', "") == want)
    })
}

/// "Open with...": the system's program chooser, then the program opens the file.
pub fn open_with(path: &Path) {
    let p = path.to_path_buf();
    std::thread::spawn(move || {
        if let Some(app) = applescript(&["POSIX path of (choose application as alias)"]) {
            let _ = Command::new("open").arg("-a").arg(app.trim()).arg(&p).spawn();
        }
    });
}

/// Finder with the file selected.
pub fn show_in_folder(path: &Path) {
    let _ = Command::new("open").arg("-R").arg(path).spawn();
}

/// Opens a file, folder or address with its default program.
pub fn open(target: impl AsRef<std::ffi::OsStr>) {
    let _ = Command::new("open").arg(target).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
}

/// "Turn off computer when done": shut down (asks the open programs to quit first), or sleep.
pub fn power_off(kind: u8, _force: bool) {
    match kind {
        0 => {
            let _ = applescript(&["tell application \"System Events\" to shut down"]);
        }
        _ => {
            let _ = Command::new("pmset").arg("sleepnow").spawn();
        }
    }
}

#[test]
fn file_type_icons() {
    for (ext, folder) in [("pdf", false), ("zip", false), ("", false), ("/", true)] {
        let (w, h, px) = shell_icon(ext, folder).unwrap_or_else(|| panic!("no icon for {ext:?}"));
        assert_eq!((w, h, px.len()), (32, 32, 32 * 32 * 4));
        assert!(px.as_chunks::<4>().0.iter().filter(|p| p[3] > 200).count() > 100, "{ext:?}: mostly see-through");
    }
    assert!(exe_icon(Path::new("/System/Applications/TextEdit.app")).or_else(|| exe_icon(Path::new("/Applications/Safari.app"))).is_some());
    // NDM's own picture survives the trip into an NSImage (the drop target) and back
    let red = (2, 2, [255u8, 0, 0, 255].repeat(4));
    let (_, _, px) = image_rgba(&image_of(&red, 2.0).unwrap(), 4).unwrap();
    assert!(px.as_chunks::<4>().0.iter().all(|p| p[0] > 240 && p[1] < 16 && p[3] == 255), "{px:?}");
}

// ---------- networks ----------

// ---------- packages ----------

/// NDM on macOS is an app bundle, never a package.
pub fn package_owner(_exe: &Path) -> Option<&'static str> {
    None
}

pub fn install_package(_file: &Path) -> Result<(), String> {
    Err("not on macOS".into())
}
