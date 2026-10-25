//! Windows bits: shell file-type icons, an exe's icon, showing/hiding the main window, BMP decoding.
use std::mem::{size_of, zeroed};
use std::path::Path;
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL};
use windows_sys::Win32::UI::Shell::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

pub type Rgba = (usize, usize, Vec<u8>);

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

/// Read a bitmap's pixels as top-down BGRA.
unsafe fn dib(dc: HDC, bmp: HBITMAP, w: i32, h: i32) -> Option<Vec<u8>> {
    let mut bi: BITMAPINFO = zeroed();
    bi.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
    (bi.bmiHeader.biWidth, bi.bmiHeader.biHeight) = (w, -h);
    (bi.bmiHeader.biPlanes, bi.bmiHeader.biBitCount) = (1, 32);
    bi.bmiHeader.biCompression = BI_RGB as _;
    let mut buf = vec![0u8; (w * h * 4) as usize];
    (GetDIBits(dc, bmp, 0, h as u32, buf.as_mut_ptr().cast(), &mut bi, DIB_RGB_COLORS) != 0).then_some(buf)
}

/// HICON -> RGBA. Destroys the icon.
unsafe fn icon_rgba(icon: HICON) -> Option<Rgba> {
    let mut ii: ICONINFO = zeroed();
    if GetIconInfo(icon, &mut ii) == 0 {
        DestroyIcon(icon);
        return None;
    }
    let mut bm: BITMAP = zeroed();
    GetObjectW(ii.hbmColor, size_of::<BITMAP>() as i32, (&mut bm as *mut BITMAP).cast());
    let (w, h) = (bm.bmWidth, bm.bmHeight);
    let dc = GetDC(std::ptr::null_mut());
    let mut px = if ii.hbmColor.is_null() || w == 0 { None } else { dib(dc, ii.hbmColor, w, h) };
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
    let name = wide(&if folder { "folder".into() } else { format!("file.{ext}") });
    let attr = if folder { FILE_ATTRIBUTE_DIRECTORY } else { FILE_ATTRIBUTE_NORMAL };
    unsafe {
        let mut sfi: SHFILEINFOW = zeroed();
        let ok = SHGetFileInfoW(name.as_ptr(), attr, &mut sfi, size_of::<SHFILEINFOW>() as u32, SHGFI_ICON | SHGFI_LARGEICON | SHGFI_USEFILEATTRIBUTES);
        if ok == 0 || sfi.hIcon.is_null() { None } else { icon_rgba(sfi.hIcon) }
    }
}

/// Explorer's type name for a file extension, e.g. "ZIP archive".
pub fn type_name(ext: &str) -> String {
    let name = wide(&format!("file.{ext}"));
    unsafe {
        let mut sfi: SHFILEINFOW = zeroed();
        if SHGetFileInfoW(name.as_ptr(), FILE_ATTRIBUTE_NORMAL, &mut sfi, size_of::<SHFILEINFOW>() as u32, SHGFI_TYPENAME | SHGFI_USEFILEATTRIBUTES) == 0 {
            return String::new();
        }
        let n = sfi.szTypeName.iter().position(|c| *c == 0).unwrap_or(sfi.szTypeName.len());
        String::from_utf16_lossy(&sfi.szTypeName[..n])
    }
}

pub fn exe_icon(path: &Path) -> Option<Rgba> {
    let p = wide(&path.display().to_string());
    unsafe {
        let mut large: HICON = std::ptr::null_mut();
        if ExtractIconExW(p.as_ptr(), 0, &mut large, std::ptr::null_mut(), 1) == 0 || large.is_null() {
            return None;
        }
        icon_rgba(large)
    }
}

pub fn show(hwnd: isize) {
    let h = hwnd as HWND;
    if h.is_null() {
        return;
    }
    unsafe {
        ShowWindow(h, if IsIconic(h) != 0 { SW_RESTORE } else { SW_SHOW });
        SetForegroundWindow(h);
    }
}

pub fn hide(hwnd: isize) {
    unsafe { ShowWindow(hwnd as HWND, SW_HIDE) };
}

pub fn visible(hwnd: isize) -> bool {
    unsafe { IsWindowVisible(hwnd as HWND) != 0 }
}

