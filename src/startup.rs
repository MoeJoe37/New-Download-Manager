use crate::app::{resources::app_icon, App};
use crate::update::UPDATED;
use crate::{core, diagnostics, win};
use eframe::egui::{self, ViewportBuilder};
#[cfg(target_os = "macos")]
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;
use std::time::Duration;

pub(crate) const TITLE: &str = "New Download Manager 1.0";

/// Started by the browser extension (`/tray`, `win::native_host`): only the tray icon.
pub(crate) fn tray_start() -> bool {
    std::env::args().any(|a| a.eq_ignore_ascii_case("/tray"))
}

pub(crate) fn start() -> eframe::Result {
    diagnostics::install_hook();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if win::is_host_launch(&args) {
        win::native_host(TITLE);
        return Ok(());
    }
    if args.first().is_some_and(|a| a == UPDATED) || std::env::var_os("NDM_RENDERER").is_some() {
        // started by Update now, or again with wgpu (`run`): wait for the old copy to finish saving and exit
        // (15 s at most)
        for _ in 0..75 {
            if !win::running(TITLE, false) {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::fs::remove_file(exe.with_extension("old.exe")); // left behind by an update
                                                                     // macOS: the app bundle an update replaced
        #[cfg(target_os = "macos")]
        if let Some(bundle) = exe
            .ancestors()
            .find(|a| a.extension().is_some_and(|e| e == "app"))
        {
            let _ = std::fs::remove_dir_all(bundle.with_extension("old"));
        }
    }
    // a Mac app started from the Finder or the Dock gets a short PATH: Homebrew's and MacPorts' programs
    // (ffmpeg for mms / rtmp / rtsp streams) are found there too
    #[cfg(target_os = "macos")]
    {
        let mut path: Vec<PathBuf> =
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).collect();
        for d in ["/opt/homebrew/bin", "/usr/local/bin", "/opt/local/bin"] {
            if !path.iter().any(|p| p == Path::new(d)) {
                path.push(d.into());
            }
        }
        if let Ok(p) = std::env::join_paths(path) {
            std::env::set_var("PATH", p); // no other thread runs yet
        }
    }
    // the AppImage's GTK hook asks for X11; on a Wayland desktop without XWayland GTK would then fail
    #[cfg(target_os = "linux")]
    if !win::prefer_x11() && std::env::var_os("GDK_BACKEND").is_some_and(|b| b == "x11") {
        std::env::remove_var("GDK_BACKEND"); // no other thread runs yet
    }
    let cli = core::browser_bridge::cli_args(&args);
    // a second start for the browser (/tray) mustn't bring the window out of the tray
    if win::running(TITLE, !tray_start()) {
        // hand the command line to the running copy through its local endpoint
        if let Some(i) = cli {
            let body = serde_json::json!({ "url": i.url, "filename": i.filename, "force": true, "cli": true, "folder": i.folder,
                "silent": i.silent, "later": i.later, "quit": i.quit, "start_queue": i.start_queue });
            if let Some(port) = core::browser_bridge::running_port() {
                let _ = ureq::post(&format!("http://127.0.0.1:{port}/add"))
                    .set("X-NDM", "1")
                    .send_string(&body.to_string());
            }
        }
        return Ok(());
    }
    win::register_host(core::state::store_path().parent().unwrap());
    #[cfg(not(windows))]
    win::install_desktop_entry();
    let (w, h, rgba) = app_icon();
    let viewport = ViewportBuilder::default()
        .with_title(TITLE)
        .with_active(!tray_start())
        .with_inner_size([1000.0, 560.0])
        .with_min_inner_size([640.0, 320.0])
        .with_icon(Arc::new(egui::IconData {
            rgba,
            width: w as u32,
            height: h as u32,
        }));
    // the X11 class / Wayland app id docks match to ndm.desktop (StartupWMClass=ndm)
    #[cfg(not(windows))]
    let viewport = viewport.with_app_id("ndm");
    #[allow(unused_mut)]
    let mut opts = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    #[cfg(target_os = "linux")]
    if win::prefer_x11() {
        opts.event_loop_builder = Some(Box::new(|b| {
            use winit::platform::x11::EventLoopBuilderExtX11;
            b.with_x11();
        }));
    }
    run(opts, &args)
}

/// Opens the main window: with OpenGL, which every desktop has; where it can't start (a virtual machine or
/// a remote session without a graphics driver) NDM starts again with wgpu: Direct3D 12 (with Windows'
/// software renderer when there is no graphics card), Vulkan or Metal. That choice is kept next to the
/// settings ("renderer"), so later starts go straight to it.
fn run(mut opts: eframe::NativeOptions, args: &[String]) -> eframe::Result {
    let marker = core::state::store_path().with_file_name("renderer");
    let wgpu = std::env::var_os("NDM_RENDERER").is_some_and(|v| v == "wgpu")
        || std::fs::read_to_string(&marker).is_ok_and(|s| s.trim() == "wgpu");
    opts.renderer = if wgpu {
        eframe::Renderer::Wgpu
    } else {
        eframe::Renderer::Glow
    };
    // OpenGL is what didn't start: wgpu's own OpenGL backend would fail the same way (or panic), so only
    // Direct3D 12, Vulkan and Metal
    if let eframe::egui_wgpu::WgpuSetup::CreateNew(c) = &mut opts.wgpu_options.wgpu_setup {
        if std::env::var_os("WGPU_BACKEND").is_none() {
            c.instance_descriptor.backends = eframe::wgpu::Backends::PRIMARY;
        }
        // a graphics card if there is one, else a software renderer (Windows' WARP: computers without a
        // working GPU driver), which wgpu's own choice leaves out
        c.native_adapter_selector = Some(Arc::new(
            |adapters: &[eframe::wgpu::Adapter], surface: Option<&eframe::wgpu::Surface<'_>>| {
                use eframe::wgpu::DeviceType::*;
                let rank = |a: &eframe::wgpu::Adapter| match a.get_info().device_type {
                    DiscreteGpu => 0,
                    IntegratedGpu => 1,
                    VirtualGpu => 2,
                    Other => 3,
                    Cpu => 4,
                };
                adapters
                    .iter()
                    .filter(|a| surface.is_none_or(|s| a.is_surface_supported(s)))
                    .min_by_key(|a| rank(a))
                    .cloned()
                    .ok_or_else(|| "no graphics adapter".to_string())
            },
        ));
    }
    // a renderer can also panic while it starts: before the first frame that is the renderer's failure too
    let started = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        eframe::run_native(
            TITLE,
            opts,
            Box::new(|cc| Ok(Box::new(App::new(&cc.egui_ctx)))),
        )
    }));
    let error = match started {
        Ok(Ok(())) => return Ok(()),
        Ok(Err(e)) => e.to_string(),
        Err(p) if !PAINTED.load(Relaxed) => {
            // the panic hook's crash.log is the renderer's, not a crash of NDM's: kept under its own name
            let log = core::state::store_path().with_file_name("crash.log");
            let _ = std::fs::rename(&log, log.with_file_name("renderer-crash.log"));
            p.downcast_ref::<String>()
                .cloned()
                .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_else(|| "panic".into())
        }
        Err(p) => std::panic::resume_unwind(p),
    };
    if !wgpu {
        eprintln!("OpenGL didn't start ({error}): starting again with wgpu");
        let _ = std::fs::create_dir_all(marker.parent().unwrap());
        let _ = std::fs::write(&marker, "wgpu\n");
        // an AppImage's files go away with this process: start the AppImage itself
        let exe = std::env::var_os("APPIMAGE")
            .map(PathBuf::from)
            .or_else(|| std::env::current_exe().ok());
        if exe.is_some_and(|exe| {
            std::process::Command::new(exe)
                .args(args)
                .env("NDM_RENDERER", "wgpu")
                .spawn()
                .is_ok()
        }) {
            return Ok(());
        }
    }
    // neither works: say so (a window can't be drawn), and try OpenGL again next time
    let _ = std::fs::remove_file(&marker);
    let _ = std::fs::write(
        marker.with_file_name("renderer-error.log"),
        format!("{error}\n"),
    );
    win::fatal(TITLE, &format!("New Download Manager can't open its window: {error}\n\nUpdating the graphics driver usually fixes this."));
    Ok(())
}

/// The main window has drawn a frame: the renderer works (`run`).
pub(crate) static PAINTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
