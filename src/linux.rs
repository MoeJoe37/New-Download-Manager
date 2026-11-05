//! Linux stand-ins for `win.rs`: the same functions, done the freedesktop way (xdg-open,
//! ~/.config/autostart, the browsers' NativeMessagingHosts folders, fontconfig, shared-mime-info and the
//! icon theme, NetworkManager for dial-up / VPN, GNOME's proxy settings, Firefox's cache), with GTK on its
//! own thread for the tray, the drop target and the font / program choosers.
// The title bars are the window manager's: dark_title / own_dialogs have nothing to do here.
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{mpsc, Mutex, OnceLock};

#[path = "platform/linux/desktop.rs"]
mod desktop;
#[path = "platform/linux/drag_drop.rs"]
mod drag_drop;
#[path = "platform/linux/network.rs"]
mod network;

use self::desktop::{config_dir, home};
pub use self::desktop::{
    install_desktop_entry, installed_browsers, native_host, register_host, set_autostart,
};
pub use self::drag_drop::{
    accept_drops, drop_target, drop_target_visible, keep_hidden, show_main, take_drops, take_show,
};
use self::network::gsettings;
pub use self::network::{
    nics, ras_connected, ras_dial, ras_entries, ras_hangup_all, ras_save, set_system_proxy,
    system_proxy,
};

pub type Rgba = (usize, usize, Vec<u8>);
pub use crate::posix::{clip_seq, decimal_sep, is_host_launch, pac_proxy, running};

// ---------- programs NDM starts ----------

/// What the AppImage's GTK hook sets for NDM's own GTK (bundled schemas, modules, pixbuf loaders, X11);
/// other programs need their own system's.
const APPIMAGE_ENV: [&str; 13] = [
    "GTK_DATA_PREFIX",
    "GTK_THEME",
    "GDK_BACKEND",
    "GSETTINGS_SCHEMA_DIR",
    "GI_TYPELIB_PATH",
    "GTK_EXE_PREFIX",
    "GTK_PATH",
    "GTK_IM_MODULE_FILE",
    "GDK_PIXBUF_MODULE_FILE",
    "APPDIR",
    "APPIMAGE",
    "ARGV0",
    "OWD",
];

/// A program to run with the environment it would have outside the AppImage.
fn cmd(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut c = Command::new(program);
    clean_env(&mut c);
    c
}

/// Takes the AppImage's own settings out of a child's environment (nothing to do outside an AppImage).
pub fn clean_env(c: &mut Command) {
    let Some(dir) = std::env::var_os("APPDIR") else {
        return;
    };
    for k in APPIMAGE_ENV {
        c.env_remove(k);
    }
    if let Some(dirs) = std::env::var_os("XDG_DATA_DIRS") {
        let kept: Vec<PathBuf> = std::env::split_paths(&dirs)
            .filter(|d| !d.starts_with(&dir))
            .collect();
        if let Ok(v) = std::env::join_paths(kept) {
            c.env("XDG_DATA_DIRS", v);
        }
    }
}

// ---------- GTK thread ----------

/// GTK lives on one thread: started on first use, it runs GTK's main loop for good. False = no display.
fn gtk_ready() -> bool {
    static READY: OnceLock<bool> = OnceLock::new();
    *READY.get_or_init(|| {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            // like the main window (see `prefer_x11`): on Wayland GTK can't keep the drop target on top or place it
            if prefer_x11() {
                gdk::set_allowed_backends("x11");
            }
            let ok = gtk::init().is_ok();
            let _ = tx.send(ok);
            if ok {
                gtk::main();
            }
        });
        rx.recv().unwrap_or(false)
    })
}

/// X11 (XWayland on a Wayland desktop) whenever DISPLAY is set: on Wayland a window can't hide itself
/// or stay on top. Without DISPLAY, both NDM and GTK stay on Wayland.
pub fn prefer_x11() -> bool {
    std::env::var_os("DISPLAY").is_some_and(|d| !d.is_empty())
}

/// Run `f` on the GTK thread without waiting. False when there is no GTK.
pub fn gtk_spawn(f: impl FnOnce() + Send + 'static) -> bool {
    if !gtk_ready() {
        return false;
    }
    glib::MainContext::default().invoke(f);
    true
}

/// Run `f` on the GTK thread and wait for its result (never from the GTK thread itself).
fn gtk_run<R: Send + 'static>(f: impl FnOnce() -> R + Send + 'static) -> Option<R> {
    let (tx, rx) = mpsc::channel();
    let sent = gtk_spawn(move || {
        let _ = tx.send(f());
    });
    if sent {
        rx.recv().ok()
    } else {
        None
    }
}

