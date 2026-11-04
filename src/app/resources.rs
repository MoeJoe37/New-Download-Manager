use super::*;

pub(super) const DARK_DLG: Color32 = Color32::from_rgb(0x39, 0x39, 0x39);

pub(in crate::app) const DARK_TREE: Color32 = Color32::from_rgb(0x20, 0x20, 0x20);

pub(in crate::app) const DARK_MENU_HOT: Color32 = Color32::from_rgb(0x50, 0x50, 0x50);

pub(in crate::app) const BAR: Color32 = Color32::from_rgb(38, 120, 220);

/// Segoe UI, the font of NDM's main window.
pub(in crate::app) fn segoe(size: f32) -> FontId {
    FontId::new(size, egui::FontFamily::Name("segoe".into()))
}

/// A texture made once and kept in egui's memory.
pub(in crate::app) fn cached_texture(ctx: &egui::Context, name: &str, make: impl FnOnce() -> Option<win::Rgba>) -> Option<egui::TextureHandle> {
    let id = egui::Id::new(("tex", name));
    if let Some(t) = ctx.data(|d| d.get_temp::<Option<egui::TextureHandle>>(id)) {
        return t;
    }
    let t = make().map(|px| texture(ctx, name, &px));
    ctx.data_mut(|d| d.insert_temp(id, t.clone()));
    t
}

pub(in crate::app) fn help_logo(ctx: &egui::Context) -> Option<egui::TextureHandle> {
    cached_texture(ctx, "help_logo", || Some(app_icon()))
}