/// Uncompressed 24/32-bit .bmp -> RGBA (32-bit files keep their alpha).
pub fn load_bmp(path: &Path) -> Option<Rgba> {
    let d = std::fs::read(path).ok()?;
    let u32_at = |o: usize| Some(u32::from_le_bytes(d.get(o..o + 4)?.try_into().ok()?));
    let (off, w, h) = (u32_at(10)? as usize, u32_at(18)? as i32, u32_at(22)? as i32);
    let bpp = u16::from_le_bytes(d.get(28..30)?.try_into().ok()?) as usize / 8;
    if !(bpp == 3 || bpp == 4) || u32_at(30)? != 0 {
        return None;
    }
    let (w, hh) = (w as usize, h.unsigned_abs() as usize);
    let stride = (w * bpp).div_ceil(4) * 4;
    let mut out = Vec::with_capacity(w * hh * 4);
    for y in 0..hh {
        let row = if h > 0 { hh - 1 - y } else { y };
        let line = d.get(off + row * stride..off + row * stride + w * bpp)?;
        for p in line.chunks(bpp) {
            out.extend([p[2], p[1], p[0], if bpp == 4 { p[3] } else { 255 }]);
        }
    }
    Some((w, hh, out))
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
        if h != main as HWND && GetWindow(h, GW_OWNER).is_null() && IsWindowVisible(h) != 0 && GetWindowLongPtrW(h, GWL_STYLE) as u32 & WS_CAPTION == WS_CAPTION {
            SetWindowLongPtrW(h, GWLP_HWNDPARENT, main);
            dark_title(h as isize, dark);
        } else if h != main as HWND && retitle {
            dark_title(h as isize, dark);
        }
        1
    }
    if main != 0 {
        unsafe { EnumThreadWindows(windows_sys::Win32::System::Threading::GetCurrentThreadId(), Some(each), main) };
    }
}

/// Dark or light title bar (NDM sets DWMWA_USE_IMMERSIVE_DARK_MODE the same way).
pub fn dark_title(hwnd: isize, dark: bool) {
    use windows_sys::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE};
    let v: i32 = dark as i32;
    unsafe { DwmSetWindowAttribute(hwnd as HWND, DWMWA_USE_IMMERSIVE_DARK_MODE as u32, (&v as *const i32).cast(), 4) };
}

/// Bitmap resource `id` of a PE file (loaded as data, never executed), with `key` made transparent.
/// The bytes of a resource of a named type in a PE file (NDM's "IMAGE" JPEGs), loaded as data.
pub fn pe_raw(path: &Path, ty: &str, id: u16) -> Option<Vec<u8>> {
    use windows_sys::Win32::Foundation::FreeLibrary;
    use windows_sys::Win32::System::LibraryLoader::{FindResourceW, LoadLibraryExW, LoadResource, LockResource, SizeofResource, LOAD_LIBRARY_AS_DATAFILE, LOAD_LIBRARY_AS_IMAGE_RESOURCE};
    let (p, t) = (wide(&path.display().to_string()), wide(ty));
    unsafe {
        let m = LoadLibraryExW(p.as_ptr(), std::ptr::null_mut(), LOAD_LIBRARY_AS_DATAFILE | LOAD_LIBRARY_AS_IMAGE_RESOURCE);
        if m.is_null() {
            return None;
        }
        let r = FindResourceW(m, id as usize as *const u16, t.as_ptr());
        let mut out = None;
        if !r.is_null() {
            let data = LockResource(LoadResource(m, r)) as *const u8;
            if !data.is_null() {
                out = Some(std::slice::from_raw_parts(data, SizeofResource(m, r) as usize).to_vec());
            }
        }
        FreeLibrary(m);
        out
    }
}

