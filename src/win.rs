//! Windows bits: shell file-type icons, an exe's icon, showing/hiding the main window, BMP decoding.
use std::mem::{size_of, zeroed};
use std::path::Path;
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL};
use windows_sys::Win32::UI::Shell::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

#[path = "platform/windows/desktop.rs"]
mod desktop;
#[path = "platform/windows/drag_drop.rs"]
mod drag_drop;
#[path = "platform/windows/network.rs"]
mod network;

pub use self::desktop::{
    installed_browsers, is_host_launch, native_host, register_host, set_autostart,
};
pub use self::drag_drop::{accept_drops, drop_target, drop_target_visible, take_drops};
pub use self::network::{
    nics, pac_proxy, ras_connected, ras_dial, ras_entries, ras_hangup_all, ras_save,
    set_system_proxy, system_proxy,
};

pub type Rgba = (usize, usize, Vec<u8>);

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

/// The WinINet ("IE") cache's copy of `url`: (local file, its Last-Modified as an HTTP date): a normal cache entry whose file still exists and has a modification time.
pub fn url_cache(url: &str) -> Option<(std::path::PathBuf, String)> {
    use windows_sys::Win32::Networking::WinInet::{
        GetUrlCacheEntryInfoW, INTERNET_CACHE_ENTRY_INFOW, NORMAL_CACHE_ENTRY,
    };
    let u = wide(url);
    let mut len = 0u32;
    unsafe {
        GetUrlCacheEntryInfoW(u.as_ptr(), std::ptr::null_mut(), &mut len);
        if len < size_of::<INTERNET_CACHE_ENTRY_INFOW>() as u32 {
            return None;
        }
        let mut buf = vec![0u64; (len as usize).div_ceil(8)]; // 8-aligned for the struct
        let e = buf.as_mut_ptr() as *mut INTERNET_CACHE_ENTRY_INFOW;
        if GetUrlCacheEntryInfoW(u.as_ptr(), e, &mut len) == 0
            || (*e).CacheEntryType & NORMAL_CACHE_ENTRY == 0
            || (*e).lpszLocalFileName.is_null()
        {
            return None;
        }
        let p = (*e).lpszLocalFileName;
        let n = (0..).take_while(|&k| *p.add(k) != 0).count();
        let file =
            std::path::PathBuf::from(String::from_utf16_lossy(std::slice::from_raw_parts(p, n)));
        let ft = (*e).LastModifiedTime;
        let ticks = ((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64;
        // FILETIME = 100 ns since 1601; NDM formats it "%a, %d %b %Y %H:%M:%S GMT"
        let secs = (ticks / 10_000_000).checked_sub(11_644_473_600)?;
        let when = chrono::DateTime::from_timestamp(secs as i64, 0)?;
        file.is_file()
            .then(|| (file, when.format("%a, %d %b %Y %H:%M:%S GMT").to_string()))
    }
}

/// Read a bitmap's pixels as top-down BGRA.
unsafe fn dib(dc: HDC, bmp: HBITMAP, w: i32, h: i32) -> Option<Vec<u8>> {
    let mut bi: BITMAPINFO = zeroed();
    bi.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
    (bi.bmiHeader.biWidth, bi.bmiHeader.biHeight) = (w, -h);
    (bi.bmiHeader.biPlanes, bi.bmiHeader.biBitCount) = (1, 32);
    bi.bmiHeader.biCompression = BI_RGB as _;
    let mut buf = vec![0u8; (w * h * 4) as usize];
    (GetDIBits(
        dc,
        bmp,
        0,
        h as u32,
        buf.as_mut_ptr().cast(),
        &mut bi,
        DIB_RGB_COLORS,
    ) != 0)
        .then_some(buf)
}

/// HICON -> RGBA. Destroys the icon.
unsafe fn icon_rgba(icon: HICON) -> Option<Rgba> {
    let mut ii: ICONINFO = zeroed();
    if GetIconInfo(icon, &mut ii) == 0 {
        DestroyIcon(icon);
        return None;
    }
    let mut bm: BITMAP = zeroed();
    GetObjectW(
        ii.hbmColor,
        size_of::<BITMAP>() as i32,
        (&mut bm as *mut BITMAP).cast(),
    );
    let (w, h) = (bm.bmWidth, bm.bmHeight);
    let dc = GetDC(std::ptr::null_mut());
    let mut px = if ii.hbmColor.is_null() || w == 0 {
        None
    } else {
        dib(dc, ii.hbmColor, w, h)
    };
    if let Some(px) = px.as_mut() {
        // Old icons have no alpha channel; the AND mask says what's transparent.
        if px.chunks(4).all(|p| p[3] == 0) {
            if let Some(mask) = dib(dc, ii.hbmMask, w, h) {
                for (p, m) in px.chunks_mut(4).zip(mask.chunks(4)) {
                    p[3] = if m[0] == 0 { 255 } else { 0 };
                }
            }
        }
        px.chunks_mut(4).for_each(|p| p.swap(0, 2));
    }
    ReleaseDC(std::ptr::null_mut(), dc);
    DeleteObject(ii.hbmColor);
    DeleteObject(ii.hbmMask);
    DestroyIcon(icon);
    px.map(|p| (w as usize, h as usize, p))
}

/// The icon Explorer shows for files with this extension (or for a folder).
pub fn shell_icon(ext: &str, folder: bool) -> Option<Rgba> {
    let name = wide(&if folder {
        "folder".into()
    } else {
        format!("file.{ext}")
    });
    let attr = if folder {
        FILE_ATTRIBUTE_DIRECTORY
    } else {
        FILE_ATTRIBUTE_NORMAL
    };
    unsafe {
        let mut sfi: SHFILEINFOW = zeroed();
        let ok = SHGetFileInfoW(
            name.as_ptr(),
            attr,
            &mut sfi,
            size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON | SHGFI_LARGEICON | SHGFI_USEFILEATTRIBUTES,
        );
        if ok == 0 || sfi.hIcon.is_null() {
            None
        } else {
            icon_rgba(sfi.hIcon)
        }
    }
}

/// Explorer's type name for a file extension, e.g. "ZIP archive".
pub fn type_name(ext: &str) -> String {
    let name = wide(&format!("file.{ext}"));
    unsafe {
        let mut sfi: SHFILEINFOW = zeroed();
        if SHGetFileInfoW(
            name.as_ptr(),
            FILE_ATTRIBUTE_NORMAL,
            &mut sfi,
            size_of::<SHFILEINFOW>() as u32,
            SHGFI_TYPENAME | SHGFI_USEFILEATTRIBUTES,
        ) == 0
        {
            return String::new();
        }
        let n = sfi
            .szTypeName
            .iter()
            .position(|c| *c == 0)
            .unwrap_or(sfi.szTypeName.len());
        String::from_utf16_lossy(&sfi.szTypeName[..n])
    }
}

pub fn exe_icon(path: &Path) -> Option<Rgba> {
    let p = wide(&path.display().to_string());
    unsafe {
        let mut large: HICON = std::ptr::null_mut();
        if ExtractIconExW(p.as_ptr(), 0, &mut large, std::ptr::null_mut(), 1) == 0
            || large.is_null()
        {
            return None;
        }
        icon_rgba(large)
    }
}

/// Started for the browser (`/tray`): eframe shows the main window after its first frame whatever NDM
/// asks, so while this is on the window's subclass drops that show (and the WS_VISIBLE winit sets
/// with it). Keep the guard for the entire time in the tray: creating or closing a download viewport
/// can show its owner again. Only an explicit `show` removes it.
static KEEP_HIDDEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn keep_hidden(hwnd: isize, on: bool) {
    use std::sync::atomic::Ordering::Relaxed;
    use windows_sys::Win32::UI::Shell::{RemoveWindowSubclass, SetWindowSubclass};
    if hwnd == 0 {
        return;
    }
    if on {
        KEEP_HIDDEN.store(true, Relaxed);
        unsafe { SetWindowSubclass(hwnd as HWND, Some(hidden_proc), 1, 0) };
    } else {
        KEEP_HIDDEN.store(false, Relaxed);
        unsafe { RemoveWindowSubclass(hwnd as HWND, Some(hidden_proc), 1) };
    }
}

unsafe extern "system" fn hidden_proc(
    h: HWND,
    msg: u32,
    w: usize,
    l: isize,
    _: usize,
    _: usize,
) -> isize {
    use windows_sys::Win32::UI::Shell::DefSubclassProc;
    if KEEP_HIDDEN.load(std::sync::atomic::Ordering::Relaxed) {
        match msg {
            WM_WINDOWPOSCHANGING => (*(l as *mut WINDOWPOS)).flags &= !SWP_SHOWWINDOW,
            WM_STYLECHANGING if w as i32 == GWL_STYLE => {
                (*(l as *mut STYLESTRUCT)).styleNew &= !WS_VISIBLE
            }
            _ => {}
        }
    }
    DefSubclassProc(h, msg, w, l)
}

/// Marks a new file sparse (FSCTL_SET_SPARSE), so setting its length doesn't make NTFS write zeros later.
pub fn sparse(f: &std::fs::File) {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::IO::DeviceIoControl;
    const FSCTL_SET_SPARSE: u32 = 0x0009_00C4;
    let mut n = 0u32;
    unsafe {
        DeviceIoControl(
            f.as_raw_handle() as _,
            FSCTL_SET_SPARSE,
            std::ptr::null(),
            0,
            std::ptr::null_mut(),
            0,
            &mut n,
            std::ptr::null_mut(),
        )
    };
}

/// A message box when NDM can't start at all (no window to show it in).
pub fn fatal(title: &str, text: &str) {
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            wide(text).as_ptr(),
            wide(title).as_ptr(),
            MB_OK | MB_ICONERROR,
        )
    };
}