/// The app's own icon (assets/ndm.png, 256 px with alpha; the .exe gets assets/ndm.ico via build.rs).
pub(crate) fn app_icon() -> win::Rgba {
    let i = eframe::icon_data::from_png_bytes(include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/ndm.png"))).expect("bundled icon");
    (i.width as usize, i.height as usize, i.rgba)
}

/// Tray picture for View -> "NDM tray icon": 1 = 3D style (the app icon), 2 = Classic style (assets/ndm2d.png).
pub(in crate::app) fn tray_picture(style: u8) -> Option<tray_icon::Icon> {
    let (w, h, rgba) = match style {
        2 => eframe::icon_data::from_png_bytes(include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/ndm2d.png"))).map(|i| (i.width as usize, i.height as usize, i.rgba)).ok()?,
        _ => app_icon(),
    };
    tray_icon::Icon::from_rgba(rgba, w as u32, h as u32).ok()
}

pub(in crate::app) fn texture(ctx: &egui::Context, name: &str, (w, h, px): &win::Rgba) -> egui::TextureHandle {
    ctx.load_texture(name, egui::ColorImage::from_rgba_unmultiplied([*w, *h], px), egui::TextureOptions::LINEAR)
}

/// NDM's toolbar strips (normal, hot, and a generated disabled one), 12 icons each.
/// One toolbar button's pictures in one size: normal, hovered, disabled. `tint`: white glyphs the
/// toolbar colours as it draws them (NDM Style).
struct TbPics {
    pics: [egui::TextureHandle; 3],
    tint: bool,
}

/// The toolbar's pictures, large and small, one per button in `TB_BUTTONS` order: the built-in glyphs, or
/// a custom toolbar's PNGs (README, Custom toolbars) with the glyphs standing in for any it lacks.
pub(in crate::app) struct Toolbar {
    large: Vec<Option<TbPics>>,
    small: Vec<Option<TbPics>>,
}

pub(in crate::app) const TB_ICONS: usize = 12;

/// RT_TOOLBAR 166 in NDM's order: (fallback glyph, label, tooltip).
pub(in crate::app) const TB_BUTTONS: [(&str, &str, &str); TB_ICONS] = [
    ("➕", "Add URL", "Add new download address manually"),
    ("▶", "Resume", "Start/Resume downloading selected file(s)"),
    ("⏹", "Stop", "Stop downloading selected file(s)"),
    ("⏸", "Stop All", "Stop all downloads"),
    ("🗑", "Delete", "Remove selected file(s) from the list"),
    ("✔", "Delete Co...", "Remove all completed files from the list"),
    ("⚙", "Options", "Browsers/System integration, File types, Proxy, Passwords and others"),
    ("⏰", "Scheduler", "Scheduler and download queues"),
    ("⏩", "Start Queue", "Start main download queue, or choose a queue to start from drop-down list"),
    ("⏏", "Stop Queue", "Stop main download queue, or choose a queue to stop from drop-down list"),
    ("🌐", "Grabber", "Run site grabber"),
    ("✉", "Tell a Friend", "Write a letter to your friend about New Download Manager"),
];

/// NDM's own icons: glyphs of Windows' icon font (Segoe Fluent Icons; Segoe MDL2 Assets on Windows 10),
/// drawn in colour into textures when NDM starts. Nothing is bundled and nothing comes from another program.
fn icon_font() -> Option<&'static ab_glyph::FontVec> {
    static FONT: std::sync::OnceLock<Option<ab_glyph::FontVec>> = std::sync::OnceLock::new();
    #[cfg(not(windows))]
    return FONT.get_or_init(|| ab_glyph::FontVec::try_from_vec(include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/linux/MaterialIcons-Regular.ttf")).to_vec()).ok()).as_ref();
    #[cfg(windows)]
    FONT.get_or_init(|| {
        let dir = Path::new(&std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into())).join("Fonts");
        ["SegoeIcons.ttf", "segmdl2.ttf"].iter().find_map(|f| std::fs::read(dir.join(f)).ok()).and_then(|b| ab_glyph::FontVec::try_from_vec(b).ok())
    })
    .as_ref()
}

/// The icon font's code point for a Segoe icon: the same on Windows; Google's Material Icons (Apache 2.0,
/// assets/linux) elsewhere, where Segoe's icon fonts don't exist.
fn icon_cp(cp: u32) -> u32 {
    if cfg!(windows) {
        return cp;
    }
    match cp {
        0xE710 => 0xE145, // Add -> add
        0xE713 => 0xE8B8, // Setting -> settings
        0xE714 => 0xE04B, // Video -> videocam
        0xE715 => 0xE158, // Mail -> mail
        0xE71A => 0xE047, // Stop -> stop
        0xE721 => 0xE8B6, // Search -> search
        0xE72A => 0xE5C8, // Forward -> arrow_forward
        0xE72B => 0xE5C4, // Back -> arrow_back
        0xE72C => 0xE5D5, // Refresh -> refresh
        0xE739 => 0xE835, // Checkbox -> check_box_outline_blank
        0xE73A => 0xE834, // CheckboxComposite -> check_box
        0xE74D => 0xE872, // Delete -> delete
        0xE756 => 0xE5C3, // CommandPrompt -> apps
        0xE768 => 0xE037, // Play -> play_arrow
        0xE769 => 0xE034, // Pause -> pause
        0xE774 => 0xE80B, // Globe -> public
        0xE787 => 0xE935, // Calendar -> calendar_today
        0xE790 => 0xE40A, // Color -> palette
        0xE7B8 => 0xE149, // Package -> archive
        0xE7C4 => 0xE871, // TaskView -> dashboard
        0xE7EF => 0xE7FD, // Admin -> person
        0xE80F => 0xE88A, // Home -> home
        0xE81D => 0xEF71, // -> stop_circle
        0xE823 => 0xE8B5, // Recent -> schedule
        0xE838 => 0xE2C8, // FolderOpen -> folder_open
        0xE895 => 0xE627, // Sync -> sync
        0xE896 => 0xF090, // Download -> download
        0xE8A5 => 0xE873, // Document -> description
        0xE8AF => 0xEB2F, // -> lan
        0xE8B7 => 0xE2C7, // Folder -> folder
        0xE8D6 => 0xE405, // Audio -> music_note
        0xE8D7 => 0xE897, // Permissions -> lock
        0xE8F4 => 0xE2CC, // NewFolder -> create_new_folder
        0xE8FD => 0xE896, // BulletedList -> list
        0xE930 => 0xE86C, // Completed -> check_circle
        0xE945 => 0xEA0B, // LightningBolt -> bolt
        0xE946 => 0xE88E, // Info -> info
        0xE9D9 => 0xE429, // Diagnostic -> tune
        0xEA80 => 0xE0F0, // Lightbulb -> lightbulb
        0xEC4A => 0xE9E4, // Speed -> speed
        0xF5B0 => 0xE1C4, // PlaySolid -> play_circle
        _ => cp,
    }
}

/// A strip of `cells` (glyph code point, colour) side by side, each cell `cw` x `ch` px with the glyph `px` high.
pub(in crate::app) fn glyph_strip(cells: &[(u32, [u8; 3])], cw: usize, ch: usize, px: f32) -> Option<win::Rgba> {
    use ab_glyph::Font;
    let font = icon_font()?;
    let (w, mut out) = (cw * cells.len(), vec![0u8; cw * cells.len() * ch * 4]);
    for (k, (cp, rgb)) in cells.iter().enumerate() {
        let id = font.glyph_id(char::from_u32(icon_cp(*cp))?);
        let Some(g) = font.outline_glyph(id.with_scale(font.pt_to_px_scale(px * 0.75).unwrap_or(ab_glyph::PxScale::from(px)))) else { continue };
        let b = g.px_bounds();
        let (ox, oy) = ((k * cw) as f32 + (cw as f32 - b.width()) / 2.0, (ch as f32 - b.height()) / 2.0);
        g.draw(|x, y, c| {
            let (x, y) = ((ox + x as f32) as usize, (oy + y as f32) as usize);
            if x < w && y < ch {
                let i = (y * w + x) * 4;
                out[i..i + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], (c.min(1.0) * 255.0) as u8]);
            }
        });
    }
    Some((w, ch, out))
}

