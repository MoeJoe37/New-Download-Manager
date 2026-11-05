#[allow(deprecated)]
use super::NSFilenamesPboardType;
use super::{
    define_class, image_of, msg_send, sel, AnyClass, AnyObject, AtomicBool, Bool, DefinedClass,
    Imp, MainThreadMarker, MainThreadOnly, Mutex, NSArray, NSBackingStoreType, NSColor,
    NSCompositingOperation, NSDragOperation, NSEvent, NSFloatingWindowLevel, NSImage, NSObject,
    NSPanel, NSPasteboard, NSPasteboardTypeString, NSPasteboardTypeURL, NSPoint, NSRect,
    NSResponder, NSScreen, NSSize, NSString, NSUInteger, NSView, NSWindowCollectionBehavior,
    NSWindowStyleMask, OnceLock, Relaxed, Retained, Rgba, Sel,
};

/// Links dropped on the main window or the drop target, for the UI to take (`take_drops`).
static DROPS: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// Repaints the UI (set by `accept_drops`).
static WAKE: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();
/// The UI should bring the main window back (a second start, a drop, a double click on the drop target).
static SHOW_MAIN: AtomicBool = AtomicBool::new(false);
static DROP_SHOWN: AtomicBool = AtomicBool::new(false);
thread_local!(static DROP_WIN: std::cell::RefCell<Option<Retained<NSPanel>>> = const { std::cell::RefCell::new(None) });

pub fn take_drops() -> Vec<String> {
    std::mem::take(&mut DROPS.lock().unwrap())
}

pub fn take_show() -> bool {
    SHOW_MAIN.swap(false, Relaxed)
}

fn wake_ui(show: bool) {
    if show {
        SHOW_MAIN.store(true, Relaxed);
    }
    if let Some(w) = WAKE.get() {
        w();
    }
}

/// A second start asked for the window: `take_show` hands it to the UI.
pub fn show_main() {
    wake_ui(true);
}

/// Every http, https and ftp address in dropped text, once each.
fn dropped_links(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for w in text
        .split_whitespace()
        .map(|w| w.trim_end_matches([',', '.', ';', ')', ']', '>', '"', '\'']))
    {
        if ["http://", "https://", "ftp://"]
            .iter()
            .any(|s| w.to_ascii_lowercase().starts_with(s))
            && !out.iter().any(|o| o == w)
        {
            out.push(w.to_string());
        }
    }
    out
}

/// The links a drag carries: a link's address (public.url) or the addresses in dragged text.
fn pasteboard_links(pb: &NSPasteboard) -> Vec<String> {
    let mut text = String::new();
    for t in unsafe { [NSPasteboardTypeURL, NSPasteboardTypeString] } {
        if let Some(s) = pb.stringForType(t) {
            text.push_str(&s.to_string());
            text.push('\n');
        }
    }
    dropped_links(&text)
}

/// Finder files: winit takes those (dropped files).
#[allow(deprecated)]
fn has_files(pb: &NSPasteboard) -> bool {
    pb.propertyListForType(unsafe { NSFilenamesPboardType })
        .is_some()
}

fn drag_pasteboard(sender: *mut AnyObject) -> Option<Retained<NSPasteboard>> {
    if sender.is_null() {
        return None;
    }
    unsafe { msg_send![&*sender, draggingPasteboard] }
}

#[allow(deprecated)]
fn drag_types() -> Retained<NSArray<NSString>> {
    unsafe { NSArray::from_slice(&[NSPasteboardTypeURL, NSPasteboardTypeString]) }
}

/// winit's own drag handlers on the main window's delegate (files), which links go around.
static WINIT_ENTERED: OnceLock<Imp> = OnceLock::new();
static WINIT_PERFORM: OnceLock<Imp> = OnceLock::new();
type WinitDrag = unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject) -> Bool;

