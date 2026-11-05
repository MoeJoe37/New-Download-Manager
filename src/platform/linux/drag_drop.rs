use super::{gdk, glib, gtk_spawn, AtomicBool, Mutex, OnceLock, Relaxed, Rgba};
use gtk::prelude::{GdkContextExt, GtkWindowExt, MonitorExt, WidgetExt, WidgetExtManual};

/// Links dropped on the drop target, for the UI to take (`take_drops`).
static DROPS: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// Repaints the UI after a drop (set by `accept_drops`).
static WAKE: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();
/// The UI should bring the main window back (a drop, or a double click on the drop target).
static SHOW_MAIN: AtomicBool = AtomicBool::new(false);
static DROP_SHOWN: AtomicBool = AtomicBool::new(false);
thread_local!(static DROP_WIN: std::cell::RefCell<Option<gtk::Window>> = const { std::cell::RefCell::new(None) });

pub fn take_drops() -> Vec<String> {
    std::mem::take(&mut DROPS.lock().unwrap())
}

/// Should the main window come back (see SHOW_MAIN)? Clears the request.
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

/// Links from a drop: a link's address (text/uri-list) or every address in dropped text.
fn dropped_links(text: &str) -> Vec<String> {
    text.split_whitespace()
        .filter(|w| {
            ["http://", "https://", "ftp://"]
                .iter()
                .any(|s| w.to_ascii_lowercase().starts_with(s))
        })
        .map(str::to_string)
        .collect()
}

/// Links dropped on the main window. winit's X11 drop handler takes file:// only, so NDM gives the window an
/// XdndProxy: drag sources talk to a window of NDM's own (on its own X connection) that takes links and text.
/// `xid` 0 only sets the repaint (the drop target); on Wayland without XWayland there is no X window.
pub fn accept_drops(xid: isize, wake: impl Fn() + Send + Sync + 'static) {
    let _ = WAKE.set(Box::new(wake));
    if xid != 0 {
        std::thread::spawn(move || {
            if let Err(e) = xdnd_proxy(xid as u32) {
                eprintln!("link drops on the main window: {e}");
            }
        });
    }
}

/// Started for the browser (`/tray`): eframe maps the main window after its first frame whatever NDM asks,
/// so it is asked to map minimized (WM_HINTS initial state Iconic) until NDM hides it to the tray; `false`
/// puts the normal state back for the next time it is shown. X11 only (`xid` 0 on Wayland does nothing).
pub fn keep_hidden(xid: isize, on: bool) {
    if xid != 0 {
        let _ = initial_state(xid as u32, if on { 3 } else { 1 });
    }
}

fn initial_state(xid: u32, state: u32) -> Result<(), Box<dyn std::error::Error>> {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _, PropMode};
    use x11rb::wrapper::ConnectionExt as _;
    let (conn, _) = x11rb::connect(None)?;
    let r = conn
        .get_property(false, xid, AtomEnum::WM_HINTS, AtomEnum::WM_HINTS, 0, 9)?
        .reply()?;
    let mut hints: Vec<u32> = r.value32().map(|v| v.collect()).unwrap_or_default();
    hints.resize(9, 0);
    // flags: StateHint (2); then input, initial_state
    hints[0] |= 2;
    hints[2] = state;
    conn.change_property32(
        PropMode::REPLACE,
        xid,
        AtomEnum::WM_HINTS,
        AtomEnum::WM_HINTS,
        &hints,
    )?;
    conn.flush()?;
    Ok(())
}

/// The drop formats NDM takes, most useful first.
const DROP_TYPES: [&str; 4] = [
    "text/uri-list",
    "text/x-moz-url",
    "UTF8_STRING",
    "text/plain",
];

/// Text from a drop in `kind` (text/x-moz-url is UTF-16: "url\ntitle").
fn drop_text(kind: &str, data: &[u8]) -> String {
    if kind == "text/x-moz-url" {
        return String::from_utf16_lossy(
            &data
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_le_bytes(*c))
                .collect::<Vec<_>>(),
        );
    }
    String::from_utf8_lossy(data).into_owned()
}

