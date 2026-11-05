use crate::app::dialogs::Dialog;
use crate::app::{core, dlu, egui, sched_time, tr, App, Vec2};

impl App {
    pub(super) fn dialog_grab_sched_ask(
        &mut self,
        ui: &mut egui::Ui,
        proj: &mut Box<core::settings::GrabProject>,
    ) -> bool {
        let mut d = dlu::D::new(ui);
        d.text(
            7.0,
            7.0,
            246.0,
            20.0,
            "To use project scheduler you should save the project at first. Save the project now?",
        );
        let yes = d.button(140.0, 40.0, 50.0, 14.0, "Yes", true);
        if yes {
            let p = (**proj).clone();
            let name = p.opts.name.trim().to_string();
            let quiet = self.core.settings().grab_quiet;
            self.open(Dialog::GrabSave {
                name,
                orig: None,
                quiet,
                proj: Some(Box::new(p)),
                sched: true,
            });
        }
        !(yes || d.button(203.0, 40.0, 50.0, 14.0, "No", true))
    }

    pub(super) fn dialog_grab_sched(
        &mut self,
        ui: &mut egui::Ui,
        p: &mut Box<core::settings::GrabProject>,
    ) -> bool {
        let mut d = dlu::D::new(ui);
        if let Some(t) = self.res.icon(285) {
            let r = d.rect(7.0, 7.0, 20.0, 20.0);
            d.ui.put(r, egui::Image::new((t, Vec2::splat(32.0))));
        }
        d.label(41.0, 6.0, 124.0, 9.0, "Project:");
        d.label(42.0, 18.0, 127.0, 9.0, p.opts.name.clone());
        d.group(7.0, 32.0, 190.0, 43.0, "Step 1. Select type");
        for (y, v, t) in [
            (45.0, false, "One-time exploring/downloading"),
            (58.0, true, "Periodic synchronization"),
        ] {
            if d.radio(11.0, y, 182.0, 10.0, p.periodic == v, t) {
                p.periodic = v;
            }
        }
        d.group(7.0, 82.0, 191.0, 58.0, "Step 2. Select action");
        for (y, v, t) in [
            (97.0, 0, "Explore only"),
            (111.0, 1, "Explore site and download matched files"),
            (123.0, 2, "Download checked files"),
        ] {
            if d.radio(11.0, y, 179.0, 10.0, p.action == v, t) {
                p.action = v;
            }
        }
        d.group(207.0, 7.0, 223.0, 133.0, "Step 3. Schedule");
        let sc = &mut p.sched;
        d.check(
            217.0,
            19.0,
            95.0,
            10.0,
            &mut sc.start_on,
            "Start download at",
        );
        d.edit(318.0, 17.0, 59.0, 15.0, &mut sc.start_at, sc.start_on);
        if p.periodic {
            d.check(
                217.0,
                36.0,
                90.0,
                10.0,
                &mut sc.again_on,
                "Start again every",
            );
            let (mut h, mut m) = (sc.again_h.to_string(), sc.again_m.to_string());
            if d.edit(310.0, 35.0, 19.0, 14.0, &mut h, sc.again_on)
                .changed()
            {
                sc.again_h = h.trim().parse().unwrap_or(0);
            }
            d.label(332.0, 38.0, 25.0, 8.0, "hours");
            if d.edit(362.0, 35.0, 19.0, 14.0, &mut m, sc.again_on)
                .changed()
            {
                sc.again_m = m.trim().parse().unwrap_or(0);
            }
            d.label(383.0, 38.0, 23.0, 8.0, "min");
        } else {
            if d.radio(225.0, 36.0, 54.0, 10.0, !sc.daily, "Once at") {
                sc.daily = false;
            }
            if d.radio(225.0, 52.0, 45.0, 10.0, sc.daily, "Daily") {
                sc.daily = true;
            }
            let days = [
                "Sunday",
                "Monday",
                "Tuesday",
                "Wednesday",
                "Thursday",
                "Friday",
                "Saturday",
            ];
            for (k, day) in days.iter().enumerate() {
                let (x, y) = if k < 4 {
                    (275.0, [52.0, 63.0, 73.0, 83.0][k])
                } else {
                    (353.0, [52.0, 63.0, 73.0][k - 4])
                };
                let daily = sc.daily;
                d.at(x, y, 72.0, 10.0, |ui| {
                    ui.add_enabled(daily, dlu::checkbox(&mut sc.days[k], day))
                });
            }
        }
        d.hline(217.0, 94.0, 197.0);
        d.check(
            217.0,
            100.0,
            97.0,
            10.0,
            &mut sc.stop_on,
            "Stop download at",
        );
        d.edit(318.0, 97.0, 59.0, 14.0, &mut sc.stop_at, sc.stop_on);
        d.hline(218.0, 114.0, 197.0);
        d.text(212.0, 118.0, 214.0, 18.0, "Note: NDM should be running in system tray at the specified time to start a scheduled project");
        d.check(
            9.0,
            145.0,
            185.0,
            10.0,
            &mut sc.hangup,
            "Hang up modem when done",
        );
        d.check(
            9.0,
            158.0,
            191.0,
            10.0,
            &mut sc.exit_done,
            "Exit New Download Manager when done",
        );
        d.check(
            9.0,
            170.0,
            188.0,
            10.0,
            &mut sc.off_done,
            "Turn off computer when done",
        );
        let kinds = ["Shut down", "Sleep", "Hibernate"].map(tr);
        let mut kind = kinds[sc.off_kind as usize % 3].clone();
        if d.combo(
            23.0,
            182.0,
            104.0,
            14.0,
            "goff",
            &mut kind,
            &kinds.each_ref().map(String::as_str),
            sc.off_done,
        ) {
            sc.off_kind = kinds.iter().position(|k| *k == kind).unwrap_or(0) as u8;
        }
        let off = sc.off_done;
        d.at(23.0, 200.0, 158.0, 10.0, |ui| {
            ui.add_enabled(
                off,
                dlu::checkbox(&mut sc.off_force, "Force processes to terminate"),
            )
        });
        d.check(
            212.0,
            146.0,
            213.0,
            10.0,
            &mut sc.open_on,
            "Open the following file when done:",
        );
        d.edit(211.0, 158.0, 198.0, 14.0, &mut sc.open_file, sc.open_on);
        if d.button(412.0, 158.0, 15.0, 14.0, "...", sc.open_on) {
            if let Some(f) = rfd::FileDialog::new().pick_file() {
                sc.open_file = f.display().to_string();
            }
        }
        let t = |s: &str| sched_time(s).map(|t| t.format("%H:%M").to_string());
        let times = (t(&sc.start_at), t(&sc.stop_at));
        let ok = d.button(
            296.0,
            198.0,
            50.0,
            14.0,
            "OK",
            times.0.is_some() && times.1.is_some(),
        );
        if ok {
            (sc.start_at, sc.stop_at) = (times.0.unwrap(), times.1.unwrap());
            let mut p = (**p).clone();
            p.opts.overwrite |= p.periodic; // grabber.chm "Scheduler": synchronization replaces changed files
            let periodic_now = p.periodic && p.sched.again_on && !p.sched.start_on;
            if let Some(x) = self
                .core
                .settings()
                .grab_projects
                .iter_mut()
                .find(|x| x.opts.name == p.opts.name)
            {
                *x = p.clone();
            }
            self.core.save();
            if periodic_now {
                self.core.run_project(p); // no start time: the first pass now, then every N hours
            }
        }
        !(ok || d.button(357.0, 198.0, 50.0, 14.0, "Cancel", true))
    }
}