pub fn show(hwnd: isize) {
    let h = hwnd as HWND;
    if h.is_null() {
        return;
    }
    keep_hidden(hwnd, false);
    unsafe {
        ShowWindow(
            h,
            if IsIconic(h) != 0 {
                SW_RESTORE
            } else {
                SW_SHOW
            },
        );
        SetForegroundWindow(h);
    }
}

/// Set by the UI while it has dialogs open: they are viewports drawn in the main window's frame, and a
/// hidden window gets no WM_PAINT, so egui would stop. Like NDM, a download from the browser shows only
/// its dialog while the main window stays in the tray.
pub static PUMP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Keeps the UI running while the main window is hidden and `PUMP` is set (posts WM_PAINT ~30 times a second).
pub fn pump(hwnd: isize) {
    use std::sync::atomic::Ordering::Relaxed;
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_millis(33));
        if PUMP.load(Relaxed) && !visible(hwnd) {
            wake(hwnd);
        }
    });
}

/// One frame of the UI even while its window is hidden.
pub fn wake(hwnd: isize) {
    unsafe { PostMessageW(hwnd as HWND, WM_PAINT, 0, 0) };
}

pub fn hide(hwnd: isize) {
    keep_hidden(hwnd, true);
    unsafe { ShowWindow(hwnd as HWND, SW_HIDE) };
}