/// A lighter copy of a strip, for hovered buttons.
fn lighter((w, h, mut px): win::Rgba) -> win::Rgba {
    for p in px.chunks_mut(4) {
        for c in &mut p[..3] {
            *c = (*c as u16 + (255 - *c as u16) * 2 / 5) as u8;
        }
    }
    (w, h, px)
}

/// The toolbar's icons, in `TB_BUTTONS` order: (Segoe icon code point, colour).
const TB_GLYPHS: [(u32, [u8; 3]); TB_ICONS] = [
    (0xE710, [46, 158, 79]),   // Add URL
    (0xE768, [30, 136, 229]),  // Resume
    (0xE71A, [229, 57, 53]),   // Stop
    (0xE769, [251, 140, 0]),   // Stop All
    (0xE74D, [211, 47, 47]),   // Delete
    (0xE930, [0, 137, 123]),   // Delete Completed
    (0xE713, [126, 87, 194]),  // Options
    (0xE823, [245, 124, 0]),   // Scheduler
    (0xF5B0, [67, 160, 71]),   // Start Queue
    (0xE81D, [198, 40, 40]),   // Stop Queue
    (0xE774, [3, 155, 229]),   // Grabber
    (0xE715, [142, 36, 170]),  // Tell a Friend
];

/// A custom toolbar's file name for each button (`<name>.png`, `<name>-hot.png`, `<name>-disabled.png`).
const TB_FILES: [&str; TB_ICONS] = ["add-url", "resume", "stop", "stop-all", "delete", "delete-completed", "options", "scheduler", "start-queue", "stop-queue", "grabber", "tell-a-friend"];

/// The built-in toolbar's name (`Settings::tb_skin`).
pub(in crate::app) const TB_BUILTIN: &str = "Neon";

/// Where custom toolbars live: one folder per toolbar, next to NDM's settings.
pub(in crate::app) fn toolbars_dir() -> PathBuf {
    core::store_path().with_file_name("toolbars")
}

/// The custom toolbars installed: folder names with at least one button picture, sorted.
pub(in crate::app) fn toolbar_sets() -> Vec<String> {
    let mut sets: Vec<String> = std::fs::read_dir(toolbars_dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| TB_FILES.iter().any(|f| e.path().join(format!("{f}.png")).is_file()))
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    sets.sort_by_key(|s| s.to_lowercase());
    sets
}

/// Where custom category icons live: one folder per set, next to NDM's settings.
pub(super) fn cat_icons_dir() -> PathBuf {
    core::store_path().with_file_name("category-icons")
}

/// The category icon sets installed: folders with at least one PNG, sorted.
pub(in crate::app) fn cat_icon_sets() -> Vec<String> {
    let mut sets: Vec<String> = std::fs::read_dir(cat_icons_dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| std::fs::read_dir(e.path()).into_iter().flatten().flatten().any(|f| f.path().extension().is_some_and(|x| x.eq_ignore_ascii_case("png"))))
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    sets.sort_by_key(|s| s.to_lowercase());
    sets
}

/// A category icon set's file names for a tree item, most specific first (README, Custom category icons).
pub(in crate::app) fn cat_icon_files(v: &Cat, running: bool) -> Vec<String> {
    let slug = |c: &str| c.trim().to_lowercase().split_whitespace().collect::<Vec<_>>().join("-");
    match v {
        Cat::Files(Which::All, None) => vec!["all-downloads".into()],
        Cat::Files(Which::Unfinished, None) => vec!["unfinished".into()],
        Cat::Files(Which::Finished, None) => vec!["finished".into()],
        Cat::Files(_, Some(c)) => vec![slug(c), "category".into()],
        Cat::Grabber(None) => vec!["grabber-projects".into()],
        Cat::Grabber(Some(_)) => vec!["grabber-project".into(), "grabber-projects".into()],
        Cat::Queue(None) => vec!["queues".into()],
        Cat::Queue(Some(_)) if running => vec!["queue-running".into(), "queue".into()],
        Cat::Queue(Some(_)) => vec!["queue".into()],
    }
}

