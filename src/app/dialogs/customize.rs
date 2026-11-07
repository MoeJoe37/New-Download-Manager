use crate::app::{
    core, dlu, egui, help, i18n, tr, tr_os, Align2, App, Color32, FontId, Sense, Vec2, COLS,
    TB_BUTTONS, TB_ICONS,
};

impl App {
    pub(super) fn dialog_columns(
        &mut self,
        ui: &mut egui::Ui,
        list: &mut Vec<(u8, bool, u16)>,
        sel: &mut usize,
        width: &mut String,
    ) -> bool {
        let mut d = dlu::D::new(ui);
        d.text(7.0, 7.0, 222.0, 25.0, "Check the columns that you would like visible in this list. Use the Move Up and Move Down buttons to reorder the columns however you like.");
        let r = d.listview(7.0, 36.0, 139.0, 87.0, &[]);
        d.ui.scope_builder(egui::UiBuilder::new().max_rect(r.shrink(2.0)), |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink(false)
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    for (k, (c, vis, w)) in list.iter_mut().enumerate() {
                        ui.horizontal(|ui| {
                            // File Name can't be hidden
                            ui.add_enabled(*c != 0, egui::Checkbox::without_text(vis));
                            if ui
                                .add_sized(
                                    [ui.available_width(), 16.0],
                                    egui::SelectableLabel::new(*sel == k, tr(COLS[*c as usize].1)),
                                )
                                .clicked()
                            {
                                (*sel, *width) = (k, w.to_string());
                            }
                        });
                    }
                });
        });
        let n = list.len();
        if d.button(158.0, 36.0, 71.0, 14.0, "Move Up", *sel > 0) {
            list.swap(*sel, *sel - 1);
            *sel -= 1;
        }
        if d.button(158.0, 54.0, 71.0, 14.0, "Move Down", *sel + 1 < n) {
            list.swap(*sel, *sel + 1);
            *sel += 1;
        }
        let (c, vis, _) = list[*sel];
        if d.button(158.0, 72.0, 71.0, 14.0, "Show", !vis) {
            list[*sel].1 = true;
        }
        if d.button(158.0, 90.0, 71.0, 14.0, "Hide", vis && c != 0) {
            list[*sel].1 = false;
        }
        if d.button(158.0, 108.0, 71.0, 14.0, "Reset", true) {
            *list = core::settings::DEFAULT_COLUMNS
                .iter()
                .map(|(k, w)| (*k, true, *w))
                .collect();
            list.extend(
                COLS.iter()
                    .enumerate()
                    .filter(|(k, _)| {
                        !core::settings::DEFAULT_COLUMNS
                            .iter()
                            .any(|c| c.0 as usize == *k)
                    })
                    .map(|(k, c)| (k as u8, false, c.2)),
            );
            (*sel, *width) = (0, list[0].2.to_string());
        }
        d.label_r(7.0, 132.0, 123.0, 10.0, "The selected column should be");
        if d.edit(136.0, 130.0, 20.0, 14.0, width, true).changed() {
            if let Some(w) = width
                .trim()
                .parse::<u16>()
                .ok()
                .filter(|w| (10..=2000).contains(w))
            {
                list[*sel].2 = w;
            }
        }
        d.label(159.0, 132.0, 66.0, 10.0, "pixels wide");
        d.hline(7.0, 149.0, 222.0);
        let ok = d.button(107.0, 155.0, 50.0, 14.0, "OK", true);
        if ok {
            self.core.settings().columns =
                list.iter().filter(|c| c.1).map(|c| (c.0, c.2)).collect();
            self.core.save();
        }
        !(ok || d.button(179.0, 155.0, 50.0, 14.0, "Cancel", true))
    }

    pub(super) fn dialog_scale(&mut self, ui: &mut egui::Ui, orig: &mut [f32; 3]) -> bool {
        let mut d = dlu::D::new(ui);
        d.text(7.0, 6.0, 218.0, 18.0, "Change the size of the toolbar, the categories pane and the text of the main window. Changes show at once.");
        let before = {
            let s = self.core.settings();
            [s.scale_toolbar, s.scale_tree, s.scale_text]
        };
        let mut v = before;
        for (name, k) in [("Toolbar", 0), ("Categories pane", 1), ("Text size", 2)] {
            let y = 28.0 + k as f32 * 15.0;
            d.label(7.0, y + 2.0, 66.0, 9.0, name);
            let mut pct = (v[k] * 100.0).round() as i32;
            d.at(76.0, y, 149.0, 13.0, |ui| {
                ui.spacing_mut().slider_width = (ui.available_width() - 58.0).max(40.0);
                ui.add(
                    egui::Slider::new(&mut pct, 50..=200)
                        .step_by(5.0)
                        .suffix("%")
                        .trailing_fill(true),
                );
            });
            v[k] = pct as f32 / 100.0;
        }
        d.hline(7.0, 76.0, 218.0);
        if d.button(7.0, 81.0, 50.0, 14.0, "Reset", v != [1.0; 3]) {
            v = [1.0; 3];
        }
        let ok = d.button(120.0, 81.0, 50.0, 14.0, "OK", true);
        let cancel = d.button(175.0, 81.0, 50.0, 14.0, "Cancel", true);
        if cancel {
            v = *orig;
        }
        if v != before {
            let mut s = self.core.settings();
            [s.scale_toolbar, s.scale_tree, s.scale_text] = v;
        }
        if ok || cancel {
            self.core.save();
        }
        !(ok || cancel)
    }

    pub(super) fn dialog_tb_customize(
        &mut self,
        ui: &mut egui::Ui,
        layout: &mut Vec<i8>,
        avail: &mut Option<usize>,
        cur: &mut Option<usize>,
    ) -> bool {
        let mut d = dlu::D::new(ui);
        // available: a separator always, plus the buttons not on the bar (comctl32's lists)
        let left: Vec<i8> = std::iter::once(-1)
            .chain((0..TB_ICONS as i8).filter(|b| !layout.contains(b)))
            .collect();
        let dark = d.ui.visuals().dark_mode;
        let pics: Vec<Option<(egui::TextureId, Color32)>> = (0..TB_ICONS)
            .map(|k| {
                self.toolbar
                    .as_ref()
                    .and_then(|t| t.pic(k, true, false, dark))
            })
            .collect();
        let list = |ui: &mut egui::Ui, items: &[i8], sel: &mut Option<usize>| {
            egui::ScrollArea::vertical()
                .auto_shrink(false)
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    for (k, b) in items.iter().enumerate() {
                        let r = ui.allocate_exact_size(
                            Vec2::new(ui.available_width(), 22.0),
                            Sense::click(),
                        );
                        if *sel == Some(k) {
                            ui.painter()
                                .rect_filled(r.0, 0.0, ui.visuals().selection.bg_fill);
                        }
                        let label = match usize::try_from(*b) {
                            Ok(b) => {
                                if let Some((t, tint)) = pics[b] {
                                    ui.painter().image(
                                        t,
                                        egui::Rect::from_min_size(
                                            r.0.min + Vec2::new(1.0, 2.0),
                                            Vec2::new(24.0, 18.0),
                                        ),
                                        egui::Rect::from_min_max(
                                            egui::pos2(0.0, 0.0),
                                            egui::pos2(1.0, 1.0),
                                        ),
                                        tint,
                                    );
                                }
                                i18n::tr_label(TB_BUTTONS[b].1)
                            }
                            Err(_) => tr("Separator"),
                        };
                        ui.painter().text(
                            r.0.left_center() + Vec2::new(26.0, 0.0),
                            Align2::LEFT_CENTER,
                            label,
                            FontId::proportional(12.0),
                            ui.visuals().text_color(),
                        );
                        if r.1.clicked() {
                            *sel = Some(k);
                        }
                    }
                });
        };
        d.label(4.0, 5.0, 84.0, 10.0, "Available toolbar buttons:");
        let r = d.listview(4.0, 17.0, 120.0, 100.0, &[]);
        d.ui.scope_builder(egui::UiBuilder::new().max_rect(r.shrink(2.0)), |ui| {
            list(ui, &left, avail)
        });
        d.label(182.0, 5.0, 78.0, 10.0, "Current toolbar buttons:");
        let r = d.listview(182.0, 17.0, 120.0, 100.0, &[]);
        d.ui.scope_builder(egui::UiBuilder::new().max_rect(r.shrink(2.0)), |ui| {
            list(ui, layout, cur)
        });
        let before = layout.clone();
        if d.button(
            131.0,
            42.0,
            44.0,
            14.0,
            &i18n::line(&format!("{} ->", tr_os("Add"))),
            avail.is_some_and(|a| a < left.len()),
        ) {
            // inserted before the selected current button, like comctl32
            let at = cur.map_or(layout.len(), |c| c.min(layout.len()));
            layout.insert(at, left[avail.unwrap()]);
            (*avail, *cur) = (None, Some(at));
        }
        if d.button(
            131.0,
            62.0,
            44.0,
            14.0,
            &i18n::line(&format!("<- {}", tr_os("Remove"))),
            cur.is_some_and(|c| c < layout.len()),
        ) {
            layout.remove(cur.unwrap());
            *cur = None;
        }
        let close = d.button(308.0, 6.0, 44.0, 14.0, "Close", true);
        if d.button(308.0, 23.0, 44.0, 14.0, "Reset", true) {
            *layout = core::settings::Settings::default().tb_layout;
            (*avail, *cur) = (None, None);
        }
        if d.button(308.0, 40.0, 44.0, 14.0, "Help", help("index.htm", false)) {
            help("customization.htm", true);
        }
        if let Some(c) = cur.filter(|c| *c < layout.len()) {
            if d.button(308.0, 74.0, 44.0, 14.0, "Move Up", c > 0) {
                layout.swap(c, c - 1);
                *cur = Some(c - 1);
            }
            if d.button(308.0, 91.0, 44.0, 14.0, "Move Down", c + 1 < layout.len()) {
                layout.swap(c, c + 1);
                *cur = Some(c + 1);
            }
        } else {
            d.button(308.0, 74.0, 44.0, 14.0, "Move Up", false);
            d.button(308.0, 91.0, 44.0, 14.0, "Move Down", false);
        }
        // comctl32 applies every change to the bar at once
        if *layout != before {
            self.core.settings().tb_layout = layout.clone();
            self.core.save();
        }
        !close
    }
}