// ---------- file types and icons ----------

/// The icon theme's picture for `icon` at 32 px (Windows' large shell icon), as RGBA. GTK thread only.
fn themed(icon: &gio::Icon) -> Option<Rgba> {
    let info =
        gtk::IconTheme::default()?.lookup_by_gicon(icon, 32, gtk::IconLookupFlags::FORCE_SIZE)?;
    let pb = info.load_icon().ok()?;
    let pb = if pb.has_alpha() {
        pb
    } else {
        pb.add_alpha(false, 0, 0, 0).ok()?
    };
    let (w, h, stride) = (
        pb.width() as usize,
        pb.height() as usize,
        pb.rowstride() as usize,
    );
    let bytes = pb.read_pixel_bytes();
    let px = (0..h)
        .flat_map(|y| bytes[y * stride..y * stride + w * 4].iter().copied())
        .collect();
    Some((w, h, px))
}

/// shared-mime-info's type for a file name with this extension ("zip" -> "application/zip").
fn content_type(ext: &str) -> glib::GString {
    gio::content_type_guess(Some(format!("file.{ext}").as_str()), &[]).0
}

/// The icon theme's icon for files with this extension (or for a folder).
pub fn shell_icon(ext: &str, folder: bool) -> Option<Rgba> {
    let ext = ext.to_string();
    gtk_run(move || {
        let icon: gio::Icon = if folder {
            gio::ThemedIcon::new("folder").upcast()
        } else {
            gio::content_type_get_icon(&content_type(&ext))
        };
        themed(&icon)
    })
    .flatten()
}

/// shared-mime-info's description of a file extension, e.g. "Zip archive".
pub fn type_name(ext: &str) -> String {
    if ext.is_empty() {
        return String::new();
    }
    gio::content_type_get_description(&content_type(ext)).to_string()
}

/// The icon of the program's desktop entry (the browsers on the Options pages).
pub fn exe_icon(path: &Path) -> Option<Rgba> {
    let name = path.file_name()?.to_os_string();
    gtk_run(move || {
        let app = gio::AppInfo::all()
            .into_iter()
            .find(|a| a.executable().file_name() == Some(&name))?;
        themed(&app.icon()?)
    })
    .flatten()
}

// ---------- browser cache ----------

/// Firefox's cached copy of `url` (Windows reads the WinINet cache instead): the profiles in ~/.cache, also
/// Snap's.
pub fn url_cache(url: &str) -> Option<(PathBuf, String)> {
    let home = home();
    crate::posix::firefox_url_cache(
        &[
            home.join(".cache/mozilla/firefox"),
            home.join("snap/firefox/common/.cache/mozilla/firefox"),
        ],
        url,
    )
}

// ---------- windows ----------

// The main window is shown and hidden through egui's viewport commands on Linux; there is no HWND (0).
pub static PUMP: AtomicBool = AtomicBool::new(false);
pub fn pump(_hwnd: isize) {}
pub fn wake(_hwnd: isize) {}
pub fn hide(_hwnd: isize) {}
pub fn visible(_hwnd: isize) -> bool {
    true
}
pub fn own_dialogs(_main: isize, _dark: bool, _retitle: bool, _light: &[String]) {}
pub fn dark_title(_hwnd: isize, _dark: bool) {}

/// A tray on the desktop: a StatusNotifierItem host on the session bus (KDE, Xfce, Cinnamon, MATE,
/// GNOME with the AppIndicator extension). Without one a tray icon would be invisible.
pub fn tray_host() -> bool {
    dbus_call(
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        "NameHasOwner",
        ("org.kde.StatusNotifierWatcher",).to_variant(),
    )
    .and_then(|v| v.child_value(0).get::<bool>())
    .unwrap_or(false)
}

/// A method call on the session bus (gio, so no `gdbus` program is needed); None without a bus or on error.
fn dbus_call(
    dest: &str,
    path: &str,
    iface: &str,
    method: &str,
    args: glib::Variant,
) -> Option<glib::Variant> {
    static BUS: OnceLock<Option<gio::DBusConnection>> = OnceLock::new();
    let bus = BUS
        .get_or_init(|| gio::bus_get_sync(gio::BusType::Session, None::<&gio::Cancellable>).ok())
        .as_ref()?;
    bus.call_sync(
        Some(dest),
        path,
        iface,
        method,
        Some(&args),
        None,
        gio::DBusCallFlags::NONE,
        3000,
        None::<&gio::Cancellable>,
    )
    .ok()
}