/// A picture file fitted into a `cw` x `ch` cell, centred, its proportions kept.
pub(in crate::app) fn tb_picture(file: &Path, cw: usize, ch: usize) -> Option<win::Rgba> {
    let img = image::open(file).ok()?.into_rgba8();
    let (w, h) = (img.width().max(1) as f32, img.height().max(1) as f32);
    let k = (cw as f32 / w).min(ch as f32 / h);
    let (nw, nh) = (((w * k).round() as u32).clamp(1, cw as u32), ((h * k).round() as u32).clamp(1, ch as u32));
    let img = image::imageops::resize(&img, nw, nh, image::imageops::FilterType::Lanczos3);
    let mut out = vec![0u8; cw * ch * 4];
    let (ox, oy) = ((cw - nw as usize) / 2, (ch - nh as usize) / 2);
    for (x, y, p) in img.enumerate_pixels() {
        let i = ((oy + y as usize) * cw + ox + x as usize) * 4;
        out[i..i + 4].copy_from_slice(&p.0);
    }
    Some((cw, ch, out))
}

/// A greyed, half transparent copy, for disabled buttons.
fn grayed((w, h, mut px): win::Rgba) -> win::Rgba {
    for p in px.chunks_mut(4) {
        let l = ((p[0] as u32 * 3 + p[1] as u32 * 6 + p[2] as u32) / 10) as u8;
        p.copy_from_slice(&[l, l, l, p[3] / 2]);
    }
    (w, h, px)
}

/// NDM Style's toolbar colours, one neon per button in `TB_BUTTONS` order: (on dark, on light). The light
/// ones are deeper, so they stay readable on white.
const TB_NEON: [([u8; 3], [u8; 3]); TB_ICONS] = [
    ([0, 229, 255], [0, 166, 204]),   // Add URL: cyan
    ([57, 255, 20], [26, 168, 30]),   // Resume: green
    ([255, 49, 70], [218, 28, 52]),   // Stop: red
    ([255, 145, 0], [226, 106, 0]),   // Stop All: orange
    ([255, 32, 160], [212, 0, 122]),  // Delete: pink
    ([0, 255, 178], [0, 160, 116]),   // Delete Completed: aqua
    ([190, 80, 255], [136, 48, 230]), // Options: violet
    ([255, 234, 0], [190, 146, 0]),   // Scheduler: yellow
    ([44, 130, 255], [28, 92, 232]),  // Start Queue: blue
    ([255, 0, 230], [184, 0, 190]),   // Stop Queue: magenta
    ([190, 255, 0], [108, 166, 0]),   // Grabber: lime
    ([255, 110, 90], [222, 72, 52]),  // Tell a Friend: coral
];

impl Toolbar {
    /// `skin`: the built-in set or a folder in `toolbars_dir()`; `ndm`: NDM Style's one-colour glyphs;
    /// `scale`: View -> Custom scale's toolbar size.
    pub(super) fn load(ctx: &egui::Context, skin: &str, ndm: bool, scale: f32) -> Toolbar {
        let dir = Some(toolbars_dir().join(skin)).filter(|d| skin != TB_BUILTIN && d.is_dir());
        let px = |v: f32| ((v * scale).round() as usize).max(8);
        let mut sizes = [vec![], vec![]];
        // cells as the toolbar draws them: 56 x 42 px large, 32 x 32 px small (at 100%)
        for (si, (cw, ch, glyph)) in [(px(56.0), px(42.0), 34.0 * scale), (px(32.0), px(32.0), 24.0 * scale)].into_iter().enumerate() {
            for k in 0..TB_ICONS {
                let file = |suffix: &str| {
                    let d = dir.as_ref()?;
                    let name = format!("{}{suffix}.png", TB_FILES[k]);
                    // a small/ folder holds pictures drawn for the small buttons
                    let f = Some(d.join("small").join(&name)).filter(|f| si == 1 && f.is_file()).unwrap_or_else(|| d.join(&name));
                    tb_picture(&f, cw, ch)
                };
                let pics = match file("") {
                    Some(n) => {
                        let hot = file("-hot").unwrap_or_else(|| lighter(n.clone()));
                        let off = file("-disabled").unwrap_or_else(|| grayed(n.clone()));
                        Some(([n, hot, off], false))
                    }
                    // NDM Style: lighter, smaller line icons
                    None if ndm => glyph_strip(&[(TB_GLYPHS[k].0, [255; 3])], cw, ch, glyph * 0.78).map(|g| ([g.clone(), g.clone(), g], true)),
                    None => glyph_strip(&[TB_GLYPHS[k]], cw, ch, glyph).map(|g| ([g.clone(), lighter(g.clone()), grayed(g)], false)),
                };
                sizes[si].push(pics.map(|(p, tint)| {
                    let [a, b, c] = p;
                    let t = |n: usize, b: &win::Rgba| texture(ctx, &format!("tb{si}_{k}_{n}"), b);
                    TbPics { pics: [t(0, &a), t(1, &b), t(2, &c)], tint }
                }));
            }
        }
        let [large, small] = sizes;
        Toolbar { large, small }
    }