pub fn visible(hwnd: isize) -> bool {
    unsafe { IsWindowVisible(hwnd as HWND) != 0 }
}

/// Make every other top-level window of this thread (the dialogs) owned by `main`,
/// so they stay above it like NDM's dialogs do.
/// `retitle` re-applies the title bar theme to every dialog (after a theme switch);
/// otherwise only newly seen dialogs get it.
pub fn own_dialogs(main: isize, dark: bool, retitle: bool, light: &[String]) {
    // Thread-local: EnumThreadWindows only passes one isize.
    thread_local!(static MODE: std::cell::RefCell<(bool, bool, Vec<String>)> = const { std::cell::RefCell::new((false, false, Vec::new())) });
    MODE.with(|d| *d.borrow_mut() = (dark, retitle, light.to_vec()));
    unsafe extern "system" fn each(h: HWND, main: isize) -> i32 {
        // the dialogs NDM keeps light in dark mode get a light title bar too (matched by title)
        let mut buf = [0u16; 256];
        let n = GetWindowTextW(h, buf.as_mut_ptr(), 256).max(0) as usize;
        let title = String::from_utf16_lossy(&buf[..n]);
        let (dark, retitle) = MODE.with(|d| {
            let d = d.borrow();
            (d.0 && !d.2.contains(&title), d.1)
        });
        if h != main as HWND
            && GetWindow(h, GW_OWNER).is_null()
            && IsWindowVisible(h) != 0
            && GetWindowLongPtrW(h, GWL_STYLE) as u32 & WS_CAPTION == WS_CAPTION
        {
            SetWindowLongPtrW(h, GWLP_HWNDPARENT, main);
            dark_title(h as isize, dark);
        } else if h != main as HWND && retitle {
            dark_title(h as isize, dark);
        }
        1
    }
    if main != 0 {
        unsafe {
            EnumThreadWindows(
                windows_sys::Win32::System::Threading::GetCurrentThreadId(),
                Some(each),
                main,
            )
        };
    }
}