pub fn pe_bitmap(path: &Path, id: u16, key: Option<[u8; 3]>) -> Option<Rgba> {
    use windows_sys::Win32::Foundation::FreeLibrary;
    use windows_sys::Win32::System::LibraryLoader::{LoadLibraryExW, LOAD_LIBRARY_AS_DATAFILE, LOAD_LIBRARY_AS_IMAGE_RESOURCE};
    let p = wide(&path.display().to_string());
    unsafe {
        let m = LoadLibraryExW(p.as_ptr(), std::ptr::null_mut(), LOAD_LIBRARY_AS_DATAFILE | LOAD_LIBRARY_AS_IMAGE_RESOURCE);
        if m.is_null() {
            return None;
        }
        let h = LoadImageW(m, id as usize as *const u16, IMAGE_BITMAP, 0, 0, LR_CREATEDIBSECTION);
        let out = if h.is_null() {
            None
        } else {
            let mut bm: BITMAP = zeroed();
            GetObjectW(h, size_of::<BITMAP>() as i32, (&mut bm as *mut BITMAP).cast());
            let dc = GetDC(std::ptr::null_mut());
            let px = dib(dc, h as HBITMAP, bm.bmWidth, bm.bmHeight);
            ReleaseDC(std::ptr::null_mut(), dc);
            DeleteObject(h);
            px.map(|mut px| {
                // 32-bit bitmaps with an alpha channel (NDM's Neon toolbar) keep it
                let alpha = bm.bmBitsPixel == 32 && px.chunks(4).any(|p| p[3] != 0);
                for p in px.chunks_mut(4) {
                    p.swap(0, 2);
                    if !alpha {
                        p[3] = if key == Some([p[0], p[1], p[2]]) { 0 } else { 255 };
                    }
                }
                (bm.bmWidth as usize, bm.bmHeight as usize, px)
            })
        };
        FreeLibrary(m);
        out
    }
}

/// Icon group resource `id` of a PE file at `size` px (loaded as data, never executed).
pub fn pe_icon(path: &Path, id: u16, size: i32) -> Option<Rgba> {
    use windows_sys::Win32::Foundation::FreeLibrary;
    use windows_sys::Win32::System::LibraryLoader::{LoadLibraryExW, LOAD_LIBRARY_AS_DATAFILE, LOAD_LIBRARY_AS_IMAGE_RESOURCE};
    let p = wide(&path.display().to_string());
    unsafe {
        let m = LoadLibraryExW(p.as_ptr(), std::ptr::null_mut(), LOAD_LIBRARY_AS_DATAFILE | LOAD_LIBRARY_AS_IMAGE_RESOURCE);
        if m.is_null() {
            return None;
        }
        let h = LoadImageW(m, id as usize as *const u16, IMAGE_ICON, size, size, 0);
        let out = if h.is_null() { None } else { icon_rgba(h as HICON) };
        FreeLibrary(m);
        out
    }
}

/// Text in the ANSI code page of language `lcid` (how NDM reads non-UTF-8 language files).
pub fn decode_ansi(b: &[u8], lcid: u32) -> String {
    use windows_sys::Win32::Globalization::{GetLocaleInfoW, MultiByteToWideChar, LOCALE_IDEFAULTANSICODEPAGE, LOCALE_RETURN_NUMBER};
    let mut cp = 0u32;
    let got = unsafe { GetLocaleInfoW(lcid, LOCALE_IDEFAULTANSICODEPAGE | LOCALE_RETURN_NUMBER, (&mut cp as *mut u32).cast(), 2) };
    let cp = if got == 0 { 1252 } else { cp };
    let n = unsafe { MultiByteToWideChar(cp, 0, b.as_ptr(), b.len() as i32, std::ptr::null_mut(), 0) };
    let mut w = vec![0u16; n.max(0) as usize];
    unsafe { MultiByteToWideChar(cp, 0, b.as_ptr(), b.len() as i32, w.as_mut_ptr(), n) };
    String::from_utf16_lossy(&w)
}

/// Changes whenever anything is copied (NDM watches with SetClipboardViewer, 0x45e1b0).
pub fn clip_seq() -> u32 {
    unsafe { windows_sys::Win32::System::DataExchange::GetClipboardSequenceNumber() }
}

/// Play a .wav without waiting, if the file exists NDM: PlaySoundW(path, 0, SND_ASYNC | SND_FILENAME)).
pub fn play_sound(path: &str) {
    use windows_sys::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_FILENAME};
    if Path::new(path).is_file() {
        unsafe { PlaySoundW(wide(path).as_ptr(), std::ptr::null_mut(), SND_ASYNC | SND_FILENAME) };
    }
}