    /// Button `k`'s picture for a state, and the colour to draw it in.
    pub(super) fn pic(&self, small: bool, k: usize, enabled: bool, hot: bool, dark: bool) -> Option<(egui::TextureId, Color32)> {
        let p = if small { &self.small } else { &self.large }.get(k)?.as_ref()?;
        let state = if !enabled { 2 } else if hot { 1 } else { 0 };
        let tint = if !p.tint {
            Color32::WHITE
        } else {
            let (on_dark, on_light) = TB_NEON[k];
            let [r, g, b] = if dark { on_dark } else { on_light };
            let neon = Color32::from_rgb(r, g, b);
            match state {
                2 => dlu::theme(dark).muted.gamma_multiply(0.45),
                // hovered: brighter, towards white on dark, deeper on light
                1 if dark => neon.lerp_to_gamma(Color32::WHITE, 0.3),
                1 => neon.lerp_to_gamma(Color32::BLACK, 0.12),
                _ => neon,
            }
        };
        Some((p.pics[state].id(), tint))
    }
}

/// Classic Windows look: Tahoma, square corners. Light = NDM's default system colours.
/// Dark = NDM's "Dark Mode support" (Windows dark app mode, DarkMode_Explorer / ItemsView themes).
/// Colours from dark palette object (ctor 0x437aa0): dialogs and lists 0x393939,
/// text white, disabled 0x808080; tree 0x202020 (0x437df0); menu 0x202020, hot item 0x505050 (0x4ff150).
/// Edit fields are drawn by Windows' DarkMode_CFD theme, not 25,25,25 is an estimate.
/// Set when the Language menu was first opened: `style` then loads the CJK fonts the language names need.
pub(in crate::app) static ALL_CJK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The list font's face (View -> Font), read by `style`.
pub(in crate::app) static LIST_FONT: Mutex<String> = Mutex::new(String::new());

fn load_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    for (name, file) in [("tahoma", r"C:\Windows\Fonts\tahoma.ttf"), ("segoe", r"C:\Windows\Fonts\segoeui.ttf")] {
        if let Ok(b) = std::fs::read(file) {
            fonts.font_data.insert(name.into(), Arc::new(egui::FontData::from_owned(b)));
            fonts.families.get_mut(&egui::FontFamily::Proportional).unwrap().insert(0, name.into());
            break;
        }
    }
    // Linux and macOS: egui's built-in font stays first (the dialogs are laid out for its widths); the desktop's
    // sans-serif, a font with Arabic letters and DejaVu Sans fill in the symbols it lacks (arrows, ticks) and
    // other scripts
    #[cfg(not(windows))]
    {
        let mut seen = vec![];
        for (name, face) in [("sys", "sans-serif"), ("sys_ar", "sans-serif:lang=ar"), ("sys_fb", "DejaVu Sans")] {
            let Some(f) = win::font_file(face).filter(|f| !seen.contains(f)) else { continue };
            if let Ok(b) = std::fs::read(&f) {
                fonts.font_data.insert(name.into(), Arc::new(egui::FontData::from_owned(b)));
                fonts.families.get_mut(&egui::FontFamily::Proportional).unwrap().push(name.into());
                seen.push(f);
            }
        }
    }
    // Fallbacks for scripts Tahoma lacks: Segoe UI (Georgian...), Nirmala (Hindi), and the CJK font of the
    // chosen language (loaded only then; the files are large).
    let cjk = match i18n::current().as_str() {
        "ndm_zh" => r"C:\Windows\Fonts\msyh.ttc",
        "ndm_zh_tw" => r"C:\Windows\Fonts\msjh.ttc",
        "ndm_ja" => r"C:\Windows\Fonts\YuGothM.ttc",
        "ndm_ko" => r"C:\Windows\Fonts\malgun.ttf",
        _ => "",
    };
    // once the Language menu was opened: Chinese (both scripts) and Korean for the language names
    let (zh, ko) = if ALL_CJK.load(Relaxed) { (r"C:\Windows\Fonts\msyh.ttc", r"C:\Windows\Fonts\malgun.ttf") } else { ("", "") };
    for (name, file) in [("segoe_fb", r"C:\Windows\Fonts\segoeui.ttf"), ("nirmala", r"C:\Windows\Fonts\Nirmala.ttf"), ("cjk", cjk), ("zh", zh), ("ko", ko)] {
        if let Ok(b) = std::fs::read(file) {
            fonts.font_data.insert(name.into(), Arc::new(egui::FontData::from_owned(b)));
            fonts.families.get_mut(&egui::FontFamily::Proportional).unwrap().push(name.into());
        }
    }
    // NDM's main window (menu bar, toolbar labels, tree, list) uses the system font, Segoe UI 9pt
    let mut ui_fam: Vec<String> = fonts.font_data.contains_key("segoe_fb").then(|| "segoe_fb".to_string()).into_iter().collect();
    ui_fam.extend(fonts.families[&egui::FontFamily::Proportional].iter().filter(|f| *f != "segoe_fb").cloned());
    fonts.families.insert(egui::FontFamily::Name("segoe".into()), ui_fam);
    // the download list: the chosen face first, the usual fallbacks after it
    let mut list_fam = fonts.families[&egui::FontFamily::Proportional].clone();
    let face = LIST_FONT.lock().unwrap().clone();
    if let Some(b) = win::font_file(&face).filter(|_| !face.is_empty()).and_then(|f| std::fs::read(f).ok()) {
        fonts.font_data.insert("list".into(), Arc::new(egui::FontData::from_owned(b)));
        list_fam.insert(0, "list".into());
    }
    fonts.families.insert(egui::FontFamily::Name("list".into()), list_fam);
    ctx.set_fonts(fonts);
}

