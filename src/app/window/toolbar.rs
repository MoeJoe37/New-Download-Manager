use crate::app::{
    core, dlu, egui, explore, i18n, segoe, style, toolbar_sets, toolbars_dir, tr, Align, Align2,
    App, Color32, Dialog, FontId, Layout, Relaxed, Row, Sense, Status, Toolbar, Vec2, BAR,
    TB_BUILTIN, TB_BUTTONS,
};

/// (strip index, fallback glyph, label, tooltip, enabled, drop-down)
type TbButton = (usize, &'static str, String, String, bool, bool);

impl App {
    /// NDM's toolbar (RT_TOOLBAR 166: 12 buttons, 56x42 images, no separators). Labels and tooltips are
    /// NDM's strings; buttons share one width (0x45f560); Start/Stop Queue are BTNS_DROPDOWN buttons whose
    /// arrow lists the queues. Right click: NDM's toolbar menu (0x4613c0, popup +0x22c).
    pub(in crate::app) fn toolbar(&mut self, ui: &mut egui::Ui, rows: &[Row], sel: &[u64]) {
        let chosen: Vec<&Row> = rows.iter().filter(|r| self.sel.contains(&r.id)).collect();
        let can_resume = chosen
            .iter()
            .any(|r| !r.running && r.status != Status::Complete);
        let can_stop = chosen.iter().any(|r| r.running);
        let any_running = rows.iter().any(|r| r.running);
        let any_done = rows.iter().any(|r| r.status == Status::Complete);
        let queues: Vec<(u32, String, bool)> = self
            .core
            .settings()
            .queues
            .iter()
            .map(|q| (q.id, tr(&q.name), self.core.queue_running(q.id)))
            .collect();
        let small = self.core.settings().small_buttons;
        let enabled = [
            true,
            can_resume,
            can_stop,
            any_running,
            !sel.is_empty(),
            any_done,
            true,
            true,
            queues.iter().any(|q| !q.2),
            queues.iter().any(|q| q.2),
            true,
            true,
        ];
        let buttons: Vec<TbButton> = TB_BUTTONS
            .iter()
            .enumerate()
            .map(|(k, (g, l, t))| {
                (
                    k,
                    *g,
                    i18n::tr_label(l),
                    tr(t),
                    enabled[k],
                    k == 8 || k == 9,
                )
            })
            .collect();
        let key = {
            let s = self.core.settings();
            (
                s.tb_skin.clone(),
                dlu::ndm(),
                (s.scale_toolbar * 100.0).round() as u32,
            )
        };
        if self.tb_key != key {
            self.toolbar = Some(Toolbar::load(ui.ctx(), &key.0, key.1, key.2 as f32 / 100.0));
            self.tb_key = key;
        }
        if !self.core.settings().classic && !dlu::ndm() {
            // NDM draws an etched line between the menu and the toolbar
            let (r, w) = (ui.max_rect(), ui.ctx().screen_rect().x_range());
            for (dy, g) in [(0.0, 240), (1.0, 160), (2.0, 255)] {
                ui.painter().with_clip_rect(ui.ctx().screen_rect()).hline(
                    w,
                    r.top() - 3.0 + dy + 0.5,
                    egui::Stroke::new(1.0_f32, Color32::from_gray(g)),
                );
            }
        }
        let clicked = if i18n::is_rtl() {
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                self.tool_buttons(ui, &buttons, &queues, small)
            })
            .inner
        } else {
            self.tool_buttons(ui, &buttons, &queues, small)
        };
        match clicked {
            Some(0) => self.add_url(),
            Some(1) => self.resume(sel),
            Some(2) => self.stop(sel),
            Some(3) => self.core.stop_all(),
            Some(4) => self.ask_delete(sel.to_vec()),
            Some(5) => self.remove_completed(),
            Some(6) => self.options(0),
            Some(7) => self.open_scheduler(0),
            // the button itself acts on the main queue
            Some(8) => self.core.start_queue(core::queue::MAIN_QUEUE),
            Some(9) => self.core.stop_queue(core::queue::MAIN_QUEUE),
            Some(10) => self.grabber(),
            Some(11) => App::tell_friend(),
            _ => {}
        }
    }

    /// The toolbar's buttons in the user's order (Customize...), separators as gaps. Returns the clicked button.
    pub(in crate::app) fn tool_buttons(
        &mut self,
        ui: &mut egui::Ui,
        buttons: &[TbButton],
        queues: &[(u32, String, bool)],
        small: bool,
    ) -> Option<usize> {
        // View -> Custom scale: the pictures are made at this size (Toolbar::load), the rest follows
        let sc = self.core.settings().scale_toolbar;
        let font = segoe(11.6 * sc);
        // NDM draws the strip 1:1 in screen pixels at any DPI, every button image width + 19 px wide with the
        // label cut to "..."
        let ppp = ui.ctx().pixels_per_point();
        let cell = |v: f32| ((v * sc).round()).max(8.0) / ppp;
        let (iw, ih) = if small {
            (cell(32.0), cell(32.0))
        } else {
            (cell(56.0), cell(42.0))
        };
        let bw = iw + 19.0 * sc / ppp;
        let bh = ih + 28.0 * sc / ppp;
        let arrow_w = 19.0 * sc / ppp; // BTNS_DROPDOWN arrow: 19 px
        let dark = ui.visuals().dark_mode;
        let ndm = dlu::ndm().then(|| dlu::theme(dark));
        let (hot_fill, hot_edge, down_fill) = if let Some(t) = &ndm {
            (t.raised, Color32::TRANSPARENT, t.accent_soft)
        } else if dark {
            (
                Color32::from_gray(0x45),
                Color32::from_gray(0x60),
                Color32::from_gray(0x55),
            )
        } else {
            (
                Color32::from_rgb(229, 243, 255),
                Color32::from_rgb(204, 232, 255),
                Color32::from_rgb(204, 232, 255),
            )
        };
        let mut clicked = None;
        let mut drop: Option<(usize, egui::Rect)> = None;
        let bar = ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            let layout = self.core.settings().tb_layout.clone();
            for b in layout {
                let Some((k, glyph, label, tip, enabled, dropdown)) = usize::try_from(b)
                    .ok()
                    .and_then(|b| buttons.get(b))
                    .cloned()
                else {
                    let (r, _) = ui.allocate_exact_size(Vec2::new(8.0, bh), Sense::hover());
                    let c = ndm
                        .as_ref()
                        .map_or(Color32::from_gray(if dark { 90 } else { 190 }), |t| {
                            t.border
                        });
                    ui.painter().vline(
                        r.center().x,
                        r.y_range().shrink(10.0 * sc),
                        egui::Stroke::new(1.0_f32, c),
                    );
                    continue;
                };
                let w = bw + if dropdown { arrow_w } else { 0.0 };
                let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, bh), Sense::click());
                let resp = resp.on_hover_text(tip);
                resp.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, &label)
                });
                let arrow = egui::Rect::from_min_max(
                    egui::pos2(rect.right() - arrow_w, rect.top()),
                    rect.max,
                );
                let on_arrow = dropdown && resp.hover_pos().is_some_and(|p| arrow.contains(p));
                let hot = enabled && resp.hovered();
                let p = ui.painter();
                if hot {
                    let fill = if resp.is_pointer_button_down_on() {
                        down_fill
                    } else {
                        hot_fill
                    };
                    let rad = if ndm.is_some() { dlu::R_CARD } else { 2.0 };
                    p.rect_filled(
                        rect.shrink(if ndm.is_some() { 2.0 } else { 1.0 }),
                        rad,
                        fill,
                    );
                    p.rect_stroke(
                        rect.shrink(1.0),
                        2.0,
                        egui::Stroke::new(1.0_f32, hot_edge),
                        egui::StrokeKind::Inside,
                    );
                    if dropdown {
                        p.vline(
                            arrow.left(),
                            arrow.y_range().shrink(3.0),
                            egui::Stroke::new(1.0_f32, hot_edge),
                        );
                    }
                }
                let color = match &ndm {
                    Some(t) => {
                        if enabled {
                            t.text
                        } else {
                            t.muted.gamma_multiply(0.6)
                        }
                    }
                    None => {
                        if enabled {
                            ui.visuals().text_color()
                        } else {
                            Color32::from_gray(if dark { 110 } else { 150 })
                        }
                    }
                };
                let body = egui::Rect::from_min_max(
                    rect.min,
                    egui::pos2(
                        rect.right() - if dropdown { arrow_w } else { 0.0 },
                        rect.bottom(),
                    ),
                );
                let icon = egui::Rect::from_center_size(
                    egui::pos2(body.center().x, rect.top() + 4.0 * sc + ih / 2.0),
                    Vec2::new(iw, ih),
                );
                match self
                    .toolbar
                    .as_ref()
                    .and_then(|t| t.pic(small, k, enabled, hot && !on_arrow, dark))
                {
                    Some((tex, tint)) => {
                        if ndm.is_some() && hot && !on_arrow && tint != Color32::WHITE {
                            // a soft neon glow behind the hovered icon
                            let r = iw.min(ih) * 0.5;
                            for (f, a) in [(1.0, 0.07), (0.78, 0.09), (0.56, 0.11)] {
                                p.circle_filled(
                                    icon.center(),
                                    r * f,
                                    tint.gamma_multiply(if dark { a * 1.6 } else { a }),
                                );
                            }
                        }
                        p.image(
                            tex,
                            icon,
                            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                            tint,
                        );
                    }
                    None => {
                        p.text(
                            icon.center(),
                            Align2::CENTER_CENTER,
                            glyph,
                            FontId::proportional(28.0),
                            if enabled { BAR } else { color },
                        );
                    }
                }
                // a longer translation gets smaller type, then "…" (at the left in right-to-left languages)
                let room = body.width() - 7.0 / ppp;
                let f = dlu::fit_font(ui, &label, font.clone(), room);
                let text = if i18n::is_rtl() {
                    dlu::cut_left(&label, room, |l| {
                        ui.fonts(|fo| fo.layout_no_wrap(l.to_string(), f.clone(), color).size().x)
                    })
                } else {
                    label.clone()
                };
                let mut job = egui::text::LayoutJob::simple_singleline(text, f, color);
                job.wrap = egui::text::TextWrapping {
                    max_width: room,
                    max_rows: 1,
                    break_anywhere: true,
                    overflow_character: Some('…'),
                };
                let g = ui.painter().layout_job(job);
                p.galley(
                    egui::pos2(
                        body.center().x - g.size().x / 2.0,
                        rect.bottom() - 9.0 * sc - g.size().y / 2.0,
                    ),
                    g,
                    color,
                );
                if let (true, Some(t)) = (dropdown, &ndm) {
                    // NDM Style: a thin chevron beside the icon
                    let c = egui::pos2(arrow.center().x, icon.center().y);
                    let d = 3.0 * sc;
                    let pts = vec![
                        c + Vec2::new(-d, -d / 2.0),
                        c + Vec2::new(0.0, d / 2.0),
                        c + Vec2::new(d, -d / 2.0),
                    ];
                    p.add(egui::Shape::line(
                        pts,
                        egui::Stroke::new(1.4_f32, if enabled { t.muted } else { color }),
                    ));
                } else if dropdown {
                    // NDM (measured at 125%): a solid 9x5 px near-black triangle, 3 px right of the arrow
                    // column's middle and 10 px below the icon's centre, black in dark mode too
                    let px = |v: f32| v / ppp;
                    let c = egui::pos2(
                        (arrow.center().x * ppp).round() / ppp + px(3.0),
                        (icon.center().y * ppp).round() / ppp + px(10.0),
                    );
                    let fill = if enabled {
                        Color32::from_gray(4)
                    } else {
                        Color32::from_gray(if dark { 110 } else { 150 })
                    };
                    let pts = vec![
                        c + Vec2::new(px(-4.5), px(-2.5)),
                        c + Vec2::new(px(4.5), px(-2.5)),
                        c + Vec2::new(0.0, px(2.5)),
                    ];
                    p.add(egui::Shape::convex_polygon(pts, fill, egui::Stroke::NONE));
                }
                if enabled && resp.clicked() {
                    if on_arrow {
                        drop = Some((k, rect))
                    } else {
                        clicked = Some(k)
                    }
                }
            }
        });
        bar.response
            .interact(Sense::click())
            .context_menu(|ui| self.toolbar_menu(ui));
        if let Some((k, rect)) = drop {
            let id = ui.id().with("tbdrop");
            ui.memory_mut(|m| m.toggle_popup(id));
            ui.data_mut(|m| m.insert_temp(id, (k, rect.left_bottom())));
        }
        let id = ui.id().with("tbdrop");
        if ui.memory(|m| m.is_popup_open(id)) {
            let (k, at): (usize, egui::Pos2) =
                ui.data(|m| m.get_temp(id)).unwrap_or((8, egui::Pos2::ZERO));
            let start = k == 8;
            let area = egui::Area::new(id)
                .order(egui::Order::Foreground)
                .fixed_pos(at)
                .show(ui.ctx(), |ui| {
                    egui::Frame::menu(ui.style()).show(ui, |ui| {
                        for (q, name, running) in queues {
                            if ui
                                .add_enabled(*running != start, egui::Button::new(name))
                                .clicked()
                            {
                                if start {
                                    self.core.start_queue(*q)
                                } else {
                                    self.core.stop_queue(*q)
                                }
                                ui.memory_mut(|m| m.close_popup());
                            }
                        }
                    });
                });
            if area.response.clicked_elsewhere() && drop.is_none() {
                ui.memory_mut(|m| m.close_popup());
            }
        }
        clicked
    }

    /// NDM's toolbar menu (0x4613c0, also View -> Toolbar): Customize..., Large/Small Buttons, Look for new
    /// toolbars, then Modern / Classic style.
    pub(in crate::app) fn toolbar_menu(&mut self, ui: &mut egui::Ui) {
        if ui.button(tr("Customize...")).clicked() {
            let layout = self.core.settings().tb_layout.clone();
            self.open(Dialog::TbCustomize {
                layout,
                avail: None,
                cur: None,
            });
            ui.close_menu();
        }
        ui.separator();
        let small = self.core.settings().small_buttons;
        for (label, v) in [("Large Buttons", false), ("Small Buttons", true)] {
            if ui.radio(small == v, tr(label)).clicked() {
                self.core.settings().small_buttons = v;
                self.core.save();
                ui.close_menu();
            }
        }
        ui.separator();
        // the built-in pictures, or a custom toolbar from the toolbars folder (README, Custom toolbars)
        let skin = self.core.settings().tb_skin.clone();
        let sets = toolbar_sets();
        let builtin = skin == TB_BUILTIN || !sets.contains(&skin);
        let mut pick = None;
        if ui.radio(builtin, tr("Built-in toolbar")).clicked() {
            pick = Some(TB_BUILTIN.to_string());
        }
        for name in sets {
            if ui.radio(skin == name, name.as_str()).clicked() {
                pick = Some(name);
            }
        }
        if let Some(name) = pick {
            self.core.settings().tb_skin = name;
            self.core.save();
            ui.close_menu();
        }
        if ui.button(tr("Open toolbars folder")).clicked() {
            let dir = toolbars_dir();
            let _ = std::fs::create_dir_all(&dir);
            explore(dir);
            ui.close_menu();
        }
        ui.separator();
        // Modern (tree chevrons, list grid lines), Classic (+/- tree boxes, 3D look) or NDM Style (NDM's own
        // look for every window)
        let (classic, ndm) = {
            let s = self.core.settings();
            (s.classic, s.ndm_style)
        };
        for (name, c, n) in [
            ("Modern style", false, false),
            ("Classic style", true, false),
            ("NDM Style", classic, true),
        ] {
            if ui
                .radio(ndm == n && (n || classic == c), tr(name))
                .clicked()
            {
                {
                    let mut s = self.core.settings();
                    (s.classic, s.ndm_style) = (c, n);
                }
                self.core.save();
                dlu::NDM_STYLE.store(n, Relaxed);
                style(ui.ctx(), self.dark);
                ui.close_menu();
            }
        }
    }
}