/// XDND (version 5) target for the main window, run on its own thread.
fn xdnd_proxy(main: u32) -> Result<(), Box<dyn std::error::Error>> {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{
        AtomEnum, ClientMessageEvent, ConnectionExt as _, CreateWindowAux, EventMask, PropMode,
        WindowClass,
    };
    use x11rb::protocol::Event;
    use x11rb::wrapper::ConnectionExt as _;
    let (conn, screen) = x11rb::connect(None)?;
    let root = conn.setup().roots[screen].root;
    let atom = |n: &str| -> Result<u32, Box<dyn std::error::Error>> {
        Ok(conn.intern_atom(false, n.as_bytes())?.reply()?.atom)
    };
    let [aware, proxy, enter, position, status, leave, drop, finished, selection, copy, type_list, prop] =
        [
            "XdndAware",
            "XdndProxy",
            "XdndEnter",
            "XdndPosition",
            "XdndStatus",
            "XdndLeave",
            "XdndDrop",
            "XdndFinished",
            "XdndSelection",
            "XdndActionCopy",
            "XdndTypeList",
            "NDM_DROP",
        ]
        .map(atom);
    let (
        aware,
        proxy,
        enter,
        position,
        status,
        leave,
        drop,
        finished,
        selection,
        copy,
        type_list,
        prop,
    ) = (
        aware?, proxy?, enter?, position?, status?, leave?, drop?, finished?, selection?, copy?,
        type_list?, prop?,
    );
    let kinds: Vec<(u32, &str)> = DROP_TYPES
        .iter()
        .map(|t| atom(t).map(|a| (a, *t)))
        .collect::<Result<_, _>>()?;
    let me = conn.generate_id()?;
    conn.create_window(
        0,
        me,
        root,
        -10,
        -10,
        1,
        1,
        0,
        WindowClass::INPUT_ONLY,
        0,
        &CreateWindowAux::new(),
    )?;
    conn.change_property32(PropMode::REPLACE, me, aware, AtomEnum::ATOM, &[5])?;
    conn.change_property32(PropMode::REPLACE, me, proxy, AtomEnum::WINDOW, &[me])?;
    conn.change_property32(PropMode::REPLACE, main, proxy, AtomEnum::WINDOW, &[me])?;
    conn.flush()?;
    let send = |to: u32, kind: u32, data: [u32; 5]| -> Result<(), Box<dyn std::error::Error>> {
        conn.send_event(
            false,
            to,
            EventMask::NO_EVENT,
            ClientMessageEvent::new(32, to, kind, data),
        )?;
        conn.flush()?;
        Ok(())
    };
    // the drag in progress: (its source, the format NDM asks for)
    let mut drag: Option<(u32, Option<(u32, &str)>)> = None;
    loop {
        match conn.wait_for_event()? {
            Event::ClientMessage(e) if e.format == 32 => {
                let d = e.data.as_data32();
                let source = d[0];
                if e.type_ == enter {
                    // up to three formats in the message, or all of them in XdndTypeList
                    let offered: Vec<u32> = if d[1] & 1 == 1 {
                        conn.get_property(false, source, type_list, AtomEnum::ATOM, 0, 1024)?
                            .reply()?
                            .value32()
                            .map(|v| v.collect())
                            .unwrap_or_default()
                    } else {
                        d[2..5].iter().copied().filter(|a| *a != 0).collect()
                    };
                    drag = Some((
                        source,
                        kinds.iter().find(|(a, _)| offered.contains(a)).copied(),
                    ));
                } else if e.type_ == position {
                    let ok = drag
                        .as_ref()
                        .is_some_and(|(s, k)| *s == source && k.is_some());
                    // accepted (bit 0), keep sending positions (bit 1); the target is the window the source sees
                    send(
                        source,
                        status,
                        [
                            main,
                            if ok { 3 } else { 2 },
                            0,
                            0,
                            if ok { copy } else { 0 },
                        ],
                    )?;
                } else if e.type_ == leave {
                    drag = None;
                } else if e.type_ == drop {
                    match drag.as_ref().and_then(|(s, k)| k.filter(|_| *s == source)) {
                        Some((kind, _)) => {
                            conn.convert_selection(me, selection, kind, prop, d[2])?;
                            conn.flush()?;
                        }
                        None => {
                            send(source, finished, [main, 0, 0, 0, 0])?;
                            drag = None;
                        }
                    }
                }
            }
            Event::SelectionNotify(e) if e.requestor == me => {
                let Some((source, Some((_, kind)))) = drag.take() else {
                    continue;
                };
                let mut got = false;
                if e.property != u32::from(AtomEnum::NONE) {
                    let r = conn
                        .get_property(true, me, prop, AtomEnum::ANY, 0, 1 << 20)?
                        .reply()?;
                    let links = dropped_links(&drop_text(kind, &r.value));
                    if !links.is_empty() {
                        DROPS.lock().unwrap().extend(links);
                        got = true;
                        wake_ui(false);
                    }
                }
                send(
                    source,
                    finished,
                    [main, got as u32, if got { copy } else { 0 }, 0, 0],
                )?;
            }
            _ => {}
        }
    }
}