/// The user's decimal separator (LOCALE_SDECIMAL), as NDM's size formatters read it; "," if unavailable.
pub fn decimal_sep() -> &'static str {
    static SEP: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    SEP.get_or_init(|| {
        use windows_sys::Win32::Globalization::{GetLocaleInfoEx, LOCALE_SDECIMAL};
        let mut b = [0u16; 8];
        let n = unsafe { GetLocaleInfoEx(std::ptr::null(), LOCALE_SDECIMAL, b.as_mut_ptr(), b.len() as i32) };
        if n > 1 { String::from_utf16_lossy(&b[..n as usize - 1]) } else { ",".into() }
    })
}

/// Windows "Choose your default app mode" is Dark.
pub fn system_dark() -> bool {
    use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
    let key = wide(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
    let val = wide("AppsUseLightTheme");
    let (mut data, mut len) = (1u32, 4u32);
    let r = unsafe { RegGetValueW(HKEY_CURRENT_USER, key.as_ptr(), val.as_ptr(), RRF_RT_REG_DWORD, std::ptr::null_mut(), (&mut data as *mut u32).cast(), &mut len) };
    r == 0 && data == 0
}

/// Start with Windows (HKCU Run key), like NDM's "Launch ... on startup".
/// Windows' (Internet Options) proxy: (ProxyEnable, ProxyServer, ProxyOverride).
pub fn system_proxy() -> (bool, String, String) {
    use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RRF_RT_REG_SZ};
    let key = wide(r"Software\Microsoft\Windows\CurrentVersion\Internet Settings");
    let text = |name: &str| {
        let (val, mut buf, mut len) = (wide(name), vec![0u16; 2048], 4096u32);
        let r = unsafe { RegGetValueW(HKEY_CURRENT_USER, key.as_ptr(), val.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), buf.as_mut_ptr().cast(), &mut len) };
        if r != 0 { String::new() } else { String::from_utf16_lossy(&buf[..buf.iter().position(|c| *c == 0).unwrap_or(0)]) }
    };
    let (val, mut on, mut len) = (wide("ProxyEnable"), 0u32, 4u32);
    let r = unsafe { RegGetValueW(HKEY_CURRENT_USER, key.as_ptr(), val.as_ptr(), RRF_RT_REG_DWORD, std::ptr::null_mut(), (&mut on as *mut u32).cast(), &mut len) };
    (r == 0 && on != 0, text("ProxyServer"), text("ProxyOverride"))
}

/// The proxy ("host:port") a PAC script picks for `url`, evaluated by WinHTTP; None = DIRECT or failure.
pub fn pac_proxy(pac: &str, url: &str) -> Option<String> {
    use windows_sys::Win32::Foundation::GlobalFree;
    use windows_sys::Win32::Networking::WinHttp::*;
    if pac.trim().is_empty() {
        return None;
    }
    let (pac, url) = (wide(pac.trim()), wide(url));
    unsafe {
        let h = WinHttpOpen(std::ptr::null(), WINHTTP_ACCESS_TYPE_NO_PROXY, std::ptr::null(), std::ptr::null(), 0);
        if h.is_null() {
            return None;
        }
        let mut opts = WINHTTP_AUTOPROXY_OPTIONS { dwFlags: WINHTTP_AUTOPROXY_CONFIG_URL, dwAutoDetectFlags: 0, lpszAutoConfigUrl: pac.as_ptr(), lpvReserved: std::ptr::null_mut(), dwReserved: 0, fAutoLogonIfChallenged: 1 };
        let mut info: WINHTTP_PROXY_INFO = std::mem::zeroed();
        let ok = WinHttpGetProxyForUrl(h, url.as_ptr(), &mut opts, &mut info) != 0;
        WinHttpCloseHandle(h);
        let take = |p: *mut u16| {
            if p.is_null() {
                return String::new();
            }
            let n = (0..).take_while(|&i| *p.add(i) != 0).count();
            let t = String::from_utf16_lossy(std::slice::from_raw_parts(p, n));
            GlobalFree(p.cast());
            t
        };
        let (list, _) = (take(info.lpszProxy), take(info.lpszProxyBypass));
        // "[scheme=]host:port; ..." -> the first host:port
        let first = list.split([';', ' ']).find(|x| !x.is_empty())?;
        Some(first.rsplit('=').next().unwrap_or(first).to_string()).filter(|_| ok)
    }
}