unsafe extern "C-unwind" fn main_entered(
    this: *mut AnyObject,
    sel: Sel,
    sender: *mut AnyObject,
) -> NSUInteger {
    if let Some(pb) = drag_pasteboard(sender).filter(|pb| !has_files(pb)) {
        return if pasteboard_links(&pb).is_empty() {
            0
        } else {
            NSDragOperation::Copy.0
        };
    }
    match WINIT_ENTERED.get() {
        Some(f) => {
            let f: WinitDrag = std::mem::transmute(*f);
            if f(this, sel, sender).as_bool() {
                NSDragOperation::Copy.0
            } else {
                0
            }
        }
        None => 0,
    }
}

unsafe extern "C-unwind" fn main_perform(
    this: *mut AnyObject,
    sel: Sel,
    sender: *mut AnyObject,
) -> Bool {
    if let Some(pb) = drag_pasteboard(sender).filter(|pb| !has_files(pb)) {
        let links = pasteboard_links(&pb);
        if links.is_empty() {
            return Bool::NO;
        }
        DROPS.lock().unwrap().extend(links);
        wake_ui(false);
        return Bool::YES;
    }
    match WINIT_PERFORM.get() {
        Some(f) => {
            let f: WinitDrag = std::mem::transmute(*f);
            f(this, sel, sender)
        }
        None => Bool::NO,
    }
}

/// `window` is the main window's NSView (0: only the repaint, set before the window exists). winit's
/// window takes dropped files only; NDM registers it for links and text too and puts its own handlers in
/// front of winit's on the window's delegate, handing files on to winit.
pub fn accept_drops(window: isize, wake: impl Fn() + Send + Sync + 'static) {
    let _ = WAKE.set(Box::new(wake));
    if window == 0 || WINIT_ENTERED.get().is_some() {
        return;
    }
    let view = unsafe { &*(window as *const NSView) };
    let Some(delegate) = view.window().and_then(|w| {
        w.registerForDraggedTypes(&drag_types());
        w.delegate()
    }) else {
        eprintln!("link drops on the main window: no window delegate");
        return;
    };
    let obj: &AnyObject = (*delegate).as_ref();
    let cls = obj.class() as *const AnyClass as *mut AnyClass;
    unsafe {
        let entered: Imp = std::mem::transmute(
            main_entered
                as unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject) -> NSUInteger,
        );
        let perform: Imp = std::mem::transmute(
            main_perform
                as unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject) -> Bool,
        );
        if let Some(old) =
            objc2::ffi::class_replaceMethod(cls, sel!(draggingEntered:), entered, c"Q@:@".as_ptr())
        {
            let _ = WINIT_ENTERED.set(old);
        }
        if let Some(old) = objc2::ffi::class_replaceMethod(
            cls,
            sel!(performDragOperation:),
            perform,
            c"B@:@".as_ptr(),
        ) {
            let _ = WINIT_PERFORM.set(old);
        }
    }
    if WINIT_ENTERED.get().is_none() || WINIT_PERFORM.get().is_none() {
        eprintln!(
            "link drops on the main window: the window's delegate ({}) has no drag handlers",
            obj.class().name().to_string_lossy()
        );
    }
}

