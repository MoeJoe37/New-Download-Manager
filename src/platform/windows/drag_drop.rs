use super::{HWND, Rgba, show, size_of, visible, wide, zeroed};
use windows_sys::Win32::Graphics::Gdi::{AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, ReleaseDC, SelectObject};
use windows_sys::Win32::UI::WindowsAndMessaging::{CreateWindowExW, DefWindowProcW, GetSystemMetrics, HTCAPTION, IDC_SIZEALL, LoadCursorW, RegisterClassW, SM_CXSCREEN, SM_CYSCREEN, SW_HIDE, SW_SHOWNOACTIVATE, ShowWindow, ULW_ALPHA, UpdateLayeredWindow, WM_NCHITTEST, WM_NCLBUTTONDBLCLK, WM_NCRBUTTONUP, WNDCLASSW, WS_EX_LAYERED, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP};

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

/// Accept dropped links on `hwnd`.
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
                ShowWindow(h, SW_HIDE); // close it; "Show drop target" brings it back
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
