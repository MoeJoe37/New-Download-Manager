use crate::app::{App, Arc, Duration, Relaxed, RichText, channel, dlu, egui, i18n, tr, tr_os};
use crate::app::dialogs::{ChecksumSource};

impl App {
    pub(super) fn dialog_checksum(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        name: &str,
        run: &mut Arc<crate::checksum::Run>,
        expect: &mut String,
        source: &mut ChecksumSource,
    ) -> bool {
        let mut d = dlu::D::new(ui);
        d.label(7.0, 7.0, 286.0, 9.0, name);
        let done = run.result.lock().unwrap().as_ref().map(|r| r.as_ref().map(|s| (s.sha256.clone(), s.sha1.clone(), s.md5.clone())).map_err(Clone::clone));
        let busy = match &done {
            None => i18n::trf_os("Computing... %d%%", &[&(run.done.load(Relaxed) * 100 / run.total.max(1))]),
            Some(Err(e)) => e.clone(),
            Some(Ok(_)) => String::new(),
        };
        let sums = done.as_ref().and_then(|r| r.as_ref().ok()).cloned();
        for (k, (label, v)) in [("SHA-256:", sums.as_ref().map(|s| s.0.clone())), ("SHA-1:", sums.as_ref().map(|s| s.1.clone())), ("MD5:", sums.as_ref().map(|s| s.2.clone()))].into_iter().enumerate() {
            let y = 21.0 + k as f32 * 16.0;
            d.label(7.0, y + 3.0, 40.0, 9.0, label);
            // an edit box, so the value can be selected and copied
            let mut text = v.unwrap_or_else(|| busy.clone());
            d.edit(50.0, y, 243.0, 14.0, &mut text, sums.is_some());
        }
        d.label(7.0, 73.0, 286.0, 9.0, "Checksum from the download page (paste it to compare):");
        d.edit(7.0, 84.0, 286.0, 14.0, expect, true);
        if let (Some((a, b, c)), false) = (&sums, expect.trim().is_empty()) {
            let s = crate::checksum::Sums { sha256: a.clone(), sha1: b.clone(), md5: c.clone() };
            let (text, ok) = match crate::checksum::compare(&s, expect) {
                Some((kind, true)) => (i18n::trf_os("The %s checksums match: the file is intact.", &[&kind]), Some(true)),
                Some((kind, false)) => (i18n::trf_os("The %s checksums don't match: the file is damaged or isn't the one published.", &[&kind]), Some(false)),
                None => (tr_os("That isn't a SHA-256, SHA-1 or MD5 checksum."), None),
            };
            let th = dlu::theme(d.ui.visuals().dark_mode);
            let ink = match ok { Some(true) => th.success, Some(false) => th.danger, None => th.muted };
            d.label(7.0, 102.0, 286.0, 9.0, RichText::new(tr(&text)).color(ink));
        }
        if done.is_none() {
            d.ui.ctx().request_repaint_after(Duration::from_millis(150));
        }
        if let Some(rx) = &source.pending {
            match rx.try_recv() {
                Ok(result) => {
                    source.preview = Some(result);
                    source.pending = None;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    source.preview = Some(Err("Checksum lookup stopped unexpectedly.".into()));
                    source.pending = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => ctx.request_repaint_after(Duration::from_millis(150)),
            }
        }
        d.label(7.0, 115.0, 286.0, 9.0, "Publisher checksum list (HTTPS)");
        if d.edit(7.0, 127.0, 286.0, 14.0, &mut source.url, source.pending.is_none()).changed() {
            source.preview = None;
        }
        let idle = source.pending.is_none();
        if d.button(7.0, 147.0, 86.0, 14.0, "Choose checksum file...", idle) {
            if let Some(path) = rfd::FileDialog::new().set_title(tr_os("Choose checksum file...")).pick_file() {
                source.preview = Some(crate::checksum::from_file(&path, name));
            }
        }
        if d.button(99.0, 147.0, 69.0, 14.0, "Fetch checksum", idle && !source.url.trim().is_empty()) {
            if let Some(url) = crate::checksum::publisher_url(&source.url) {
                let core = self.core.clone();
                let name = name.to_owned();
                let (tx, rx) = channel();
                std::thread::spawn(move || {
                    let result = crate::checksum::from_publisher(&core.agent(url.as_str()), url.as_str(), &name);
                    let _ = tx.send(result);
                });
                source.preview = None;
                source.pending = Some(rx);
            } else {
                source.preview = Some(Err("Enter the publisher's HTTPS checksum-list address.".into()));
            }
        }
        match &source.preview {
            Some(Ok(hash)) => {
                d.label(7.0, 168.0, 286.0, 9.0, RichText::new(i18n::trf("Matched SHA-256 for %s", &[&name])));
                d.readonly(7.0, 180.0, 286.0, 14.0, hash);
                if d.button(7.0, 200.0, 87.0, 14.0, "Verify matched hash", sums.is_some()) {
                    expect.clone_from(hash);
                }
            }
            Some(Err(error)) => d.text(7.0, 167.0, 286.0, 27.0, error),
            None if !idle => d.text(7.0, 167.0, 286.0, 19.0, "Looking up checksum..."),
            None => {}
        }
        !d.button(243.0, 200.0, 50.0, 14.0, "Close", true)
    }
}