/// Dark or light title bar.
pub fn dark_title(hwnd: isize, dark: bool) {
    use windows_sys::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE};
    let v: i32 = dark as i32;
    unsafe {
        DwmSetWindowAttribute(
            hwnd as HWND,
            DWMWA_USE_IMMERSIVE_DARK_MODE as u32,
            (&v as *const i32).cast(),
            4,
        )
    };
}

/// Changes whenever anything is copied.
pub fn clip_seq() -> u32 {
    unsafe { windows_sys::Win32::System::DataExchange::GetClipboardSequenceNumber() }
}

/// Play a.wav without waiting, if the file exists.
pub fn play_sound(path: &str) {
    use windows_sys::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_FILENAME};
    if Path::new(path).is_file() {
        unsafe {
            PlaySoundW(
                wide(path).as_ptr(),
                std::ptr::null_mut(),
                SND_ASYNC | SND_FILENAME,
            )
        };
    }
}

/// The user's decimal separator (LOCALE_SDECIMAL), as NDM's size formatters read it; "," if unavailable.
pub fn decimal_sep() -> &'static str {
    static SEP: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    SEP.get_or_init(|| {
        use windows_sys::Win32::Globalization::{GetLocaleInfoEx, LOCALE_SDECIMAL};
        let mut b = [0u16; 8];
        let n = unsafe {
            GetLocaleInfoEx(
                std::ptr::null(),
                LOCALE_SDECIMAL,
                b.as_mut_ptr(),
                b.len() as i32,
            )
        };
        if n > 1 {
            String::from_utf16_lossy(&b[..n as usize - 1])
        } else {
            ",".into()
        }
    })
}

/// Windows "Choose your default app mode" is Dark.
pub fn system_dark() -> bool {
    use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
    let key = wide(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
    let val = wide("AppsUseLightTheme");
    let (mut data, mut len) = (1u32, 4u32);
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            val.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            (&mut data as *mut u32).cast(),
            &mut len,
        )
    };
    r == 0 && data == 0
}

/// Explorer with the file selected in its folder. The shell call takes any path (spaces, commas,
/// non-Latin names); "explorer /select," is the fall-back, its path in quotes of its own (a quoted
/// "/select,..." argument makes Explorer open Documents instead).
pub fn show_in_folder(path: &std::path::Path) {
    use windows_sys::Win32::System::Com::{
        CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED,
    };
    use windows_sys::Win32::UI::Shell::{ILCreateFromPathW, ILFree, SHOpenFolderAndSelectItems};
    let full = std::path::absolute(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string()
        .replace('/', "\\");
    std::thread::spawn(move || {
        let selected = unsafe {
            let initialized =
                CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32) >= 0;
            if initialized {
                let pidl = ILCreateFromPathW(wide(&full).as_ptr());
                let selected = if pidl.is_null() {
                    false
                } else {
                    let hr = SHOpenFolderAndSelectItems(pidl, 0, std::ptr::null(), 0);
                    ILFree(pidl);
                    hr >= 0
                };
                CoUninitialize();
                selected
            } else {
                false
            }
        };
        if !selected {
            use std::os::windows::process::CommandExt;
            let _ = std::process::Command::new("explorer.exe")
                .raw_arg(format!("/select,\"{full}\""))
                .spawn();
        }
    });
}

