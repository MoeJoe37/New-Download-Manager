use crate::app::dialogs::{PlaylistLookup, PlaylistPicker};
use crate::app::{channel, egui, engine, i18n, tr, tr_os, ytdlp, App, Duration, Info};

impl App {
    pub(super) fn dialog_playlist(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        pick: &mut Box<PlaylistPicker>,
    ) -> bool {
        if let Some(lookup) = &pick.pending {
            match lookup.results.try_recv() {
                Ok(Ok(list)) => {
                    pick.selected = vec![false; list.entries.len()];
                    pick.list = Some(list);
                    pick.pending = None;
                }
                Ok(Err(error)) => {
                    pick.error = error;
                    pick.pending = None;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    pick.error = tr_os("Playlist lookup stopped unexpectedly.");
                    pick.pending = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    ctx.request_repaint_after(Duration::from_millis(150))
                }
            }
        }
        ui.label(tr("Playlist address"));
        let busy = pick.pending.is_some();
        let edited = ui
            .add_enabled(
                !busy,
                egui::TextEdit::singleline(&mut pick.url).desired_width(f32::INFINITY),
            )
            .changed();
        let old_browser = pick.browser.clone();
        let (mut load, mut stop) = (false, false);
        ui.horizontal(|ui| {
            ui.add_enabled_ui(!busy, |ui| {
                let label = pick
                    .browsers
                    .iter()
                    .find(|b| b.id == pick.browser)
                    .map_or("Public access", |b| b.label);
                egui::ComboBox::from_id_salt("playlist-browser")
                    .selected_text(tr(label))
                    .show_ui(ui, |ui| {
                        crate::dlu::scale_popup(ui);
                        if ui
                            .selectable_value(&mut pick.browser, String::new(), tr("Public access"))
                            .clicked()
                        {
                            ui.close_menu();
                        }
                        for browser in &pick.browsers {
                            if ui
                                .selectable_value(
                                    &mut pick.browser,
                                    browser.id.to_string(),
                                    tr(browser.label),
                                )
                                .clicked()
                            {
                                ui.close_menu();
                            }
                        }
                    });
            });
            load = ui
                .add_enabled(
                    !busy && !pick.url.trim().is_empty(),
                    egui::Button::new(tr("List videos")),
                )
                .clicked();
            if busy {
                ui.spinner();
                stop = ui.button(tr("Stop")).clicked();
            }
        });
        ui.label(tr("Selecting a browser lets yt-dlp use its default profile for this playlist and each queued video."));
        if edited || old_browser != pick.browser {
            pick.list = None;
            pick.error.clear();
        }
        if stop {
            pick.pending = None;
        }
        if load {
            pick.list = None;
            pick.error.clear();
            if let Some(url) = ytdlp::playlist::playlist_url(&pick.url) {
                let core = self.core.clone();
                let browser = pick.browser.clone();
                let (tx, rx) = channel();
                let job = engine::Job::new(Info::default());
                let worker = job.clone();
                std::thread::spawn(move || {
                    let result =
                        ytdlp::playlist::list_playlist(&worker, &core.agent(&url), &url, &browser);
                    let _ = tx.send(result);
                });
                pick.pending = Some(PlaylistLookup { job, results: rx });
            } else {
                pick.error =
                    tr_os("Enter an HTTP or HTTPS playlist address without embedded credentials.");
            }
        }
        if !pick.error.is_empty() {
            ui.label(tr(&pick.error));
        }
        if let Some(list) = &pick.list {
            if list.limited {
                ui.label(i18n::trf(
                    "Showing entries from the first %d playlist positions.",
                    &[&ytdlp::playlist::PLAYLIST_LIMIT],
                ));
            }
            ui.horizontal(|ui| {
                if ui.button(tr("Select all")).clicked() {
                    pick.selected.fill(true);
                }
                if ui.button(tr("Clear selection")).clicked() {
                    pick.selected.fill(false);
                }
            });
            egui::ScrollArea::vertical()
                .max_height((ui.available_height() - 42.0).max(70.0))
                .show(ui, |ui| {
                    for (entry, selected) in list.entries.iter().zip(&mut pick.selected) {
                        ui.checkbox(selected, i18n::shown(&entry.title));
                    }
                });
        }
        let mut close = false;
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    pick.list.is_some() && pick.selected.contains(&true),
                    egui::Button::new(tr("Queue selected videos")),
                )
                .clicked()
            {
                let mut ids = Vec::new();
                if let Some(list) = &pick.list {
                    for (entry, &selected) in list.entries.iter().zip(&pick.selected) {
                        if !selected {
                            continue;
                        }
                        let mut info = self.core.new_info(&entry.url);
                        info.url.clone_from(&entry.url);
                        info.ytdlp = true;
                        info.yt.playlist_entry = true;
                        info.yt.browser.clone_from(&pick.browser);
                        info.queued = true;
                        ids.push(info.id);
                        self.core.add(info);
                    }
                }
                self.core.save();
                self.pick_queue(ids, true);
                close = true;
            }
            close |= ui.button(tr("Close")).clicked();
        });
        !close
    }
}