pub(in crate::app) fn style(ctx: &egui::Context, dark: bool) {
    load_fonts(ctx);
    apply_style(ctx, dark);
}

pub(in crate::app) fn apply_style(ctx: &egui::Context, dark: bool) {
    ctx.set_theme(if dark { egui::ThemePreference::Dark } else { egui::ThemePreference::Light });
    // both themes are set up: NDM keeps most dialogs light while the main window is dark (`dialog`)
    for dark in [false, true] {
        let (text, chrome, field) = if dark {
            (Color32::WHITE, DARK_DLG, Color32::from_rgb(25, 25, 25))
        } else {
            (Color32::BLACK, Color32::from_rgb(240, 240, 240), Color32::WHITE)
        };
        let mut v = if dark { egui::Visuals::dark() } else { egui::Visuals::light() };
        (v.panel_fill, v.window_fill, v.extreme_bg_color) = (chrome, chrome, field);
        v.faint_bg_color = if dark { Color32::from_gray(30) } else { Color32::from_gray(248) };
        v.override_text_color = Some(text);
        v.window_corner_radius = egui::CornerRadius::ZERO;
        v.menu_corner_radius = egui::CornerRadius::ZERO;
        v.window_shadow = egui::Shadow::NONE;
        v.popup_shadow = egui::Shadow { offset: [2, 2], blur: 4, spread: 0, color: Color32::from_black_alpha(60) };
        v.selection.bg_fill = if dark { Color32::from_gray(77) } else { Color32::from_rgb(204, 232, 255) };
        v.selection.stroke = egui::Stroke::new(1.0_f32, text);
        v.hyperlink_color = if dark { Color32::from_rgb(96, 170, 255) } else { BAR };
        let w = &mut v.widgets;
        let states: [(&mut egui::style::WidgetVisuals, Color32, Color32); 5] = if dark {
            [
                (&mut w.noninteractive, chrome, Color32::from_gray(70)),
                (&mut w.inactive, Color32::from_gray(51), Color32::from_gray(155)),
                (&mut w.hovered, Color32::from_gray(69), Color32::from_gray(155)),
                (&mut w.active, Color32::from_gray(85), Color32::from_gray(180)),
                (&mut w.open, Color32::from_gray(69), Color32::from_gray(155)),
            ]
        } else {
            [
                (&mut w.noninteractive, chrome, Color32::from_gray(200)),
                (&mut w.inactive, Color32::from_rgb(225, 225, 225), Color32::from_rgb(173, 173, 173)),
                (&mut w.hovered, Color32::from_rgb(229, 241, 251), Color32::from_rgb(0, 120, 215)),
                (&mut w.active, Color32::from_rgb(204, 228, 247), Color32::from_rgb(0, 84, 153)),
                (&mut w.open, Color32::from_rgb(204, 228, 247), Color32::from_rgb(0, 84, 153)),
            ]
        };
        for (wv, fill, stroke) in states {
            wv.corner_radius = egui::CornerRadius::ZERO;
            (wv.bg_fill, wv.weak_bg_fill) = (fill, fill);
            wv.bg_stroke = egui::Stroke::new(1.0_f32, stroke);
            wv.fg_stroke.color = text;
            wv.expansion = 0.0;
        }
        if dlu::ndm() {
            ndm_visuals(&mut v, dark);
        }
        ctx.set_visuals_of(if dark { egui::Theme::Dark } else { egui::Theme::Light }, v);
    }
    ctx.all_styles_mut(|s| {
        s.spacing.button_padding = Vec2::new(10.0, 3.0);
        s.spacing.item_spacing = Vec2::new(6.0, 4.0);
        s.spacing.interact_size.y = 18.0;
        // Tahoma 8 as NDM's dialogs, tree and list draw it (fitted to text widths measured on NDM; the
        // menu bar is Segoe UI 12.2, the toolbar labels Segoe UI 11.6)
        for (t, size) in [(egui::TextStyle::Body, 10.0), (egui::TextStyle::Button, 10.0), (egui::TextStyle::Small, 10.0), (egui::TextStyle::Heading, 14.0)] {
            s.text_styles.insert(t, FontId::proportional(size));
        }
    });
}