/// A REG_SZ value ("" = the key's default value); empty if missing.
fn reg_text(root: windows_sys::Win32::System::Registry::HKEY, key: &str, name: &str) -> String {
    use windows_sys::Win32::System::Registry::{RegGetValueW, RRF_RT_REG_SZ};
    let (key, val, mut buf, mut len) = (wide(key), wide(name), vec![0u16; 2048], 4096u32);
    let r = unsafe { RegGetValueW(root, key.as_ptr(), val.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), buf.as_mut_ptr().cast(), &mut len) };
    if r != 0 { String::new() } else { String::from_utf16_lossy(&buf[..buf.iter().position(|c| *c == 0).unwrap_or(0)]) }
}

/// Browsers registered with Windows (Clients\StartMenuInternet, per user and per machine): (name, exe).
/// NDM fills its General page list the same way and adds the ones it can integrate with.
pub fn installed_browsers() -> Vec<(String, String)> {
    use windows_sys::Win32::System::Registry::*;
    let mut out: Vec<(String, String)> = Vec::new();
    let (native, wow) = (r"SOFTWARE\Clients\StartMenuInternet", r"SOFTWARE\WOW6432Node\Clients\StartMenuInternet");
    for (root, base) in [(HKEY_CURRENT_USER, native), (HKEY_LOCAL_MACHINE, native), (HKEY_LOCAL_MACHINE, wow)] {
        let mut hk: HKEY = std::ptr::null_mut();
        if unsafe { RegOpenKeyExW(root, wide(base).as_ptr(), 0, KEY_READ, &mut hk) } != 0 {
            continue;
        }
        for i in 0.. {
            let (mut name, mut len) = (vec![0u16; 256], 256u32);
            let r = unsafe { RegEnumKeyExW(hk, i, name.as_mut_ptr(), &mut len, std::ptr::null(), std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut()) };
            if r != 0 {
                break;
            }
            let sub = format!(r"{base}\{}", String::from_utf16_lossy(&name[..len as usize]));
            let title = reg_text(root, &sub, "");
            let cmd = reg_text(root, &format!(r"{sub}\shell\open\command"), "");
            let exe = cmd.trim().trim_start_matches('"').split('"').next().unwrap_or("").trim().to_string();
            if !title.is_empty() && Path::new(&exe).is_file() && !out.iter().any(|b| b.1.eq_ignore_ascii_case(&exe)) {
                out.push((title, exe));
            }
        }
        unsafe { RegCloseKey(hk) };
    }
    out
}

pub fn set_autostart(on: bool) {
    let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default();
    let key = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
    let args: Vec<&str> = if on { vec!["add", key, "/v", "NewDownloadManager", "/t", "REG_SZ", "/d", &exe, "/f"] } else { vec!["delete", key, "/v", "NewDownloadManager", "/f"] };
    let _ = std::process::Command::new("reg").args(args).output();
}

/// NDM runs once: starting it again shows the running window (the only way back with "Don't show"
/// tray icon). Only a copy of this same exe counts, so a build elsewhere still runs side by side.
pub fn show_running(title: &str) -> bool {
    running(title, true)
}

/// Is a copy of this exe running (with its window `title`)? `activate` brings that window up.
pub fn running(title: &str, activate: bool) -> bool {
    use windows_sys::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION};
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
        let dlg = CreateWindowExW(WS_EX_TOPMOST | WS_EX_DLGMODALFRAME, wide("#32770").as_ptr(), wide(title).as_ptr(), WS_POPUP | WS_CAPTION | WS_VISIBLE, (sw - w) / 2, (sh - h) / 2, w, h, null, null, null, std::ptr::null());
        if dlg.is_null() {
            return;
        }
        let mut rc = zeroed();
        GetClientRect(dlg, &mut rc);
        let st = CreateWindowExW(0, wide("STATIC").as_ptr(), wide(text).as_ptr(), WS_CHILD | WS_VISIBLE | 0x201 /* SS_CENTER | SS_CENTERIMAGE */, 0, 0, rc.right, rc.bottom, dlg, null, null, std::ptr::null());
        SendMessageW(st, WM_SETFONT, GetStockObject(DEFAULT_GUI_FONT) as usize, 1);
        UpdateWindow(dlg);
        UpdateWindow(st);
    }
}

