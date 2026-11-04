use super::*;

/// What the window's close button does.
#[derive(Debug, PartialEq)]
pub(in crate::app) enum Close {
    Hide,
    Minimize,
    Quit,
}

/// To the tray when there is one; without one, minimized while downloads or queues run, else quit.
pub(in crate::app) fn close_action(tray: bool, busy: bool) -> Close {
    match (tray, busy) {
        (true, _) => Close::Hide,
        (false, true) => Close::Minimize,
        (false, false) => Close::Quit,
    }
}

/// Hides the main window on Linux and macOS; Wayland can't hide a window, so there it is minimized.
#[cfg(not(windows))]
fn hide_main(ctx: &egui::Context) {
    let hide = cfg!(target_os = "macos") || win::prefer_x11();
    ctx.send_viewport_cmd(if hide { ViewportCommand::Visible(false) } else { ViewportCommand::Minimized(true) });
}

/// NDM's windows are laid out in dialog units and break when zoomed, so the UI never zooms.
pub(in crate::app) fn no_zoom(ctx: &egui::Context) {
    ctx.options_mut(|o| o.zoom_with_keyboard = false);
    if ctx.zoom_factor() != 1.0 {
        ctx.set_zoom_factor(1.0);
    }
}

impl App {
    /// Linux: an AppIndicator on the GTK thread (tray-icon needs GTK's loop and keeps its items there), only
    /// where the desktop shows one; else closing quits. Sets LINUX_TRAY; the items stay on that thread
    /// (`LIMIT_ITEM`, for the Speed Limiter tick and a language change).
    #[cfg(target_os = "linux")]
    pub(in crate::app) fn tray(ctx: &egui::Context, core: &Arc<Core>, _: &Arc<AtomicIsize>) -> Option<(TrayIcon, CheckMenuItem, Vec<(MenuItem, &'static str)>)> {
        let style = core.settings().tray_icon;
        // without the indicator library tray-icon panics on the GTK thread, which aborts NDM
        if style == 3 || !win::tray_host() || !win::indicator_lib() {
            return None;
        }
        let (core, ctx) = (core.clone(), ctx.clone());
        win::gtk_spawn(move || {
            // the desktop lays the menu out itself (right-to-left languages too)
            let open = MenuItem::new(i18n::tr_os("Open"), true, None);
            let startq = MenuItem::new(i18n::tr_os("Start queue"), true, None);
            let stopq = MenuItem::new(i18n::tr_os("Stop queue"), true, None);
            let limit = CheckMenuItem::new(i18n::tr_os("Speed Limiter"), true, core.settings().limit_on, None);
            let exit = MenuItem::new(i18n::tr_os("Exit"), true, None);
            let menu = Menu::new();
            if menu.append_items(&[&open, &PredefinedMenuItem::separator(), &startq, &stopq, &limit, &PredefinedMenuItem::separator(), &exit]).is_err() {
                return;
            }
            let Some(tray) = tray_picture(style).and_then(|i| TrayIconBuilder::new().with_menu(Box::new(menu)).with_tooltip(TITLE).with_icon(i).build().ok()) else {
                return;
            };
            LINUX_TRAY.store(true, Relaxed);
            let ids = (open.id().clone(), startq.id().clone(), stopq.id().clone(), limit.id().clone(), exit.id().clone());
            MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
                let (open, startq, stopq, limit, exit) = &ids;
                if e.id == *open {
                    ctx.send_viewport_cmd(ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(ViewportCommand::Focus);
                } else if e.id == *startq {
                    core.start_queue(core::queue::MAIN_QUEUE);
                } else if e.id == *stopq {
                    core.stop_queue(core::queue::MAIN_QUEUE);
                } else if e.id == *limit {
                    let on = !core.settings().limit_on;
                    core.settings().limit_on = on;
                    core.apply_settings();
                } else if e.id == *exit {
                    core.shutdown();
                    std::process::exit(0);
                }
                ctx.request_repaint();
            }));
            let items = vec![(open, "Open"), (startq, "Start queue"), (stopq, "Stop queue"), (exit, "Exit")];
            LIMIT_ITEM.with(|l| *l.borrow_mut() = Some((limit, items)));
            std::mem::forget(tray); // lives as long as the app
        });
        None
    }

    /// macOS: NDM's item in the menu bar, made on the main thread like the window; its menu shows on a click.
    /// The window is hidden and shown with egui's viewport commands.
    #[cfg(target_os = "macos")]
    pub(in crate::app) fn tray(ctx: &egui::Context, core: &Arc<Core>, _: &Arc<AtomicIsize>) -> Option<(TrayIcon, CheckMenuItem, Vec<(MenuItem, &'static str)>)> {
        let open = MenuItem::new(i18n::tr_os("Open"), true, None);
        let startq = MenuItem::new(i18n::tr_os("Start queue"), true, None);
        let stopq = MenuItem::new(i18n::tr_os("Stop queue"), true, None);
        let limit = CheckMenuItem::new(i18n::tr_os("Speed Limiter"), true, core.settings().limit_on, None);
        let exit = MenuItem::new(i18n::tr_os("Exit"), true, None);
        let menu = Menu::new();
        menu.append_items(&[&open, &PredefinedMenuItem::separator(), &startq, &stopq, &limit, &PredefinedMenuItem::separator(), &exit]).ok()?;
        let style = core.settings().tray_icon;
        if style == 3 {
            return None;
        }
        let tray = TrayIconBuilder::new().with_menu(Box::new(menu)).with_tooltip(TITLE).with_icon(tray_picture(style)?).build().map_err(|e| eprintln!("tray: {e}")).ok()?;
        let ids = (open.id().clone(), startq.id().clone(), stopq.id().clone(), limit.id().clone(), exit.id().clone());
        let (core2, ctx2) = (core.clone(), ctx.clone());
        MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
            let (open, startq, stopq, limit, exit) = &ids;
            if e.id == *open {
                ctx2.send_viewport_cmd(ViewportCommand::Visible(true));
                ctx2.send_viewport_cmd(ViewportCommand::Minimized(false));
                ctx2.send_viewport_cmd(ViewportCommand::Focus);
            } else if e.id == *startq {
                core2.start_queue(core::queue::MAIN_QUEUE);
            } else if e.id == *stopq {
                core2.stop_queue(core::queue::MAIN_QUEUE);
            } else if e.id == *limit {
                let on = !core2.settings().limit_on;
                core2.settings().limit_on = on;
                core2.apply_settings();
            } else if e.id == *exit {
                core2.shutdown();
                std::process::exit(0);
            }
            ctx2.request_repaint();
        }));
        let items = vec![(open, "Open"), (startq, "Start queue"), (stopq, "Stop queue"), (exit, "Exit")];
        Some((tray, limit, items))
    }

    /// Notification-area icon. Its handlers run on the UI thread's message loop even while
    /// the window is hidden, so they act on Core and the HWND directly.
    #[cfg(windows)]
    pub(in crate::app) fn tray(ctx: &egui::Context, core: &Arc<Core>, hwnd: &Arc<AtomicIsize>) -> Option<(TrayIcon, CheckMenuItem, Vec<(MenuItem, &'static str)>)> {
        let open = MenuItem::new(i18n::tr_os("Open"), true, None);
        let startq = MenuItem::new(i18n::tr_os("Start queue"), true, None);
        let stopq = MenuItem::new(i18n::tr_os("Stop queue"), true, None);
        let limit = CheckMenuItem::new(i18n::tr_os("Speed Limiter"), true, core.settings().limit_on, None);
        let exit = MenuItem::new(i18n::tr_os("Exit"), true, None);
        let menu = Menu::new();
        menu.append_items(&[&open, &PredefinedMenuItem::separator(), &startq, &stopq, &limit, &PredefinedMenuItem::separator(), &exit]).ok()?;
        let style = core.settings().tray_icon;
        let tray = TrayIconBuilder::new().with_menu(Box::new(menu)).with_tooltip(TITLE).with_icon(tray_picture(style)?).build().map_err(|e| eprintln!("tray: {e}")).ok()?;
        let _ = tray.set_visible(style != 3);

        let ids = (open.id().clone(), startq.id().clone(), stopq.id().clone(), limit.id().clone(), exit.id().clone());
        let (core2, hwnd2, ctx2) = (core.clone(), hwnd.clone(), ctx.clone());
        MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
            let (open, startq, stopq, limit, exit) = &ids;
            if e.id == *open {
                win::show(hwnd2.load(Relaxed));
            } else if e.id == *startq {
                core2.start_queue(core::queue::MAIN_QUEUE);
            } else if e.id == *stopq {
                core2.stop_queue(core::queue::MAIN_QUEUE);
            } else if e.id == *limit {
                let on = !core2.settings().limit_on;
                core2.settings().limit_on = on;
                core2.apply_settings();
            } else if e.id == *exit {
                win::exit_splash(&i18n::tr_os("Exit NDM"), &i18n::tr_os("Saving settings..."));
                core2.shutdown();
                std::process::exit(0);
            }
            ctx2.request_repaint();
        }));
        let (hwnd3, ctx3) = (hwnd.clone(), ctx.clone());
        TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
            if matches!(e, TrayIconEvent::DoubleClick { button: MouseButton::Left, .. } | TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. }) {
                win::show(hwnd3.load(Relaxed));
                ctx3.request_repaint();
            }
        }));
        let items = vec![(open, "Open"), (startq, "Start queue"), (stopq, "Stop queue"), (exit, "Exit")];
        Some((tray, limit, items))
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        PAINTED.store(true, Relaxed);
        no_zoom(ctx);
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        if self.hwnd.load(Relaxed) == 0 {
            if let Ok(RawWindowHandle::Win32(h)) = frame.window_handle().map(|h| h.as_raw()) {
                self.hwnd.store(h.hwnd.get(), Relaxed);
                win::pump(h.hwnd.get());
                win::dark_title(h.hwnd.get(), self.dark);
                let c = ctx.clone();
                win::accept_drops(h.hwnd.get(), move || c.request_repaint());
            }
        }
        // Linux and macOS: links dropped on the main window (X11; Wayland's own windows take none)
        #[cfg(not(windows))]
        {
            static XDND: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
            if !XDND.load(Relaxed) {
                if let Ok(raw) = frame.window_handle().map(|h| h.as_raw()) {
                    XDND.store(true, Relaxed);
                    let xid = match raw {
                        RawWindowHandle::Xlib(h) => h.window as isize,
                        RawWindowHandle::Xcb(h) => h.window.get() as isize,
                        // macOS: the window's view; links dropped on it come through mac::accept_drops
                        #[cfg(target_os = "macos")]
                        RawWindowHandle::AppKit(h) => h.ns_view.as_ptr() as isize,
                        _ => 0,
                    };
                    MAIN_XID.store(xid, Relaxed);
                    let c = ctx.clone();
                    win::accept_drops(xid, move || c.request_repaint());
                }
            }
        }
        // Closing the window keeps NDM in the notification area; without a tray it minimizes while
        // something downloads, so the downloads aren't lost, and quits otherwise.
        if ctx.input(|i| i.viewport().close_requested()) && !self.quitting {
            let queues: Vec<u32> = self.core.settings().queues.iter().map(|q| q.id).collect();
            let busy = self.core.jobs().iter().any(|j| j.running()) || queues.iter().any(|q| self.core.queue_running(*q));
            match close_action(self.tray.is_some() || LINUX_TRAY.load(Relaxed), busy) {
                Close::Hide => {
                    ctx.send_viewport_cmd(ViewportCommand::CancelClose);
                    win::hide(self.hwnd.load(Relaxed));
                    #[cfg(not(windows))]
                    hide_main(ctx);
                    return;
                }
                Close::Minimize => {
                    ctx.send_viewport_cmd(ViewportCommand::CancelClose);
                    ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
                    let shown = std::mem::replace(&mut self.core.settings().close_note_shown, true);
                    if !shown {
                        self.open(Dialog::Info { title: TITLE.into(), text: "New Download Manager keeps downloading while its window is minimized. To quit, use Tasks → Exit.".into() });
                    }
                }
                Close::Quit => {}
            }
        }
        if let Some((tray, limit, _)) = &self.tray {
            let on = self.core.settings().limit_on;
            if limit.is_checked() != on {
                limit.set_checked(on);
            }
            let total: f64 = self.core.jobs().iter().map(|j| j.speed()).sum();
            let _ = tray.set_tooltip(Some(if total > 0.0 { format!("{TITLE}\n{}", rate(total)) } else { TITLE.to_string() }));
        }
        // eframe shows the main window after its first frame whatever NDM asks. Started for the browser,
        // that show is held back (Windows) or made minimized (X11) by `keep_hidden`, then the window goes to
        // the tray, so only the download's dialog comes up.
        static FRAME: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        static TRAYED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        let n = FRAME.fetch_add(1, Relaxed);
        let h = self.hwnd.load(Relaxed);
        #[cfg(windows)]
        let native = h;
        #[cfg(not(windows))]
        let native = MAIN_XID.load(Relaxed);
        if tray_start() && !TRAYED.load(Relaxed) {
            if n == 0 {
                win::keep_hidden(native, true);
                // the next frame comes even though the window stays hidden
                win::wake(h);
                ctx.request_repaint();
            } else {
                #[cfg(windows)]
                {
                    win::hide(h);
                    TRAYED.store(true, Relaxed);
                }
                // Linux: to the tray once it is up (it starts on its own thread); without one it stays minimized
                #[cfg(target_os = "linux")]
                {
                    let tray = LINUX_TRAY.load(Relaxed);
                    if tray {
                        hide_main(ctx);
                    }
                    if tray || n > 20 {
                        win::keep_hidden(native, false);
                        TRAYED.store(true, Relaxed);
                    } else {
                        ctx.request_repaint_after(Duration::from_millis(100));
                    }
                }
                // macOS: to the menu bar item
                #[cfg(target_os = "macos")]
                {
                    if self.tray.is_some() {
                        hide_main(ctx);
                    }
                    TRAYED.store(true, Relaxed);
                }
            }
        }
        #[cfg(target_os = "linux")]
        if LINUX_TRAY.load(Relaxed) {
            // the tray menu's Speed Limiter tick follows the setting (its item lives on the GTK thread)
            static LIMIT_SHOWN: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(2);
            let on = self.core.settings().limit_on;
            if LIMIT_SHOWN.swap(on as u8, Relaxed) != on as u8 {
                win::gtk_spawn(move || LIMIT_ITEM.with(|l| l.borrow().as_ref().map(|(l, _)| l.set_checked(on))).unwrap_or_default());
            }
        }
        // a link dropped on the drop target, or its double click, brings a hidden main window back
        #[cfg(not(windows))]
        if win::take_show() {
            ctx.send_viewport_cmd(ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(ViewportCommand::Focus);
        }
        while let Ok(inc) = self.rx.try_recv() {
            self.incoming(ctx, inc);
        }
        // links dropped on the drop target or the main window: as if added by hand (Add URL)
        for url in win::take_drops() {
            self.incoming(ctx, Incoming { url, force: true, ..Default::default() });
        }
        let want_help = take_help_request();
        if let Some(p) = want_help {
            let (f, a) = p.split_once('#').map_or((p.clone(), None), |(f, a)| (f.to_string(), Some(a.to_string())));
            match self.dialogs.iter_mut().find_map(|(_, d)| if let Dialog::Help { page, anchor, .. } = d { Some((page, anchor)) } else { None }) {
                Some((page, anchor)) => (*page, *anchor) = (f, a),
                None => self.open(Dialog::Help { page: f, anchor: a, back: vec![], fwd: vec![], search: String::new() }),
            }
        }
        // after the incoming downloads: their dialogs need frames while the main window is in the tray
        win::PUMP.store(!self.dialogs.is_empty(), Relaxed);
        let want = self.core.settings().drop_target;
        if want != self.drop_shown {
            // the drop target shows NDM's icon
            let px = app_icon();
            win::drop_target(want, &px);
            self.drop_shown = want;
        } else if want && !win::drop_target_visible() {
            // closed from its own right click
            self.core.settings().drop_target = false;
            self.drop_shown = false;
        }
        for j in self.core.jobs() {
            if j.just_completed.swap(false, Relaxed) {
                // a file whose name didn't say what it is: named and filed by its contents
                if self.core.classify(&j) {
                    self.core.save();
                }
                let id = j.info().id;
                self.dialogs.retain(|(_, d)| !matches!(d, Dialog::Progress { id: i, .. } if *i == id));
                let (exe, args) = { let s = self.core.settings(); (s.virus_exe.clone(), s.virus_args.clone()) };
                if !exe.is_empty() {
                    let mut c = std::process::Command::new(&exe);
                    win::clean_env(&mut c);
                    if let Err(e) = c.args(virus_args(&args, &j.info().path())).spawn() {
                        self.open(Dialog::Info { title: "Virus checking".into(), text: i18n::trf_os("Can not start virus checking program.\n%s", &[&e]) });
                    }
                }
                if self.core.settings().show_complete {
                    self.open(Dialog::Complete { id, dont_show: false });
                }
                if let Some((exit, off)) = self.on_done.remove(&id) {
                    if off {
                        // 60 s grace period; `shutdown /a` cancels it
                        #[cfg(windows)]
                        let _ = std::process::Command::new("shutdown").args(["/s", "/t", "60", "/c", "Downloads complete. The computer will shut down in 60 seconds."]).spawn();
                        #[cfg(not(windows))]
                        win::power_off(0, false);
                    }
                    if exit || off {
                        self.quitting = true;
                        ctx.send_viewport_cmd(ViewportCommand::Close);
                    }
                }
            }
        }

        let rows = self.rows();
        let visible: HashSet<u64> = rows.iter().map(|r| r.id).collect();
        self.sel.retain(|id| visible.contains(id));
        let sel: Vec<u64> = rows.iter().filter(|r| self.sel.contains(&r.id)).map(|r| r.id).collect();

        if !ctx.wants_keyboard_input() {
            if ctx.input(|i| i.key_pressed(Key::Delete)) && !sel.is_empty() {
                self.ask_delete(sel.clone());
            }
            if ctx.input(|i| i.modifiers.command && i.key_pressed(Key::A)) {
                self.sel = visible;
            }
        }
        if ctx.input(|i| i.modifiers.command && i.key_pressed(Key::F)) {
            self.open_find();
        }
        if ctx.input(|i| i.key_pressed(Key::F3)) {
            self.find_next();
        }
        if ctx.input(|i| i.modifiers.command && i.key_pressed(Key::M)) && self.sel.len() == 1 {
            let id = *self.sel.iter().next().unwrap();
            self.move_rename(id);
        }

        // NDM's menu bar: 24 px tall, first item 3 px from the edge
        let nt = dlu::ndm().then(|| dlu::theme(self.dark));
        let menu_frame = match &nt {
            Some(t) => egui::Frame::new().inner_margin(egui::Margin { left: 6, right: 6, top: 3, bottom: 2 }).fill(t.surface),
            None => egui::Frame::new().inner_margin(egui::Margin { left: 2, right: 2, top: 1, bottom: 0 }).fill(if self.dark { DARK_TREE } else { ctx.style().visuals.panel_fill }),
        };
        egui::TopBottomPanel::top("menu").frame(menu_frame).show_separator_line(false).show(ctx, |ui| {
            if self.dark && nt.is_none() {
                ui.visuals_mut().widgets.hovered.weak_bg_fill = DARK_MENU_HOT;
                ui.visuals_mut().widgets.open.weak_bg_fill = DARK_MENU_HOT;
            }
            // right-to-left languages: menu bar, toolbar, panes and columns run from the right
            // mirrored window (egui's horizontal layouts follow the parent's direction)
            if i18n::is_rtl() {
                ui.with_layout(Layout::right_to_left(Align::Min), |ui| self.menu(ctx, ui, &sel));
            } else {
                self.menu(ctx, ui, &sel)
            }
        });
        // NDM's toolbar starts at the window edge; the tree pane is 145 px wide, 8 px in, the list 7 px right of
        // it (measured at 125%).
        let m = |l: f32, r: f32, t: f32, b: f32| egui::Margin { left: l as i8, right: r as i8, top: t as i8, bottom: b as i8 };
        let fill = ctx.style().visuals.panel_fill;
        let tb_frame = match &nt {
            Some(t) => egui::Frame::new().fill(t.surface).inner_margin(m(6.0, 6.0, 2.0, 4.0)),
            None => egui::Frame::new().fill(fill).inner_margin(m(0.0, 0.0, 0.0, 3.0)),
        };
        let tb = egui::TopBottomPanel::top("toolbar").frame(tb_frame).show_separator_line(false).show(ctx, |ui| self.toolbar(ui, &rows, &sel));
        if let Some(t) = &nt {
            // one hairline under the menu and toolbar
            let y = tb.response.rect.bottom() - 0.5;
            ctx.layer_painter(egui::LayerId::background()).hline(ctx.screen_rect().x_range(), y, egui::Stroke::new(1.0_f32, t.border));
        }
        if let Some(t) = &nt {
            self.status_bar(ctx, t, &rows);
        }
        let main_font = |ui: &mut egui::Ui| _ = ui.style_mut().text_styles.insert(egui::TextStyle::Body, FontId::proportional(10.0));
        if self.show_cats {
            let side = if i18n::is_rtl() { egui::SidePanel::right("cats") } else { egui::SidePanel::left("cats") };
            let side_frame = match &nt {
                Some(t) => egui::Frame::new().fill(t.surface).inner_margin(m(6.0, 6.0, 6.0, 6.0)),
                None => egui::Frame::new().fill(fill).inner_margin(m(6.0, 0.0, 3.0, 3.0)),
            };
            let sp = side.frame(side_frame).show_separator_line(false).resizable(true).default_width(if nt.is_some() { 170.0 } else { 122.0 }).min_width(80.0).show(ctx, |ui| {
                main_font(ui);
                self.categories(ui)
            });
            if let Some(t) = &nt {
                let r = sp.response.rect;
                let x = if i18n::is_rtl() { r.left() + 0.5 } else { r.right() - 0.5 };
                ctx.layer_painter(egui::LayerId::background()).vline(x, r.y_range(), egui::Stroke::new(1.0_f32, t.border));
            }
        }
        let list = match &nt {
            Some(t) => egui::Frame::new().fill(t.surface).stroke(egui::Stroke::new(1.0_f32, t.border)).corner_radius(dlu::R_CARD + 1.0).inner_margin(egui::Margin { left: 2, right: 2, top: 2, bottom: 6 }),
            None => {
                let edge = if self.dark { Color32::from_gray(70) } else { Color32::from_gray(170) };
                egui::Frame::new().fill(if self.dark { DARK_DLG } else { Color32::WHITE }).stroke(egui::Stroke::new(1.0_f32, edge)).inner_margin(2.0)
            }
        };
        let central = if nt.is_some() { egui::Frame::new().fill(fill).inner_margin(m(10.0, 10.0, 10.0, 8.0)) } else { egui::Frame::new().fill(fill).inner_margin(m(5.0, 6.0, 3.0, 3.0)) };
        egui::CentralPanel::default().frame(central).show(ctx, |ui| {
            // View -> Custom scale: the list's text
            let px = { let s = self.core.settings(); s.list_font_px * s.scale_text };
            // the filter box: the list narrows to the downloads with the text in their name, address or description
            ui.with_layout(menu_layout(), |ui| {
                ui.add_space(if dlu::ndm() { 6.0 } else { 2.0 });
                let edit = egui::TextEdit::singleline(&mut self.filter).hint_text(tr("Filter the list")).desired_width(240.0);
                let r = ui.add(edit);
                if !self.filter.is_empty() && (ui.small_button("\u{2716}").on_hover_text(tr("Clear")).clicked() || (r.has_focus() && ui.input(|i| i.key_pressed(Key::Escape)))) {
                    self.filter.clear();
                }
            });
            ui.add_space(3.0);
            ui.style_mut().text_styles.insert(egui::TextStyle::Body, FontId::new(px, egui::FontFamily::Name("list".into())));
            list.show(ui, |ui| self.table(ui, &rows));
        });

        let mut ds = std::mem::take(&mut self.dialogs);
        ds.retain_mut(|(key, d)| self.dialog(ctx, *key, d));
        ds.append(&mut self.dialogs);
        self.dialogs = ds;
        let open: HashSet<u64> = self.dialogs.iter().map(|(k, _)| *k).collect();
        self.dlg_pos.retain(|k, _| open.contains(k));
        if !self.opt_patch.is_empty() && !self.dialogs.iter().any(|(_, d)| matches!(d, Dialog::Options { .. })) {
            // Options was closed first: the change goes straight to the settings
            let mut s = self.core.settings().clone();
            self.opt_patch.drain(..).for_each(|f| f(&mut s));
            *self.core.settings() = s;
            self.core.save();
        }
        self.watch_clipboard();
        // Follow Windows' app mode (checked about every 2 s) when "Dark Mode support" is on.
        self.theme_check = self.theme_check.saturating_add(1);
        let mut retitle = false;
        if self.theme_check > 6 {
            self.theme_check = 0;
            let dark = self.core.settings().dark_support && win::system_dark();
            if dark != self.dark {
                self.dark = dark;
                style(ctx, dark);
                win::dark_title(self.hwnd.load(Relaxed), dark);
                retitle = true;
            }
        }
        let light = std::mem::take(&mut self.light_titles);
        win::own_dialogs(self.hwnd.load(Relaxed), self.dark, retitle, &light);
        if self.core.dial_text.lock().unwrap().is_some() && !self.dialogs.iter().any(|(_, d)| matches!(d, Dialog::DialUp)) {
            self.open(Dialog::DialUp);
        }
        let quota_msg = self.core.quota_msg.lock().unwrap().take();
        if let Some(text) = quota_msg {
            self.open(Dialog::QuotaHit { text });
        }
        let finished: Vec<u32> = std::mem::take(&mut *self.core.queue_finished.lock().unwrap());
        let mut done: Vec<core::queue::Sched> = finished.iter().filter_map(|q| self.core.settings().queue(*q).map(|q| q.sched.clone())).collect();
        done.append(&mut self.core.sched_done.lock().unwrap()); // scheduled grabber projects
        for s in done {
            if s.open_on && !s.open_file.trim().is_empty() {
                explore(s.open_file.trim());
            }
            if s.hangup {
                win::ras_hangup_all();
            }
            if s.off_done {
                win::power_off(s.off_kind, s.off_force);
            }
            // sleep and hibernate come back to a running app
            if s.exit_done || (s.off_done && s.off_kind == 0) {
                self.quitting = true;
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
        }

        ctx.request_repaint_after(Duration::from_millis(300));
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if !self.updating {
            win::exit_splash(&i18n::tr_os("Exit NDM"), &i18n::tr_os("Saving settings..."));
        }
        self.core.shutdown();
    }
}