/// NDM Style: soft rounded controls on a cool grey ground, the accent colour for selection, focus and
/// the active control, light shadows under menus and windows.
fn ndm_visuals(v: &mut egui::Visuals, dark: bool) {
    let t = dlu::theme(dark);
    let r = |x: f32| egui::CornerRadius::same(x as u8);
    (v.panel_fill, v.window_fill, v.extreme_bg_color, v.faint_bg_color) = (t.bg, t.surface, t.surface, t.raised);
    v.override_text_color = Some(t.text);
    v.window_corner_radius = r(dlu::R_CARD + 3.0);
    v.menu_corner_radius = r(dlu::R_CARD);
    v.window_stroke = egui::Stroke::new(1.0_f32, t.border);
    v.window_shadow = egui::Shadow { offset: [0, 6], blur: 18, spread: 0, color: Color32::from_black_alpha(if dark { 110 } else { 40 }) };
    v.popup_shadow = egui::Shadow { offset: [0, 4], blur: 12, spread: 0, color: Color32::from_black_alpha(if dark { 100 } else { 34 }) };
    v.selection.bg_fill = t.accent_soft;
    v.selection.stroke = egui::Stroke::new(1.0_f32, t.accent);
    v.hyperlink_color = t.accent;
    v.text_cursor.stroke = egui::Stroke::new(1.5_f32, t.accent);
    let w = &mut v.widgets;
    let states: [(&mut egui::style::WidgetVisuals, Color32, Color32); 5] = [
        (&mut w.noninteractive, t.bg, t.border),
        (&mut w.inactive, t.surface, t.border),
        (&mut w.hovered, t.raised, t.accent),
        (&mut w.active, t.accent_soft, t.accent),
        (&mut w.open, t.accent_soft, t.accent),
    ];
    for (wv, fill, stroke) in states {
        wv.corner_radius = r(dlu::R_CTRL);
        (wv.bg_fill, wv.weak_bg_fill) = (fill, fill);
        wv.bg_stroke = egui::Stroke::new(1.0_f32, stroke);
        wv.fg_stroke.color = t.text;
        wv.expansion = 0.0;
    }
    // ticks, radio dots and slider handles in the accent colour
    w.active.fg_stroke.color = t.accent;
    w.noninteractive.fg_stroke.color = t.text;
    w.noninteractive.bg_stroke.color = t.border;
}

/// NDM's icons (see `icon_font`), keyed by the slot numbers the UI asks for.
#[derive(Default)]
pub(in crate::app) struct Res {
    pub(in crate::app) tree: HashMap<u16, egui::TextureHandle>,
    pub(in crate::app) icons: HashMap<u16, egui::TextureHandle>,
    /// Download progress connection bar: ends, background, downloaded, start marker (358, 359, 361, 362).
    pub(in crate::app) bars: Option<[egui::TextureHandle; 4]>,
    /// Grabber: the wizard's picture and toolbar
    /// 260's 24 px strips (bitmaps 265 normal, 266 hot, and a greyed copy).
    pub(in crate::app) grab_pic: Option<egui::TextureHandle>,
    pub(in crate::app) grab_tb: Option<[egui::TextureHandle; 3]>,
}