/// Show or hide the drop target: a floating, always-on-top picture links can be dragged onto, bottom
/// right of the screen. Drag it to move it, double click for the main window, right click to close it.
pub fn drop_target(show: bool, (iw, ih, px): &Rgba) {
    let (w, h, px) = (*iw as i32, *ih as i32, px.clone());
    gtk_spawn(move || {
        DROP_WIN.with(|cell| {
            let mut cell = cell.borrow_mut();
            if cell.is_none() {
                if !show {
                    return;
                }
                *cell = Some(make_drop_target(w, h, px));
            }
            let win = cell.as_ref().unwrap();
            if show {
                win.show_all()
            } else {
                win.hide()
            }
            DROP_SHOWN.store(show, Relaxed);
        })
    });
}

fn make_drop_target(w: i32, h: i32, px: Vec<u8>) -> gtk::Window {
    let win = gtk::Window::new(gtk::WindowType::Toplevel);
    win.set_title("Drop target");
    win.set_decorated(false);
    win.set_keep_above(true);
    win.set_skip_taskbar_hint(true);
    win.set_skip_pager_hint(true);
    win.set_type_hint(gdk::WindowTypeHint::Utility);
    win.set_accept_focus(false);
    win.set_default_size(w, h);
    win.set_resizable(false);
    // see-through where the picture is, if the screen composites
    if let Some(v) = WidgetExt::screen(&win).and_then(|s| s.rgba_visual()) {
        win.set_visual(Some(&v));
    }
    win.set_app_paintable(true);
    let pb = gtk::gdk_pixbuf::Pixbuf::from_bytes(
        &glib::Bytes::from_owned(px),
        gtk::gdk_pixbuf::Colorspace::Rgb,
        true,
        8,
        w,
        h,
        w * 4,
    );
    win.connect_draw(move |_, cr| {
        cr.set_operator(gtk::cairo::Operator::Source);
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.0);
        let _ = cr.paint();
        cr.set_operator(gtk::cairo::Operator::Over);
        cr.set_source_pixbuf(&pb, 0.0, 0.0);
        let _ = cr.paint();
        glib::Propagation::Stop
    });
    if let Some(area) = gdk::Display::default()
        .and_then(|d| d.primary_monitor().or_else(|| d.monitor(0)))
        .map(|m| m.workarea())
    {
        win.move_(
            area.x() + area.width() - w - 40,
            area.y() + area.height() - h - 120,
        );
    }
    win.add_events(gdk::EventMask::BUTTON_PRESS_MASK);
    win.connect_button_press_event(|win, e| {
        match (e.button(), e.event_type()) {
            (1, gdk::EventType::DoubleButtonPress) => wake_ui(true),
            (1, gdk::EventType::ButtonPress) => {
                let (x, y) = e.root();
                win.begin_move_drag(1, x as i32, y as i32, e.time());
            }
            (3, gdk::EventType::ButtonPress) => {
                // close it; "Show drop target" brings it back
                win.hide();
                DROP_SHOWN.store(false, Relaxed);
                wake_ui(false);
            }
            _ => {}
        }
        glib::Propagation::Stop
    });
    let targets = [
        "text/uri-list",
        "text/x-moz-url",
        "UTF8_STRING",
        "text/plain",
    ]
    .map(|t| gtk::TargetEntry::new(t, gtk::TargetFlags::OTHER_APP, 0));
    win.drag_dest_set(
        gtk::DestDefaults::ALL,
        &targets,
        gdk::DragAction::COPY | gdk::DragAction::LINK | gdk::DragAction::MOVE,
    );
    win.connect_drag_data_received(|_, _, _, _, data, _, _| {
        let mut text = data.uris().join("\n");
        if text.is_empty() {
            // text/x-moz-url is UTF-16: "url\ntitle"
            let raw = data.data();
            text = data.text().map(|t| t.to_string()).unwrap_or_else(|| {
                String::from_utf16_lossy(
                    &raw.as_chunks::<2>()
                        .0
                        .iter()
                        .map(|c| u16::from_le_bytes(*c))
                        .collect::<Vec<_>>(),
                )
            });
        }
        let links = dropped_links(&text);
        if !links.is_empty() {
            DROPS.lock().unwrap().extend(links);
            wake_ui(true);
        }
    });
    win
}

pub fn drop_target_visible() -> bool {
    DROP_SHOWN.load(Relaxed)
}

/// A second start asked for the window: `take_show` hands it to the UI.
pub fn show_main() {
    SHOW_MAIN.store(true, Relaxed);
}
