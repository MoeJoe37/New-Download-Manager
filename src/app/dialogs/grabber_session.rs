use crate::app::dialogs::Dialog;
use crate::app::{core, dlu, egui, i18n, tr, tr_os, App, Arc, HashSet, Status, Vec2};

impl App {
    pub(super) fn dialog_grab_login(
        &mut self,
        ui: &mut egui::Ui,
        realm: &str,
        site: &str,
        user: &mut String,
        pass: &mut String,
        save: &mut bool,
    ) -> bool {
        let mut d = dlu::D::new(ui);
        if let Some(t) = self.res.icon(272) {
            let r = d.rect(10.0, 7.0, 20.0, 20.0);
            d.ui.put(r, egui::Image::new((t, Vec2::splat(32.0))));
        }
        d.label(40.0, 9.0, 223.0, 10.0, "Type your user name and password.");
        d.label(
            40.0,
            28.0,
            225.0,
            10.0,
            i18n::line(&format!("{} {}", tr_os("Realm:"), i18n::ltr(realm))),
        );
        d.label(40.0, 48.0, 73.0, 10.0, "User name:");
        d.edit(117.0, 46.0, 146.0, 14.0, user, true);
        d.label(40.0, 69.0, 72.0, 10.0, "Password:");
        d.password(117.0, 66.0, 146.0, 14.0, pass, true);
        d.check(
            40.0,
            93.0,
            216.0,
            10.0,
            save,
            "Save this password in NDM password list",
        );
        let ok = d.button(155.0, 117.0, 50.0, 14.0, "OK", !user.is_empty());
        if ok {
            if *save {
                self.core.settings().logins.push(core::settings::Login {
                    site: site.to_owned(),
                    user: user.clone(),
                    pass: pass.clone(),
                });
                self.core.save();
            }
            self.grab_auth = Some((user.clone(), pass.clone()));
        }
        !(ok || d.button(212.0, 117.0, 50.0, 14.0, "Cancel", true))
    }

    pub(super) fn dialog_grab_settings(
        &mut self,
        ui: &mut egui::Ui,
        explore: &mut String,
        dl: &mut String,
        link: &mut bool,
        iec: &mut bool,
        adv: &mut bool,
    ) -> bool {
        // template 262, checked (explore 1-20, download 1-10)
        let mut d = dlu::D::new(ui);
        d.edit(7.0, 6.0, 21.0, 14.0, explore, true);
        d.label(
            33.0,
            10.0,
            239.0,
            10.0,
            "files to explore at the same time (1 to 10)",
        );
        d.edit(7.0, 25.0, 22.0, 14.0, dl, true);
        d.label(
            33.0,
            28.0,
            239.0,
            8.0,
            "files to download at the same time (1 to 10)",
        );
        d.text(7.0, 43.0, 278.0, 18.0, "Please note that a web server may reject requests if you set a large number of files to explore (download) at the same time.");
        d.hline(7.0, 66.0, 276.0);
        d.check(
            7.0,
            75.0,
            278.0,
            10.0,
            iec,
            "Look up files in IE cache before downloading",
        );
        d.check(
            7.0,
            89.0,
            278.0,
            10.0,
            link,
            "Use link texts as download descriptions when adding files to NDM main list",
        );
        d.check(
            7.0,
            103.0,
            278.0,
            8.0,
            adv,
            "Use Advanced web page processing",
        );
        let ok = d.button(164.0, 118.0, 50.0, 14.0, "OK", true);
        if ok {
            let (e, n) = (
                explore
                    .trim()
                    .parse::<usize>()
                    .ok()
                    .filter(|v| (1..=20).contains(v)),
                dl.trim()
                    .parse::<usize>()
                    .ok()
                    .filter(|v| (1..=10).contains(v)),
            );
            let bad = if e.is_none() {
                Some("Invalid number of files explored at the same time. Please set a correct number.")
            } else if n.is_none() {
                Some("Invalid number of files downloaded at the same time. Please set a correct number.")
            } else {
                None
            };
            if let Some(text) = bad {
                self.open(Dialog::Info {
                    title: "Settings of NDM Grabber".into(),
                    text: text.into(),
                });
                return true;
            }
            let mut s = self.core.settings();
            (
                s.grab_explore,
                s.grab_dl,
                s.grab_link_text,
                s.grab_ie_cache,
                s.grab_adv,
            ) = (e.unwrap(), n.unwrap(), *link, *iec, *adv);
            drop(s);
            self.core.save();
        }
        !(ok || d.button(235.0, 118.0, 50.0, 14.0, "Cancel", true))
    }

    pub(super) fn dialog_grab_manual(&mut self, ui: &mut egui::Ui, url: &str) -> bool {
        let mut d = dlu::D::new(ui);
        d.label(
            7.0,
            7.0,
            246.0,
            9.0,
            "Enter login and password manually at the following web page:",
        );
        d.label(7.0, 20.0, 246.0, 9.0, url.to_owned());
        let ok = d.button(140.0, 49.0, 50.0, 14.0, "OK", true);
        let cancel = d.button(203.0, 49.0, 50.0, 14.0, "Cancel", true);
        if ok || cancel {
            *self.core.login_wait.lock().unwrap() = None;
            let ck = std::mem::take(&mut *self.core.login_cookies.lock().unwrap());
            if ok {
                self.grab_cookies = Some(ck);
            }
        }
        !(ok || cancel)
    }

    pub(super) fn dialog_grab_stats(
        &mut self,
        ui: &mut egui::Ui,
        crawl: &mut Arc<crate::grabber::Crawl>,
    ) -> bool {
        // template 271
        let st = crawl.state.lock().unwrap();
        let (done, pages, total, explored) = (st.done, st.pages, st.files.len(), st.explored);
        let urls: HashSet<String> = st.files.iter().map(|f| f.url.clone()).collect();
        drop(st);
        let downloaded = self
            .core
            .jobs()
            .iter()
            .filter(|j| {
                let i = j.info();
                i.status == Status::Complete && urls.contains(&i.url)
            })
            .count();
        let mut d = dlu::D::new(ui);
        d.label_r(3.0, 3.0, 36.0, 9.0, "Status:");
        d.label(
            41.0,
            3.0,
            63.0,
            9.0,
            if done {
                tr("Complete")
            } else {
                tr("Exploring site")
            },
        );
        d.hline(5.0, 14.0, 101.0);
        d.label(7.0, 17.0, 97.0, 9.0, "Web pages processed");
        d.label_r(7.0, 28.0, 51.0, 9.0, "Simple");
        d.label(65.0, 29.0, 39.0, 9.0, pages.to_string());
        d.label_r(7.0, 37.0, 51.0, 9.0, "Advanced");
        d.label(65.0, 38.0, 39.0, 9.0, "0");
        d.hline(5.0, 49.0, 101.0);
        d.label(7.0, 52.0, 97.0, 9.0, "Files");
        for (y, k, v) in [
            (63.0, "Total", total),
            (72.0, "Explored", explored),
            (81.0, "Matched", total),
            (90.0, "Downloaded", downloaded),
        ] {
            d.label_r(7.0, y, 54.0, 9.0, k);
            d.label(65.0, y, 38.0, 9.0, v.to_string());
        }
        true
    }
}