define_class!(
    /// The drop target's picture: links dropped on it are downloaded; drag it to move it, double click
    /// for the main window, right click to close it.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NDMDropTargetView"]
    #[ivars = Retained<NSImage>]
    struct DropView;

    impl DropView {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            let all = self.bounds();
            self.ivars().drawInRect_fromRect_operation_fraction(all, NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0)), NSCompositingOperation::SourceOver, 1.0);
        }

        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            if event.clickCount() >= 2 {
                wake_ui(true);
            } else if let Some(w) = self.window() {
                w.performWindowDragWithEvent(event);
            }
        }

        #[unsafe(method(rightMouseDown:))]
        fn right_mouse_down(&self, _event: &NSEvent) {
            // close it; "Show drop target" brings it back
            if let Some(w) = self.window() {
                w.orderOut(None);
            }
            DROP_SHOWN.store(false, Relaxed);
            wake_ui(false);
        }

        #[unsafe(method(draggingEntered:))]
        fn dragging_entered(&self, sender: *mut AnyObject) -> NSUInteger {
            match drag_pasteboard(sender) {
                Some(pb) if !pasteboard_links(&pb).is_empty() => NSDragOperation::Copy.0,
                _ => 0,
            }
        }

        #[unsafe(method(performDragOperation:))]
        fn perform_drag_operation(&self, sender: *mut AnyObject) -> Bool {
            let links = drag_pasteboard(sender).map(|pb| pasteboard_links(&pb)).unwrap_or_default();
            if links.is_empty() {
                return Bool::NO;
            }
            DROPS.lock().unwrap().extend(links);
            wake_ui(true);
            Bool::YES
        }
    }
);

/// Show or hide the drop target: a floating picture over every window and Space, bottom right of the
/// screen, that links can be dragged onto.
pub fn drop_target(show: bool, picture: &Rgba) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    DROP_WIN.with(|cell| {
        let mut cell = cell.borrow_mut();
        if cell.is_none() {
            if !show {
                return;
            }
            *cell = make_drop_target(mtm, picture);
        }
        let Some(panel) = cell.as_ref() else { return };
        if show {
            panel.orderFrontRegardless();
        } else {
            panel.orderOut(None);
        }
        DROP_SHOWN.store(show, Relaxed);
    });
}

fn make_drop_target(mtm: MainThreadMarker, picture: &Rgba) -> Option<Retained<NSPanel>> {
    const SIZE: f64 = 64.0;
    let image = image_of(picture, SIZE)?;
    let area = NSScreen::mainScreen(mtm).map(|s| s.visibleFrame())?;
    let at = NSPoint::new(
        area.origin.x + area.size.width - SIZE - 40.0,
        area.origin.y + 120.0,
    );
    let frame = NSRect::new(at, NSSize::new(SIZE, SIZE));
    let style = NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel;
    let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
        NSPanel::alloc(mtm),
        frame,
        style,
        NSBackingStoreType::Buffered,
        false,
    );
    unsafe { panel.setReleasedWhenClosed(false) };
    panel.setOpaque(false);
    panel.setBackgroundColor(Some(&NSColor::clearColor()));
    panel.setHasShadow(false);
    panel.setLevel(NSFloatingWindowLevel);
    panel.setFloatingPanel(true);
    panel.setBecomesKeyOnlyIfNeeded(true);
    panel.setHidesOnDeactivate(false);
    panel.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::Stationary
            | NSWindowCollectionBehavior::IgnoresCycle,
    );
    let view = DropView::alloc(mtm).set_ivars(image);
    let view: Retained<DropView> = unsafe {
        msg_send![super(view), initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), frame.size)]
    };
    view.registerForDraggedTypes(&drag_types());
    panel.setContentView(Some(&view));
    Some(panel)
}

pub fn drop_target_visible() -> bool {
    DROP_SHOWN.load(Relaxed)
}

#[test]
fn links_from_a_drag() {
    let pb = NSPasteboard::pasteboardWithUniqueName();
    pb.clearContents();
    pb.setString_forType(&NSString::from_str("https://example.com/a.zip"), unsafe {
        NSPasteboardTypeURL
    });
    pb.setString_forType(&NSString::from_str("https://example.com/a.zip"), unsafe {
        NSPasteboardTypeString
    });
    assert_eq!(pasteboard_links(&pb), ["https://example.com/a.zip"]);
    assert!(!has_files(&pb));
    pb.clearContents();
    pb.setString_forType(
        &NSString::from_str("see http://a.example/x and ftp://b.example/y, or not:this"),
        unsafe { NSPasteboardTypeString },
    );
    assert_eq!(
        pasteboard_links(&pb),
        ["http://a.example/x", "ftp://b.example/y"]
    );
}