/// Adds `dir` to the user's Path (HKCU\Environment) unless it is there, and tells running programs (new
/// terminals and Explorer pick it up).
pub fn add_user_path(dir: &std::path::Path) {
    use windows_sys::Win32::System::Registry::{
        RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_EXPAND_SZ, RRF_NOEXPAND,
        RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SendMessageTimeoutW, HWND_BROADCAST, SMTO_ABORTIFHUNG, WM_SETTINGCHANGE,
    };
    let (key, val) = (wide("Environment"), wide("Path"));
    let (mut buf, mut len) = (vec![0u16; 32768], 65536u32);
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            val.as_ptr(),
            RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | RRF_NOEXPAND,
            std::ptr::null_mut(),
            buf.as_mut_ptr().cast(),
            &mut len,
        )
    };
    let path = if r == 0 {
        String::from_utf16_lossy(&buf[..buf.iter().position(|c| *c == 0).unwrap_or(0)])
    } else {
        String::new()
    };
    let want = dir.display().to_string();
    if path.split(';').any(|p| {
        p.trim()
            .trim_end_matches('\\')
            .eq_ignore_ascii_case(want.trim_end_matches('\\'))
    }) {
        return;
    }
    let new = if path.trim().is_empty() {
        want
    } else {
        format!("{};{want}", path.trim_end_matches(';'))
    };
    let v = wide(&new);
    unsafe {
        if RegSetKeyValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            val.as_ptr(),
            REG_EXPAND_SZ,
            v.as_ptr().cast(),
            v.len() as u32 * 2,
        ) == 0
        {
            let env = wide("Environment");
            SendMessageTimeoutW(
                HWND_BROADCAST,
                WM_SETTINGCHANGE,
                0,
                env.as_ptr() as isize,
                SMTO_ABORTIFHUNG,
                3000,
                std::ptr::null_mut(),
            );
        }
    }
}

// ---------- Windows Dial Up / VPN (RAS, rasapi32) ----------

/// Is a copy of this exe running (with its window `title`)? `activate` brings that window up:
/// starting it again shows the running window (the only way back with "Don't show" tray icon). Only a
/// copy of this same exe counts, so a build elsewhere still runs side by side.
pub fn running(title: &str, activate: bool) -> bool {
    use windows_sys::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    let me = std::env::current_exe().ok();
    unsafe {
        let h = FindWindowW(std::ptr::null(), wide(title).as_ptr());
        if h.is_null() {
            return false;
        }
        let mut pid = 0;
        GetWindowThreadProcessId(h, &mut pid);
        let p = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if p.is_null() {
            return false;
        }
        let (mut buf, mut n) = ([0u16; 1024], 1024u32);
        let ok = QueryFullProcessImageNameW(p, 0, buf.as_mut_ptr(), &mut n) != 0;
        windows_sys::Win32::Foundation::CloseHandle(p);
        if !ok || me.as_deref() != Some(Path::new(&String::from_utf16_lossy(&buf[..n as usize]))) {
            return false;
        }
        if activate {
            show(h as isize);
        }
        true
    }
}

/// "Exit NDM" / "Saving settings..." (template 352) shown while the app stops its downloads and saves:
/// a dialog-class popup with one centred STATIC, painted once; it goes away with the process.
pub fn exit_splash(title: &str, text: &str) {
    unsafe {
        let (w, h) = (180, 80);
        let (sw, sh) = (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN));
        let null = std::ptr::null_mut();
        let dlg = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_DLGMODALFRAME,
            wide("#32770").as_ptr(),
            wide(title).as_ptr(),
            WS_POPUP | WS_CAPTION | WS_VISIBLE,
            (sw - w) / 2,
            (sh - h) / 2,
            w,
            h,
            null,
            null,
            null,
            std::ptr::null(),
        );
        if dlg.is_null() {
            return;
        }
        let mut rc = zeroed();
        GetClientRect(dlg, &mut rc);
        let st = CreateWindowExW(
            0,
            wide("STATIC").as_ptr(),
            wide(text).as_ptr(),
            WS_CHILD | WS_VISIBLE | 0x201, /* SS_CENTER | SS_CENTERIMAGE */
            0,
            0,
            rc.right,
            rc.bottom,
            dlg,
            null,
            null,
            std::ptr::null(),
        );
        SendMessageW(st, WM_SETFONT, GetStockObject(DEFAULT_GUI_FONT) as usize, 1);
        UpdateWindow(dlg);
        UpdateWindow(st);
    }
}

