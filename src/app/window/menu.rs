use crate::app::{ALL_CJK, Align, App, Col, Color32, Dialog, FontId, LIST_FONT, Layout, PlaylistPicker, Relaxed, RichText, Row, Sense, UPDATE_REPO, Vec2, ViewportCommand, clipboard, core, crash_diagnostics, dlu, egui, explore, help, i18n, issue_text, links, open_url, rate, segoe, style, tr, tray_picture, win};
#[cfg(target_os = "linux")]
use crate::app::{LIMIT_ITEM};

/// A menu bar's layout: its menus start at the right edge in right-to-left languages (`menu::bar` rows
/// follow their parent's direction).
pub(in crate::app) fn menu_layout() -> Layout {
    if i18n::is_rtl() { Layout::right_to_left(Align::Min) } else { Layout::left_to_right(Align::Min) }
}

impl App {
    /// Help -> "Report a problem...": a new GitHub issue on NDM's page, filled in with NDM's version, the
    /// operating system, architecture and language.
    pub(in crate::app) fn report_problem() {
        let (title, body) = issue_text(env!("CARGO_PKG_VERSION"), &i18n::current());
        let enc = |t: &str| percent_encoding::utf8_percent_encode(t, percent_encoding::NON_ALPHANUMERIC).to_string();
        open_url(&format!("https://github.com/{UPDATE_REPO}/issues/new?title={}&body={}", enc(&title), enc(&body)));
    }

    /// "Tell a friend": a ready-to-send letter in the default mail program.
    pub(in crate::app) fn tell_friend() {
        let enc = |t: &str| percent_encoding::utf8_percent_encode(t, percent_encoding::NON_ALPHANUMERIC).to_string();
        let body = "Hi,\r\n\r\nI use New Download Manager to speed up my downloads. It splits files into several connections, \
resumes broken downloads, has a scheduler, a site grabber and a browser button for videos.\r\n\r\nGive it a try!";
        open_url(&format!("mailto:?subject={}&body={}", enc("A download manager you should try"), enc(body)));
    }