/// Does the tray library load? tray-icon opens libayatana-appindicator3 (or the older libappindicator3)
/// at run time and panics, taking NDM down, when neither is installed.
pub fn indicator_lib() -> bool {
    static OK: OnceLock<bool> = OnceLock::new();
    *OK.get_or_init(|| {
        [
            "libayatana-appindicator3.so.1",
            "libappindicator3.so.1",
            "libayatana-appindicator3.so",
            "libappindicator3.so",
        ]
        .iter()
        .any(|name| {
            let c = std::ffi::CString::new(*name).unwrap();
            // SAFETY: dlopen with a valid C string; the handle is kept open, as tray-icon opens the same library
            !unsafe { libc::dlopen(c.as_ptr(), libc::RTLD_LAZY) }.is_null()
        })
    })
}

/// A sound file through PulseAudio, PipeWire or ALSA, whichever player is installed.
pub fn play_sound(path: &str) {
    if !Path::new(path).is_file() {
        return;
    }
    for (player, quiet) in [("paplay", None), ("pw-play", None), ("aplay", Some("-q"))] {
        if cmd(player)
            .args(quiet)
            .arg(path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .is_ok()
        {
            return;
        }
    }
}

/// GNOME / GTK's dark preference.
/// The desktop's dark style, asked again on every call (the UI checks about every 2 s): the desktop
/// portal's color-scheme (1 = dark; GNOME, KDE, Xfce...), else GNOME's settings.
pub fn system_dark() -> bool {
    let portal = dbus_call(
        "org.freedesktop.portal.Desktop",
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.Settings",
        "Read",
        ("org.freedesktop.appearance", "color-scheme").to_variant(),
    )
    .and_then(|r| {
        // (v), the v holding a u, or on older portals another v around it
        let mut v = r.child_value(0);
        while let Some(inner) = v.as_variant() {
            v = inner;
        }
        v.get::<u32>()
    });
    match portal {
        Some(1) => true,
        Some(2) => false,
        _ => {
            let get = |key: &str| {
                gsettings(&["get", "org.gnome.desktop.interface", key])
                    .unwrap_or_default()
                    .to_lowercase()
            };
            get("color-scheme").contains("dark") || get("gtk-theme").contains("dark")
        }
    }
}

// ---------- proxy ----------

// ---------- Dial Up / VPN: NetworkManager's connections through nmcli ----------

// ---------- programs and the desktop ----------

/// When NDM can't start at all: on the terminal, and in a desktop dialog when zenity or kdialog is there.
pub fn fatal(title: &str, text: &str) {
    eprintln!("{text}");
    let _ = cmd("zenity")
        .args(["--error", "--title", title, "--text", text])
        .status()
        .or_else(|_| {
            cmd("kdialog")
                .args(["--title", title, "--error", text])
                .status()
        });
}

/// "Exit NDM" / "Saving settings..." while the app stops its downloads and saves; it goes away with the
/// process.
pub fn exit_splash(title: &str, text: &str) {
    let (title, text) = (title.to_string(), text.to_string());
    gtk_run(move || {
        let w = gtk::Window::new(gtk::WindowType::Toplevel);
        w.set_title(&title);
        w.set_position(gtk::WindowPosition::Center);
        w.set_resizable(false);
        w.set_keep_above(true);
        let l = gtk::Label::new(Some(&text));
        l.set_margin(24);
        w.add(&l);
        w.show_all();
        while gtk::events_pending() {
            gtk::main_iteration();
        }
        std::mem::forget(w);
    });
}

/// GTK's font chooser. Returns (face, size in logical px) like Windows' Font dialog; blocks like it.
pub fn choose_font(_owner: isize, face: &str, px: f32) -> Option<(String, f32)> {
    let start = format!("{face} {}", (px * 72.0 / 96.0).round());
    gtk_run(move || {
        let d = gtk::FontChooserDialog::new(Some("Font"), None::<&gtk::Window>);
        d.set_font(&start);
        let ok = d.run() == gtk::ResponseType::Ok;
        let pick = d.font_desc().filter(|_| ok).map(|f| {
            (
                f.family().map(|s| s.to_string()).unwrap_or_default(),
                f.size() as f32 / gtk::pango::SCALE as f32 * 96.0 / 72.0,
            )
        });
        // SAFETY: the dialog is ours and nothing uses it after this
        unsafe { d.destroy() };
        pick.filter(|p| !p.0.is_empty() && p.1 > 0.0)
    })
    .flatten()
}

/// The file of an installed font face, from fontconfig.
pub fn font_file(face: &str) -> Option<PathBuf> {
    let o = cmd("fc-match")
        .args(["-f", "%{file}", face])
        .output()
        .ok()?;
    let f = PathBuf::from(String::from_utf8_lossy(&o.stdout).trim());
    f.is_file().then_some(f)
}

/// "Open with...": GTK's program chooser for the file's type; the default program without GTK.
pub fn open_with(path: &Path) {
    let p = path.to_path_buf();
    let shown = gtk_spawn({
        let p = p.clone();
        move || {
            let file = gio::File::for_path(&p);
            let d =
                gtk::AppChooserDialog::new(None::<&gtk::Window>, gtk::DialogFlags::MODAL, &file);
            if d.run() == gtk::ResponseType::Ok {
                if let Some(app) = d.app_info() {
                    let _ = app.launch(&[file], None::<&gio::AppLaunchContext>);
                }
            }
            // SAFETY: the dialog is ours and nothing uses it after this
            unsafe { d.destroy() };
        }
    });
    if !shown {
        open(&p);
    }
}

/// The file manager with the file selected (FileManager1 on the session bus: Nautilus, Dolphin, Nemo,
/// Caja, Thunar), else its folder.
pub fn show_in_folder(path: &Path) {
    let uri = gio::File::for_path(path).uri().to_string();
    let shown = dbus_call(
        "org.freedesktop.FileManager1",
        "/org/freedesktop/FileManager1",
        "org.freedesktop.FileManager1",
        "ShowItems",
        (vec![uri], "").to_variant(),
    )
    .is_some();
    if !shown {
        open(path.parent().unwrap_or(path));
    }
}

/// Opens a file, folder or address with the desktop's default program.
pub fn open(target: impl AsRef<std::ffi::OsStr>) {
    let _ = cmd("xdg-open")
        .arg(target)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

/// "Turn off computer when done": shut down in a minute (`shutdown -c` cancels it), suspend or hibernate.
pub fn power_off(kind: u8, _force: bool) {
    let _ = match kind {
        1 => cmd("systemctl").arg("suspend").spawn(),
        2 => cmd("systemctl").arg("hibernate").spawn(),
        _ => cmd("shutdown")
            .args([
                "-h",
                "+1",
                "Downloads complete. The computer will shut down in a minute.",
            ])
            .spawn(),
    };
}

// ---------- packages ----------

/// The package format that installed `exe` ("deb" or "rpm"), None when no package owns it (the
/// AppImage, the .tar.gz).
pub fn package_owner(exe: &Path) -> Option<&'static str> {
    let exe = exe.canonicalize().ok()?;
    let owns = |p: &str, args: &[&str]| {
        on_path(p)
            && cmd(p)
                .args(args)
                .arg(&exe)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
    };
    if owns("dpkg-query", &["-S"]) {
        Some("deb")
    } else if owns("rpm", &["-qf"]) {
        Some("rpm")
    } else {
        None
    }
}

fn on_path(program: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(program).is_file()))
}