/// Windows' "Font" dialog: TrueType
/// faces only, so the face's file can be loaded. Returns (face, size in logical px).
pub fn choose_font(owner: isize, face: &str, px: f32) -> Option<(String, f32)> {
    use windows_sys::Win32::UI::Controls::Dialogs::*;
    unsafe {
        let mut lf: LOGFONTW = zeroed();
        lf.lfHeight = -(px.round() as i32);
        for (d, c) in lf.lfFaceName.iter_mut().zip(face.encode_utf16().take(31)) {
            *d = c;
        }
        let mut cf: CHOOSEFONTW = zeroed();
        cf.lStructSize = size_of::<CHOOSEFONTW>() as u32;
        cf.hwndOwner = owner as HWND;
        cf.lpLogFont = &mut lf;
        cf.Flags = CF_SCREENFONTS | CF_INITTOLOGFONTSTRUCT | CF_TTONLY | CF_NOVERTFONTS;
        if ChooseFontW(&mut cf) == 0 {
            return None;
        }
        let name = String::from_utf16_lossy(
            &lf.lfFaceName[..lf.lfFaceName.iter().position(|c| *c == 0).unwrap_or(32)],
        );
        Some((name, cf.iPointSize as f32 / 10.0 * 96.0 / 72.0)) // 1/10 pt -> egui points (1/96 inch)
    }
}

/// The file of an installed font face ("Arial" -> C:\Windows\Fonts\arial.ttf), per-user fonts included.
pub fn font_file(face: &str) -> Option<std::path::PathBuf> {
    use windows_sys::Win32::System::Registry::*;
    let key = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Fonts";
    let fonts = std::path::PathBuf::from(std::env::var("WINDIR").unwrap_or(r"C:\Windows".into()))
        .join("Fonts");
    for root in [HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER] {
        let mut hk: HKEY = std::ptr::null_mut();
        if unsafe { RegOpenKeyExW(root, wide(key).as_ptr(), 0, KEY_READ, &mut hk) } != 0 {
            continue;
        }
        let mut found = None;
        for i in 0.. {
            let (mut name, mut nlen, mut data, mut dlen) =
                (vec![0u16; 512], 512u32, vec![0u16; 1024], 2048u32);
            let r = unsafe {
                RegEnumValueW(
                    hk,
                    i,
                    name.as_mut_ptr(),
                    &mut nlen,
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    data.as_mut_ptr().cast(),
                    &mut dlen,
                )
            };
            if r != 0 {
                break;
            }
            // "Cambria & Cambria Math (TrueType)" = "cambria.ttc"
            let n = String::from_utf16_lossy(&name[..nlen as usize]);
            let faces = n.split(" (").next().unwrap_or("");
            if faces.split(" & ").any(|f| f.eq_ignore_ascii_case(face)) {
                let file = String::from_utf16_lossy(&data[..(dlen as usize / 2)])
                    .trim_end_matches('\0')
                    .to_string();
                found = Some(fonts.join(file)); // join keeps an absolute (per-user) path as is
                break;
            }
        }
        unsafe { RegCloseKey(hk) };
        if found.is_some() {
            return found;
        }
    }
    None
}

/// "Turn off computer when done": shut down with a 60 s grace period (`shutdown /a` cancels it), sleep or
/// hibernate; `force` closes programs that would block it.
pub fn power_off(kind: u8, force: bool) {
    let mut c = match kind {
        1 => {
            let mut c = std::process::Command::new("rundll32.exe");
            c.args(["powrprof.dll,SetSuspendState", "0,1,0"]);
            c
        }
        2 => {
            let mut c = std::process::Command::new("shutdown");
            c.arg("/h");
            c
        }
        _ => {
            let mut c = std::process::Command::new("shutdown");
            c.args([
                "/s",
                "/t",
                "60",
                "/c",
                "Download queue finished. The computer will shut down in 60 seconds.",
            ]);
            c
        }
    };
    if force && kind != 1 {
        c.arg("/f");
    }
    let _ = c.spawn();
}

/// A second start asked for the window: on Windows `running` brings it up itself.
pub fn show_main() {}

/// Linux takes the AppImage's settings out of a child's environment; nothing to do on Windows.
pub fn clean_env(_c: &mut std::process::Command) {}

/// Linux packages only (see linux.rs).
pub fn package_owner(_exe: &Path) -> Option<&'static str> {
    None
}

pub fn install_package(_file: &Path) -> Result<(), String> {
    Err("not on Windows".into())
}