/// Windows' "Font" dialog (View -> Font -> Choose Font..., NDM uses ChooseFont for its list): TrueType
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
        let name = String::from_utf16_lossy(&lf.lfFaceName[..lf.lfFaceName.iter().position(|c| *c == 0).unwrap_or(32)]);
        Some((name, cf.iPointSize as f32 / 10.0 * 96.0 / 72.0)) // 1/10 pt -> egui points (1/96 inch)
    }
}

/// The file of an installed font face ("Arial" -> C:\Windows\Fonts\arial.ttf), per-user fonts included.
pub fn font_file(face: &str) -> Option<std::path::PathBuf> {
    use windows_sys::Win32::System::Registry::*;
    let key = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Fonts";
    let fonts = std::path::PathBuf::from(std::env::var("WINDIR").unwrap_or(r"C:\Windows".into())).join("Fonts");
    for root in [HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER] {
        let mut hk: HKEY = std::ptr::null_mut();
        if unsafe { RegOpenKeyExW(root, wide(key).as_ptr(), 0, KEY_READ, &mut hk) } != 0 {
            continue;
        }
        let mut found = None;
        for i in 0.. {
            let (mut name, mut nlen, mut data, mut dlen) = (vec![0u16; 512], 512u32, vec![0u16; 1024], 2048u32);
            let r = unsafe { RegEnumValueW(hk, i, name.as_mut_ptr(), &mut nlen, std::ptr::null(), std::ptr::null_mut(), data.as_mut_ptr().cast(), &mut dlen) };
            if r != 0 {
                break;
            }
            // "Cambria & Cambria Math (TrueType)" = "cambria.ttc"
            let n = String::from_utf16_lossy(&name[..nlen as usize]);
            let faces = n.split(" (").next().unwrap_or("");
            if faces.split(" & ").any(|f| f.eq_ignore_ascii_case(face)) {
                let file = String::from_utf16_lossy(&data[..(dlen as usize / 2)]).trim_end_matches('\0').to_string();
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

// ---------- drop target (File -> Show drop target, NDM's CDropTargetWnd) ----------

/// Links dropped on the drop target or the main window, for the UI to take (`take_drops`).
static DROPS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
/// Called after a drop and on a double click of the drop target (shows the main window, repaints).
static WAKE: std::sync::OnceLock<Box<dyn Fn() + Send + Sync>> = std::sync::OnceLock::new();
static DROP_HWND: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);
static MAIN_HWND: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

pub fn take_drops() -> Vec<String> {
    std::mem::take(&mut DROPS.lock().unwrap())
}

type Pt = windows_sys::Win32::Foundation::POINTL;
type Ptr = *mut core::ffi::c_void;

/// A minimal static IDropTarget (COM): reads "UniformResourceLocatorW" or plain text from what is dropped.
#[repr(C)]
struct DropVtbl {
    query: unsafe extern "system" fn(*mut DropObj, *const windows_sys::core::GUID, *mut Ptr) -> i32,
    add_ref: unsafe extern "system" fn(*mut DropObj) -> u32,
    release: unsafe extern "system" fn(*mut DropObj) -> u32,
    enter: unsafe extern "system" fn(*mut DropObj, Ptr, u32, Pt, *mut u32) -> i32,
    over: unsafe extern "system" fn(*mut DropObj, u32, Pt, *mut u32) -> i32,
    leave: unsafe extern "system" fn(*mut DropObj) -> i32,
    drop: unsafe extern "system" fn(*mut DropObj, Ptr, u32, Pt, *mut u32) -> i32,
}
#[repr(C)]
struct DropObj {
    vtbl: *const DropVtbl,
}
unsafe impl Sync for DropObj {}

unsafe extern "system" fn d_query(this: *mut DropObj, _: *const windows_sys::core::GUID, out: *mut Ptr) -> i32 {
    *out = this.cast();
    0
}
unsafe extern "system" fn d_ref(_: *mut DropObj) -> u32 {
    1 // a static object: never freed
}
unsafe extern "system" fn d_enter(_: *mut DropObj, data: Ptr, _: u32, _: Pt, effect: *mut u32) -> i32 {
    *effect = if dropped_links(data).is_empty() { 0 } else { 1 }; // DROPEFFECT_COPY
    0
}
unsafe extern "system" fn d_over(_: *mut DropObj, _: u32, _: Pt, effect: *mut u32) -> i32 {
    *effect &= 1;
    0
}
unsafe extern "system" fn d_leave(_: *mut DropObj) -> i32 {
    0
}
unsafe extern "system" fn d_drop(_: *mut DropObj, data: Ptr, _: u32, _: Pt, effect: *mut u32) -> i32 {
    let links = dropped_links(data);
    *effect = if links.is_empty() { 0 } else { 1 };
    if !links.is_empty() {
        DROPS.lock().unwrap().extend(links);
        // a hidden window doesn't run the UI: bring it back to ask about the download
        let main = MAIN_HWND.load(std::sync::atomic::Ordering::Relaxed);
        if !visible(main) {
            show(main);
        }
        if let Some(w) = WAKE.get() {
            w();
        }
    }
    0
}
static DROP_VTBL: DropVtbl = DropVtbl { query: d_query, add_ref: d_ref, release: d_ref, enter: d_enter, over: d_over, leave: d_leave, drop: d_drop };
static DROP_OBJ: DropObj = DropObj { vtbl: &DROP_VTBL };

/// http/https/ftp addresses in a dropped IDataObject (a link: its URL; text: every address in it).
unsafe fn dropped_links(data: Ptr) -> Vec<String> {
    use windows_sys::Win32::System::Com::{FORMATETC, STGMEDIUM};
    use windows_sys::Win32::System::Memory::{GlobalLock, GlobalUnlock};
    type GetData = unsafe extern "system" fn(Ptr, *const FORMATETC, *mut STGMEDIUM) -> i32;
    if data.is_null() {
        return vec![];
    }
    let get: GetData = *(*(data as *const *const GetData)).add(3); // IDataObject::GetData
    let url = windows_sys::Win32::System::DataExchange::RegisterClipboardFormatW(wide("UniformResourceLocatorW").as_ptr()) as u16;
    for cf in [url, 13 /* CF_UNICODETEXT */] {
        let fmt = FORMATETC { cfFormat: cf, ptd: std::ptr::null_mut(), dwAspect: 1, lindex: -1, tymed: 1 /* TYMED_HGLOBAL */ };
        let mut m: STGMEDIUM = zeroed();
        if get(data, &fmt, &mut m) != 0 {
            continue;
        }
        let p = GlobalLock(m.u.hGlobal) as *const u16;
        let mut text = String::new();
        if !p.is_null() {
            let n = (0..1 << 20).find(|k| *p.add(*k) == 0).unwrap_or(0);
            text = String::from_utf16_lossy(std::slice::from_raw_parts(p, n));
            GlobalUnlock(m.u.hGlobal);
        }
        windows_sys::Win32::System::Ole::ReleaseStgMedium(&mut m);
        let links: Vec<String> = text.split_whitespace().filter(|w| ["http://", "https://", "ftp://"].iter().any(|s| w.to_ascii_lowercase().starts_with(s))).map(str::to_string).collect();
        if !links.is_empty() {
            return links;
        }
    }
    vec![]
}

/// Accept dropped links on `hwnd` (the main window too: "You can also drop links to NDM main window").
/// winit's own target (files only, unused here) is replaced.
pub fn accept_drops(hwnd: isize, wake: impl Fn() + Send + Sync + 'static) {
    let _ = WAKE.set(Box::new(wake));
    MAIN_HWND.store(hwnd, std::sync::atomic::Ordering::Relaxed);
    unsafe {
        windows_sys::Win32::System::Ole::OleInitialize(std::ptr::null());
        windows_sys::Win32::System::Ole::RevokeDragDrop(hwnd as HWND);
        windows_sys::Win32::System::Ole::RegisterDragDrop(hwnd as HWND, &DROP_OBJ as *const DropObj as Ptr);
    }
}

unsafe extern "system" fn drop_wnd_proc(h: HWND, msg: u32, w: usize, l: isize) -> isize {
    match msg {
        WM_NCHITTEST => HTCAPTION as isize, // drag it anywhere
        WM_NCRBUTTONUP | WM_NCLBUTTONDBLCLK => {
            if msg == WM_NCRBUTTONUP {
                ShowWindow(h, SW_HIDE); // NDM: close it; "Show drop target" brings it back
            } else {
                show(MAIN_HWND.load(std::sync::atomic::Ordering::Relaxed));
            }
            if let Some(f) = WAKE.get() {
                f();
            }
            0
        }
        _ => DefWindowProcW(h, msg, w, l),
    }
}

/// Show or hide the drop target: a floating, always-on-top picture links can be dragged onto.
/// Created on first use, bottom right of the screen.
pub fn drop_target(show: bool, (iw, ih, px): &Rgba) {
    use std::sync::atomic::Ordering::Relaxed;
    unsafe {
        let mut h = DROP_HWND.load(Relaxed) as HWND;
        if h.is_null() {
            if !show {
                return;
            }
            let class = wide("NDMDropTarget");
            let inst = windows_sys::Win32::System::LibraryLoader::GetModuleHandleW(std::ptr::null());
            let wc = WNDCLASSW { lpfnWndProc: Some(drop_wnd_proc), hInstance: inst, lpszClassName: class.as_ptr(), hCursor: LoadCursorW(std::ptr::null_mut(), IDC_SIZEALL), ..zeroed() };
            RegisterClassW(&wc);
            let (sw, sh) = (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN));
            let (w, ht) = (*iw as i32, *ih as i32);
            h = CreateWindowExW(WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW, class.as_ptr(), wide("Drop target").as_ptr(), WS_POPUP, sw - w - 40, sh - ht - 120, w, ht, std::ptr::null_mut(), std::ptr::null_mut(), inst, std::ptr::null());
            if h.is_null() {
                return;
            }
            // the picture through a 32-bit DIB, alpha premultiplied as UpdateLayeredWindow wants it
            let bi = BITMAPINFO { bmiHeader: BITMAPINFOHEADER { biSize: size_of::<BITMAPINFOHEADER>() as u32, biWidth: w, biHeight: -ht, biPlanes: 1, biBitCount: 32, ..zeroed() }, ..zeroed() };
            let screen = GetDC(std::ptr::null_mut());
            let mem = CreateCompatibleDC(screen);
            let mut bits = std::ptr::null_mut();
            let bmp = CreateDIBSection(mem, &bi, DIB_RGB_COLORS, &mut bits, std::ptr::null_mut(), 0);
            if !bits.is_null() {
                let dst = std::slice::from_raw_parts_mut(bits as *mut u8, iw * ih * 4);
                for (d, s) in dst.chunks_mut(4).zip(px.chunks(4)) {
                    let a = s[3] as u32;
                    (d[0], d[1], d[2], d[3]) = ((s[2] as u32 * a / 255) as u8, (s[1] as u32 * a / 255) as u8, (s[0] as u32 * a / 255) as u8, s[3]);
                }
                let old = SelectObject(mem, bmp);
                let size = windows_sys::Win32::Foundation::SIZE { cx: w, cy: ht };
                let src = windows_sys::Win32::Foundation::POINT { x: 0, y: 0 };
                let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: AC_SRC_ALPHA as u8 };
                UpdateLayeredWindow(h, screen, std::ptr::null(), &size, mem, &src, 0, &blend, ULW_ALPHA);
                SelectObject(mem, old);
            }
            DeleteObject(bmp);
            DeleteDC(mem);
            ReleaseDC(std::ptr::null_mut(), screen);
            windows_sys::Win32::System::Ole::RegisterDragDrop(h, &DROP_OBJ as *const DropObj as Ptr);
            DROP_HWND.store(h as isize, Relaxed);
        }
        ShowWindow(h, if show { SW_SHOWNOACTIVATE } else { SW_HIDE });
    }
}

pub fn drop_target_visible() -> bool {
    visible(DROP_HWND.load(std::sync::atomic::Ordering::Relaxed))
}