    /// Main menu, NDM's menu resource 129 item for item.
    pub(in crate::app) fn menu(&mut self, ctx: &egui::Context, ui: &mut egui::Ui, sel: &[u64]) {
        egui::menu::bar(ui, |ui| {
            // NDM's menu bar pads each item differently (measured with GetMenuItemRect at 125%, item width
            // minus the label's width in the menu font): Tasks 42, File 28, Downloads 52, View 33, Help 29 px
            let pad = |ui: &mut egui::Ui, px: f32| ui.spacing_mut().button_padding.x = px / 1.25 / 2.0;
            // NDM's menu font, at View -> Custom scale's text size
            let sc = self.core.settings().scale_text;
            ui.style_mut().text_styles.insert(egui::TextStyle::Button, segoe(12.2 * sc));
            ui.spacing_mut().item_spacing.x = 0.0;
            let (font, ink) = (egui::TextStyle::Button.resolve(ui.style()), ui.visuals().text_color());
            let mn = |t: &str| {
                // ponytail: underlines the first letter, as NDM's English and most translations; reading the
                // '&' position from the .lng files would cover the rest. Right-to-left menus have none (the
                // first character drawn is a word's last letter there).
                let t = tr(t);
                let k = if i18n::is_rtl() { 0 } else { t.chars().next().map_or(0, char::len_utf8) };
                let mut job = egui::text::LayoutJob::default();
                let fmt = |u: bool| egui::TextFormat { font_id: font.clone(), color: ink, underline: if u { egui::Stroke::new(1.0_f32, ink) } else { egui::Stroke::NONE }, ..Default::default() };
                job.append(&t[..k], 0.0, fmt(true));
                job.append(&t[k..], 0.0, fmt(false));
                job
            };
            pad(ui, 42.0);
            ui.menu_button(mn("Tasks"), |ui| {
                if ui.button(tr("Add new download")).clicked() {
                    self.add_url();
                    ui.close_menu();
                }
                if ui.button(tr("Add batch download")).clicked() {
                    self.open(Dialog::Batch { url: "http://www.example.com/img*.jpg".into(), from: 1, to: 10, width: 1, letters: false, lf: "a".into(), lt: "z".into(), auth: false, login: String::new(), pass: String::new() });
                    ui.close_menu();
                }
                if ui.button(tr("Add batch download from clipboard")).clicked() {
                    self.add_queued(links(&clipboard()));
                    ui.close_menu();
                }
                if ui.button(tr("Run site grabber")).clicked() {
                    self.grabber();
                    ui.close_menu();
                }
                ui.separator();
                if ui.button(tr("Export extension...")).clicked() {
                    ui.close_menu();
                    self.export_extension();
                }
                let mut dt = self.core.settings().drop_target;
                if ui.checkbox(&mut dt, tr("Show drop target")).clicked() {
                    self.core.settings().drop_target = dt;
                    ui.close_menu();
                }
                ui.separator();
                ui.menu_button(tr("Export"), |ui| {
                    for (t, ef2) in [("To NDM export file", true), ("To text file", false)] {
                        if ui.button(tr(t)).clicked() {
                            let mode = if sel.is_empty() { 2 } else { 1 };
                            self.open(Dialog::Export { ef2, mode });
                            ui.close_menu();
                        }
                    }
                });
                ui.menu_button(tr("Import"), |ui| {
                    for (t, ef2) in [("From NDM export file", true), ("From text file", false)] {
                        if ui.button(tr(t)).clicked() {
                            self.import(ef2);
                            ui.close_menu();
                        }
                    }
                });
                ui.separator();
                if ui.button(tr("Exit")).clicked() {
                    self.quitting = true;
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                }
            });
            pad(ui, 28.0);
            ui.menu_button(mn("File"), |ui| {
                ui.add_enabled_ui(!sel.is_empty(), |ui| {
                    if ui.button(tr("Stop Download")).clicked() {
                        self.stop(sel);
                        ui.close_menu();
                    }
                    if ui.button(tr("Remove")).clicked() {
                        self.ask_delete(sel.to_vec());
                        ui.close_menu();
                    }
                    if ui.button(tr("Download Now")).clicked() {
                        self.resume(sel);
                        ui.close_menu();
                    }
                    if ui.button(tr("Redownload")).clicked() {
                        self.redownload(sel);
                        ui.close_menu();
                    }
                });
            });
            pad(ui, 52.0);
            ui.menu_button(mn("Downloads"), |ui| {
                if ui.button(tr("Pause All")).clicked() || ui.button(tr("Stop All")).clicked() {
                    self.core.stop_all();
                    ui.close_menu();
                }
                ui.separator();
                if ui.button(tr("Delete All Completed")).clicked() {
                    self.remove_completed();
                    ui.close_menu();
                }
                ui.separator();
                if ui.button(tr("Find (Ctrl-F)")).clicked() {
                    self.open_find();
                    ui.close_menu();
                }
                if ui.add_enabled(!self.core.settings().search.is_empty(), egui::Button::new(tr("Find Next (F3)"))).clicked() {
                    self.find_next();
                    ui.close_menu();
                }
                ui.separator();
                if ui.button(tr("Scheduler")).clicked() {
                    self.open_scheduler(0);
                    ui.close_menu();
                }
                let queues: Vec<(u32, String)> = self.core.settings().queues.iter().map(|q| (q.id, tr(&q.name))).collect();
                for start in [true, false] {
                    ui.menu_button(tr(if start { "Start queue" } else { "Stop queue" }), |ui| {
                        for (id, name) in &queues {
                            if ui.add_enabled(self.core.queue_running(*id) != start, egui::Button::new(name)).clicked() {
                                if start { self.core.start_queue(*id) } else { self.core.stop_queue(*id) }
                                ui.close_menu();
                            }
                        }
                    });
                }
                ui.separator();
                ui.menu_button(tr("Speed Limiter"), |ui| {
                    let on = self.core.settings().limit_on;
                    for (label, v) in [("Turn on", true), ("Turn off", false)] {
                        if ui.add_enabled(on != v, egui::Button::new(tr(label))).clicked() {
                            self.core.settings().limit_on = v;
                            self.core.apply_settings();
                            ui.close_menu();
                        }
                    }
                    ui.separator();
                    if ui.button(tr("Settings...")).clicked() {
                        let s = self.core.settings();
                        let d = Dialog::Limiter { kb: s.limit_kb.to_string(), startup: s.limit_startup, sched: s.limit_sched, from: s.limit_from.clone(), to: s.limit_to.clone() };
                        drop(s);
                        self.open(d);
                        ui.close_menu();
                    }
                });
                ui.separator();
                if ui.button(tr("Download video playlist...")).clicked() {
                    self.open(Dialog::Playlist(Box::new(PlaylistPicker::new(links(&clipboard()).into_iter().next().unwrap_or_default()))));
                    ui.close_menu();
                }
                if ui.button(tr("Set yt-dlp")).on_hover_text(tr("Downloads yt-dlp and ffmpeg for YouTube and other video sites, adds them to PATH and keeps yt-dlp up to date")).clicked() {
                    self.set_ytdlp(ctx);
                    ui.close_menu();
                }
                ui.separator();
                if ui.button(tr("Options")).clicked() {
                    self.options(0);
                    ui.close_menu();
                }
            });
            pad(ui, 33.0);
            ui.menu_button(mn("View"), |ui| {
                if ui.button(tr(if self.show_cats { "Hide categories" } else { "Show categories" })).clicked() {
                    self.show_cats = !self.show_cats;
                    ui.close_menu();
                }
                ui.menu_button(tr("Arrange files"), |ui| {
                    use Col::*;
                    let by = [(Added, "Order Of Addition"), (Name, "File Name"), (Size, "Size"), (Status, "Status"), (Left, "Time Left"), (Rate, "Transfer Rate"), (LastTry, "Last Try Date"), (Desc, "Description"), (SaveTo, "Save Path"), (Referer, "Referer"), (Parent, "Parent Web Page")];
                    for (c, n) in by {
                        if ui.radio(self.sort.0 == c, tr(&format!("By {n}"))).clicked() {
                            self.sort_by(c);
                            ui.close_menu();
                        }
                    }
                });
                ui.menu_button(tr("Toolbar"), |ui| self.toolbar_menu(ui));
                ui.menu_button(tr("Category icons"), |ui| self.cat_icons_menu(ui));
                ui.menu_button(tr("NDM tray icon"), |ui| {
                    // 0x45ada0: delete the icon, then add it back with the new picture unless "Don't show"
                    let cur = self.core.settings().tray_icon;
                    for (v, label) in [(1, "3D style"), (2, "Classic style"), (3, "Don't show")] {
                        if ui.radio(cur == v, tr(label)).clicked() {
                            self.core.settings().tray_icon = v;
                            self.core.save();
                            if let Some((tray, ..)) = &self.tray {
                                let _ = tray.set_icon(tray_picture(v));
                                let _ = tray.set_visible(v != 3);
                            }
                            ui.close_menu();
                        }
                    }
                });
                ui.separator();
                if ui.button(tr("Customize URL List...")).clicked() {
                    self.open_columns();
                    ui.close_menu();
                }
                let mut ds = self.core.settings().dark_support;
                if ui.checkbox(&mut ds, tr("Dark Mode support")).on_hover_text(tr("New Download Manager will use Dark Mode when it enabled in Windows settings\r\n(Right click on Desktop -> Personalize -> Colors -> Dark app mode)")).changed() {
                    self.core.settings().dark_support = ds;
                    self.theme_check = u32::MAX;
                }
                ui.menu_button(tr("Font"), |ui| {
                    let (face, px) = { let s = self.core.settings(); (s.list_font.clone(), s.list_font_px) };
                    let mut pick = None;
                    if ui.button(tr("Choose Font...")).clicked() {
                        ui.close_menu();
                        pick = win::choose_font(self.hwnd.load(Relaxed), if face.is_empty() { "Tahoma" } else { &face }, px);
                    }
                    if ui.add_enabled(!face.is_empty(), egui::Button::new(tr("Reset to default Font"))).clicked() {
                        ui.close_menu();
                        pick = Some((String::new(), 10.0));
                    }
                    if let Some((f, px)) = pick {
                        *LIST_FONT.lock().unwrap() = f.clone();
                        { let mut s = self.core.settings(); (s.list_font, s.list_font_px) = (f, px.clamp(6.0, 40.0)); }
                        self.core.save();
                        style(ui.ctx(), self.dark);
                    }
                });
                if ui.button(tr("Custom scale...")).clicked() {
                    let orig = { let s = self.core.settings(); [s.scale_toolbar, s.scale_tree, s.scale_text] };
                    if !self.dialogs.iter().any(|(_, d)| matches!(d, Dialog::Scale { .. })) {
                        self.open(Dialog::Scale { orig });
                    }
                    ui.close_menu();
                }
                ui.separator();
                ui.menu_button(tr("Language"), |ui| {
                    // NDM's Language menu: English plus every Languages\ndm_*.lng
                    if !ALL_CJK.swap(true, Relaxed) {
                        style(ui.ctx(), self.dark); // the Chinese and Korean names need their fonts
                    }
                    let cur = self.core.settings().language.clone();
                    let mut pick = None;
                    egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                        if ui.radio(cur.is_empty(), "English").clicked() {
                            pick = Some(String::new());
                        }
                        for (stem, name) in &self.langs {
                            if ui.radio(cur == *stem, name.as_str()).clicked() {
                                pick = Some(stem.clone());
                            }
                        }
                    });
                    if let Some(stem) = pick {
                        i18n::load(&stem);
                        if let Some((_, limit, items)) = &self.tray {
                            for (m, en) in items {
                                m.set_text(i18n::tr_os(en));
                            }
                            limit.set_text(i18n::tr_os("Speed Limiter"));
                        }
                        #[cfg(target_os = "linux")]
                        win::gtk_spawn(|| {
                            LIMIT_ITEM.with(|l| {
                                if let Some((limit, items)) = &*l.borrow() {
                                    items.iter().for_each(|(m, en)| m.set_text(i18n::tr_os(en)));
                                    limit.set_text(i18n::tr_os("Speed Limiter"));
                                }
                            })
                        });
                        self.core.settings().language = stem;
                        self.core.save();
                        style(ui.ctx(), self.dark);
                        ui.close_menu();
                    }
                });
            });
            pad(ui, 29.0);
            ui.menu_button(mn("Help"), |ui| {
                // NDM's four help entries, each opening a page of NDM's own help
                for (t, page) in [("Help contents", "index.htm"), ("Grabber Help", "grabber.htm"), ("Tutorials", "starting.htm"), ("Scheduler and queues", "scheduler.htm")] {
                    if ui.add_enabled(help(page, false), egui::Button::new(tr(t))).clicked() {
                        help(page, true);
                        ui.close_menu();
                    }
                }
                if ui.button(tr("Tip of the Day...")).clicked() {
                    let text = self.next_tip();
                    self.open(Dialog::Tip { text });
                    ui.close_menu();
                }
                ui.separator();
                // how to make and install them (README)
                for (t, anchor) in [("Look for new toolbars", "custom-toolbars"), ("Look for new category icons", "custom-category-icons")] {
                    if ui.button(tr(t)).clicked() {
                        open_url(&format!("https://github.com/{UPDATE_REPO}#{anchor}"));
                        ui.close_menu();
                    }
                }
                ui.separator();
                // NDM's home is where its releases are published (UPDATE_REPO)
                for (t, page) in [("NDM Home Page", ""), ("Contact NDM Support", "/issues")] {
                    if ui.add_enabled(!UPDATE_REPO.is_empty(), egui::Button::new(tr(t))).clicked() {
                        explore(format!("https://github.com/{UPDATE_REPO}{page}"));
                        ui.close_menu();
                    }
                }
                if ui.button(tr("Report a problem...")).clicked() {
                    let (_, mut text) = issue_text(env!("CARGO_PKG_VERSION"), &i18n::current());
                    text.push_str(&crash_diagnostics(&core::state::store_path()));
                    self.open(Dialog::ProblemReport { text });
                    ui.close_menu();
                }
                ui.separator();
                if ui.button(tr("Check for updates...")).clicked() {
                    self.check_updates(ui.ctx(), false);
                    ui.close_menu();
                }
                ui.separator();
                if ui.button(tr("About New Download Manager")).clicked() {
                    self.open(Dialog::About);
                    ui.close_menu();
                }
                if ui.button(tr("Tell a friend")).clicked() {
                    App::tell_friend();
                    ui.close_menu();
                }
            });
        });
    }

    /// NDM Style's status bar: how many downloads the list holds, how many run, the total speed and
    /// whether the Speed Limiter is on.
    pub(in crate::app) fn status_bar(&mut self, ctx: &egui::Context, t: &dlu::Theme, rows: &[Row]) {
        let sc = self.core.settings().scale_text;
        let frame = egui::Frame::new().fill(t.surface).inner_margin(egui::Margin { left: 12, right: 12, top: 4, bottom: 4 });
        let bar = egui::TopBottomPanel::bottom("status").frame(frame).show_separator_line(false).show(ctx, |ui| {
            let font = FontId::new(10.0 * sc, egui::FontFamily::Name("segoe".into()));
            let running = rows.iter().filter(|r| r.running).count();
            let speed: f64 = self.core.jobs().iter().map(|j| j.speed()).sum();
            let limit = self.core.settings().limit_on;
            let dir = if i18n::is_rtl() { Layout::right_to_left(Align::Center) } else { Layout::left_to_right(Align::Center) };
            ui.with_layout(dir, |ui| {
                ui.spacing_mut().item_spacing.x = 14.0;
                ui.label(RichText::new(i18n::trf("%d downloads", &[&rows.len()])).font(font.clone()).color(t.muted));
                let dot = |ui: &mut egui::Ui, c: Color32| {
                    let (r, _) = ui.allocate_exact_size(Vec2::splat(8.0), Sense::hover());
                    ui.painter().circle_filled(r.center(), 3.5, c);
                };
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 5.0;
                    dot(ui, if running > 0 { t.accent } else { t.border });
                    ui.label(RichText::new(i18n::trf("%d downloading", &[&running])).font(font.clone()).color(if running > 0 { t.text } else { t.muted }));
                });
                if speed > 0.0 {
                    ui.label(RichText::new(rate(speed)).font(font.clone()).color(t.text));
                }
                if limit {
                    ui.label(RichText::new(tr("Speed Limiter on")).font(font.clone()).color(t.accent));
                }
            });
        });
        ctx.layer_painter(egui::LayerId::background()).hline(ctx.screen_rect().x_range(), bar.response.rect.top() + 0.5, egui::Stroke::new(1.0_f32, t.border));
    }
}