#[cfg(test)]
#[test]
fn tray_window_stays_hidden_until_explicitly_opened() {
    unsafe {
        let title = wide("NDM tray visibility test");
        let class = wide("STATIC");
        let root = CreateWindowExW(
            0,
            class.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPEDWINDOW,
            0,
            0,
            320,
            240,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null(),
        );
        assert!(!root.is_null());
        hide(root as isize);
        // eframe (or Windows restoring an owner after a popup closes) asks to show it again.
        for _ in 0..3 {
            ShowWindow(root, SW_SHOW);
            assert!(!visible(root as isize));
            let popup = CreateWindowExW(
                0,
                class.as_ptr(),
                wide("Download complete").as_ptr(),
                WS_OVERLAPPEDWINDOW,
                0,
                0,
                200,
                120,
                root,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            );
            assert!(!popup.is_null());
            ShowWindow(popup, SW_SHOW);
            assert!(visible(popup as isize));
            DestroyWindow(popup);
            assert!(!visible(root as isize));
        }
        show(root as isize);
        assert!(visible(root as isize), "Open in the tray still works");
        hide(root as isize);
        ShowWindow(root, SW_SHOW);
        assert!(!visible(root as isize), "closing re-arms the guard");
        keep_hidden(root as isize, false);
        DestroyWindow(root);
    }
}

#[cfg(test)]
#[test]
fn explorer_takes_any_path() {
    use windows_sys::Win32::UI::Shell::{ILCreateFromPathW, ILFree};
    // what made "explorer /select," open Documents: spaces, a comma, non-Latin letters
    let dir = std::env::temp_dir()
        .join(["ndm test, folder ", "\u{2713} \u{645}\u{62c}\u{644}\u{62f}"].concat());
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("My video (1).mp4");
    std::fs::write(&file, b"x").unwrap();
    for p in [
        file.display().to_string(),
        file.display()
            .to_string()
            .replace(std::path::MAIN_SEPARATOR, "/"),
    ] {
        let full = std::path::absolute(&p)
            .unwrap()
            .display()
            .to_string()
            .replace('/', "\\");
        let w = wide(&full);
        let pidl = unsafe { ILCreateFromPathW(w.as_ptr()) };
        assert!(!pidl.is_null(), "{full}");
        unsafe { ILFree(pidl) };
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Update only this application's shortcuts in user-owned launcher folders. The signed executable
/// stays intact; winit updates the running window's taskbar/title icon separately.
pub fn set_application_icon(_pixels: &Rgba, icon: &std::path::Path, _custom: bool) {
    let icon = icon.to_path_buf();
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    std::thread::spawn(move || {
        use std::os::windows::process::CommandExt;
        let script = r#"
$ErrorActionPreference='Stop'
$ws=New-Object -ComObject WScript.Shell
$exe=[IO.Path]::GetFullPath($env:NDM_EXE_PATH)
$folders=@([Environment]::GetFolderPath('DesktopDirectory'),[Environment]::GetFolderPath('Programs'),(Join-Path $env:APPDATA 'Microsoft\Internet Explorer\Quick Launch\User Pinned\TaskBar'))
$found=$false
foreach($folder in $folders) {
 if(Test-Path -LiteralPath $folder) {
  Get-ChildItem -LiteralPath $folder -Filter '*.lnk' -File -Recurse -ErrorAction SilentlyContinue | ForEach-Object {
   $link=$ws.CreateShortcut($_.FullName)
   if($link.TargetPath -and [IO.Path]::GetFullPath($link.TargetPath) -ieq $exe) {
    $link.IconLocation=$env:NDM_ICON_PATH+',0';$link.Save();$found=$true
   }
  }
 }
}
if(-not $found) {
 $link=$ws.CreateShortcut((Join-Path ([Environment]::GetFolderPath('Programs')) 'New Download Manager.lnk'))
 $link.TargetPath=$exe;$link.WorkingDirectory=[IO.Path]::GetDirectoryName($exe)
 $link.IconLocation=$env:NDM_ICON_PATH+',0';$link.Save()
}
"#;
        let _ = std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .env("NDM_EXE_PATH", exe)
            .env("NDM_ICON_PATH", icon)
            .creation_flags(0x08000000)
            .status();
    });
}