/// Installs a downloaded .deb or .rpm with the system's package manager, as root: pkexec has the
/// desktop ask for the password. apt, dnf and zypper also bring in what it depends on.
pub fn install_package(file: &Path) -> Result<(), String> {
    let f = file.display().to_string();
    let args: Vec<&str> = if f.ends_with(".deb") {
        if on_path("apt-get") {
            vec!["apt-get", "install", "-y", "--allow-downgrades", &f]
        } else {
            vec!["dpkg", "-i", &f]
        }
    } else if on_path("dnf") {
        vec!["dnf", "install", "-y", &f]
    } else if on_path("zypper") {
        vec![
            "zypper",
            "-n",
            "--no-gpg-checks",
            "install",
            "--allow-unsigned-rpm",
            &f,
        ]
    } else {
        vec!["rpm", "-U", &f]
    };
    if !on_path("pkexec") {
        return Err(format!(
            "pkexec is missing: install the update by hand with {}",
            args.join(" ")
        ));
    }
    let o = cmd("pkexec")
        .args(&args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    match o.status.code() {
        Some(0) => Ok(()),
        // pkexec: the password dialog was closed, or the user may not install software
        Some(126 | 127) => Err("The update wasn't installed: the password wasn't given.".into()),
        _ => Err(String::from_utf8_lossy(&o.stderr)
            .lines()
            .rfind(|l| !l.trim().is_empty())
            .unwrap_or("the package manager failed")
            .to_string()),
    }
}