impl Res {
    pub(super) fn load(ctx: &egui::Context) -> Res {
        let mut r = Res::default();
        if let Ok(img) = image::load_from_memory_with_format(include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/grabber.jpg")), image::ImageFormat::Jpeg) {
            let px = img.to_rgba8();
            r.grab_pic = Some(ctx.load_texture("grabpic", egui::ColorImage::from_rgba_unmultiplied([px.width() as usize, px.height() as usize], px.as_raw()), egui::TextureOptions::LINEAR));
        }
        let tex = |name: &str, i: Option<win::Rgba>| i.map(|i| texture(ctx, name, &i));
        // tree images (drawn 16 x 13 pt): folders, statuses, categories, grabber projects, queues, limits
        const FOLDER: [u8; 3] = [240, 170, 30];
        let tree: [(u16, u32, [u8; 3]); 20] = [
            (214, 0xE838, FOLDER), (215, 0xE8B7, FOLDER), (216, 0xE930, [46, 158, 79]), (217, 0xE930, [46, 158, 79]),
            (218, 0xE896, [30, 136, 229]), (219, 0xE896, [30, 136, 229]), (220, 0xE8D6, [216, 27, 96]), (221, 0xE714, [229, 57, 53]),
            (222, 0xE756, [84, 110, 122]), (223, 0xE8A5, [30, 136, 229]), (224, 0xE8B7, FOLDER), (225, 0xE7B8, [141, 110, 99]),
            (282, 0xE774, [3, 155, 229]), (311, 0xE8FD, [94, 53, 177]), (312, 0xE8FD, [67, 160, 71]), (313, 0xE8FD, [94, 53, 177]),
            (315, 0xE895, [0, 137, 123]), (316, 0xE895, [67, 160, 71]), (329, 0xEC4A, [251, 140, 0]), (104, 0xEA80, [251, 192, 45]),
        ];
        for (id, cp, rgb) in tree {
            let (w, h, px) = if id == 104 { (48, 48, 40.0) } else { (32, 26, 24.0) };
            if let Some(t) = tex(&format!("ndmt{id}"), glyph_strip(&[(cp, rgb)], w, h, px)) {
                r.tree.insert(id, t);
            }
        }
        // grabber toolbar (10 icons, drawn 24 pt): explore, stop, check all, uncheck, download, stop download,
        // add to the main list, refresh, schedule, statistics
        let gtb = [(0xE721, [30, 136, 229]), (0xE71A, [229, 57, 53]), (0xE73A, [46, 158, 79]), (0xE739, [117, 117, 117]), (0xE896, [30, 136, 229]),
            (0xE81D, [198, 40, 40]), (0xE8F4, [245, 124, 0]), (0xE72C, [0, 137, 123]), (0xE823, [245, 124, 0]), (0xE946, [94, 53, 177])];
        if let Some(n) = glyph_strip(&gtb, 48, 48, 34.0) {
            let mut g = n.clone();
            for p in g.2.chunks_mut(4) {
                let l = ((p[0] as u32 * 3 + p[1] as u32 * 6 + p[2] as u32) / 10) as u8;
                p.copy_from_slice(&[l, l, l, p[3] / 2]);
            }
            r.grab_tb = Some([texture(ctx, "gtbn", &n), texture(ctx, "gtbh", &lighter(n.clone())), texture(ctx, "gtbg", &g)]);
        }
        // 32 pt page and dialog icons: Options pages, site logins, dial up, sounds, queues, grabber schedule
        let pages: [(u16, u32, [u8; 3]); 11] = [
            (277, 0xE774, [3, 155, 229]), (295, 0xE8A5, [30, 136, 229]), (276, 0xE8B7, FOLDER), (293, 0xE896, [46, 158, 79]),
            (273, 0xE9D9, [94, 53, 177]), (278, 0xE8AF, [0, 137, 123]), (272, 0xE8D7, [251, 140, 0]), (275, 0xE7EF, [84, 110, 122]),
            (274, 0xE8D6, [216, 27, 96]), (331, 0xE8FD, [94, 53, 177]), (285, 0xE823, [245, 124, 0]),
        ];
        for (id, cp, rgb) in pages {
            if let Some(t) = tex(&format!("ndmi{id}"), glyph_strip(&[(cp, rgb)], 64, 64, 52.0)) {
                r.icons.insert(id, t);
            }
        }
        r
    }
    pub(super) fn tree(&self, id: u16) -> Option<egui::TextureId> {
        self.tree.get(&id).map(|t| t.id())
    }
    pub(super) fn icon(&self, id: u16) -> Option<egui::TextureId> {
        self.icons.get(&id).map(|t| t.id())
    }
}

impl App {
    /// Cached Explorer icon for a file extension ("/" = folder).
    pub(in crate::app) fn icon(&mut self, ctx: &egui::Context, ext: &str) -> Option<egui::TextureId> {
        self.icons
            .entry(ext.to_string())
            .or_insert_with(|| win::shell_icon(ext, ext == "/").map(|i| texture(ctx, &format!("icon{ext}"), &i)))
            .as_ref()
            .map(|t| t.id())
    }

    pub(in crate::app) fn exe_icon(&mut self, ctx: &egui::Context, exe: &str) -> Option<egui::TextureId> {
        let key = format!("exe:{exe}");
        self.icons
            .entry(key.clone())
            .or_insert_with(|| win::exe_icon(Path::new(exe)).map(|i| texture(ctx, &key, &i)))
            .as_ref()
            .map(|t| t.id())
    }
}
