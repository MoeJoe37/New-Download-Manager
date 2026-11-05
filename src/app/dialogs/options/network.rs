use crate::app::dialogs::Dialog;
use crate::app::{core, dlu, egui, tr, win, App, RichText, Settings, Vec2, PAGE_DX};

impl App {
    pub(super) fn options_site_logins(
        &mut self,
        ui: &mut egui::Ui,
        s: &mut Settings,
        origin: (f32, f32),
    ) {
        // Site Logins (template 145): credentials applied to matching download addresses.
        let icon = self.res.icon(272);
        let mut d2 = dlu::D::new_offset(ui, origin.0, origin.1).with_dx(PAGE_DX);
        d2.page_header(icon, 42.0, "User names and passwords for servers/sites");
        let list = d2.listview(
            7.0,
            28.0,
            255.0,
            172.0,
            &[
                ("Server/path", 110.0),
                ("User name", 75.0),
                ("Password", 70.0),
            ],
        );
        let sel_id = egui::Id::new("login_sel");
        let mut sel: Option<usize> = d2.ui.data(|m| m.get_temp(sel_id)).flatten();
        d2.rows(
            list.shrink2(Vec2::new(2.0, 0.0))
                .with_min_y(list.min.y + 18.0),
            |ui| {
                for (k, l) in s.logins.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        let w = |n: f32| n * PAGE_DX - 6.0;
                        let a = ui.add(
                            egui::TextEdit::singleline(&mut l.site)
                                .desired_width(w(110.0))
                                .hint_text("example.com"),
                        );
                        let b =
                            ui.add(egui::TextEdit::singleline(&mut l.user).desired_width(w(75.0)));
                        let c = ui.add(
                            egui::TextEdit::singleline(&mut l.pass)
                                .password(true)
                                .desired_width(w(70.0)),
                        );
                        if a.gained_focus() || b.gained_focus() || c.gained_focus() {
                            sel = Some(k);
                        }
                        if ui.ctx().memory(|m| m.has_focus(a.id))
                            && ui
                                .data(|m| m.get_temp::<bool>(sel_id.with("edit")))
                                .unwrap_or(false)
                        {
                            ui.data_mut(|m| m.remove::<bool>(sel_id.with("edit")));
                        }
                        if sel == Some(k)
                            && ui
                                .data(|m| m.get_temp::<bool>(sel_id.with("edit")))
                                .unwrap_or(false)
                        {
                            a.request_focus();
                        }
                    });
                }
            },
        );
        if d2.button(13.0, 205.0, 50.0, 14.0, "New", true) {
            s.logins.push(Default::default());
            sel = Some(s.logins.len() - 1);
            d2.ui.data_mut(|m| m.insert_temp(sel_id.with("edit"), true));
        }
        let has = sel.is_some_and(|k| k < s.logins.len());
        if d2.button(76.0, 205.0, 50.0, 14.0, "Edit", has) {
            d2.ui.data_mut(|m| m.insert_temp(sel_id.with("edit"), true));
        }
        if d2.button(139.0, 205.0, 50.0, 14.0, "Remove", has) {
            s.logins.remove(sel.unwrap());
            sel = None;
        }
        d2.ui.data_mut(|m| m.insert_temp(sel_id, sel));
    }

    pub(super) fn options_dial_up(
        &mut self,
        ui: &mut egui::Ui,
        s: &mut Settings,
        origin: (f32, f32),
    ) {
        // Dial Up / VPN (template 185, COptDialUp): Windows' connections; Apply stores the user
        // name (and with "Save password" the password) in Windows, as NDM does (RasSetEntryDialParams).
        let icon = self.res.icon(275);
        let mut d2 = dlu::D::new_offset(ui, origin.0, origin.1).with_dx(PAGE_DX);
        d2.page_header(icon, 47.0, "Dial up / VPN settings");
        d2.check(
            8.0,
            30.0,
            247.0,
            10.0,
            &mut s.dial_on,
            "Use Windows Dial Up / VPN Networking",
        );
        let on = s.dial_on;
        d2.group(7.0, 44.0, 254.0, 110.0, "Connection options");
        for (y, t) in [
            (62.0, "Connection:"),
            (84.0, "User name:"),
            (106.0, "Password:"),
        ] {
            d2.label_r(
                16.0,
                y,
                59.0,
                10.0,
                if on {
                    RichText::new(tr(t))
                } else {
                    RichText::new(tr(t)).weak()
                },
            );
        }
        let id = egui::Id::new("dialup_form");
        let (mut user, mut pass, mut save) = d2
            .ui
            .data_mut(|m| m.get_temp::<(String, String, bool)>(id))
            .unwrap_or_default();
        let entries = win::ras_entries();
        if s.dial_entry.is_empty() {
            s.dial_entry = entries.first().cloned().unwrap_or_default();
        }
        let list: Vec<&str> = entries.iter().map(String::as_str).collect();
        d2.combo(
            80.0,
            59.0,
            156.0,
            14.0,
            "dialup",
            &mut s.dial_entry,
            &list,
            on,
        );
        d2.edit(80.0, 82.0, 117.0, 14.0, &mut user, on);
        d2.password(80.0, 103.0, 117.0, 14.0, &mut pass, on);
        d2.check(80.0, 123.0, 115.0, 10.0, &mut save, "Save password");
        if d2.button(
            201.0,
            134.0,
            50.0,
            14.0,
            "Apply",
            on && !s.dial_entry.is_empty(),
        ) {
            if let Err(e) = win::ras_save(&s.dial_entry, &user, &pass, save) {
                self.open(Dialog::Info {
                    title: "Dial Up / VPN".into(),
                    text: e,
                });
            }
        }
        d2.ui.data_mut(|m| m.insert_temp(id, (user, pass, save)));
        d2.group(7.0, 158.0, 254.0, 54.0, "Redial options");
        let weak = |t: &str| {
            if on {
                RichText::new(tr(t))
            } else {
                RichText::new(tr(t)).weak()
            }
        };
        d2.label_r(
            13.0,
            172.0,
            132.0,
            10.0,
            weak("Redial attempts (zero if endlessly):"),
        );
        d2.drag(151.0, 169.0, 28.0, 14.0, &mut s.dial_redial, 0, 999);
        d2.label(184.0, 172.0, 68.0, 10.0, weak("times"));
        d2.label_r(
            14.0,
            192.0,
            131.0,
            10.0,
            weak("Time between redial attempts:"),
        );
        d2.drag(151.0, 189.0, 28.0, 14.0, &mut s.dial_wait, 1, 3600);
        d2.label(184.0, 192.0, 69.0, 10.0, weak("seconds"));
    }

    pub(super) fn options_proxy(
        ui: &mut egui::Ui,
        s: &mut Settings,
        origin: (f32, f32),
        icon: Option<egui::TextureId>,
        adv: &mut Option<core::routing::AdvProxy>,
    ) {
        let mut d2 = dlu::D::new_offset(ui, origin.0, origin.1).with_dx(PAGE_DX);
        d2.page_header(icon, 36.0, "Proxy / socks configuration");
        // below the page, off the header (whose title ends at 255 DLU)
        if d2.button(197.0, 241.0, 63.0, 14.0, "Get System", true) {
            // Internet Options' proxy into the manual fields ("h:p" or "http=h:p;https=h:p;...")
            let (_, server, bypass) = win::system_proxy();
            let hp = server
                .split(';')
                .find_map(|p| p.strip_prefix("http="))
                .or_else(|| Some(server.as_str()).filter(|x| !x.contains('=')))
                .unwrap_or("");
            let (h, p) = hp.rsplit_once(':').unwrap_or((hp, ""));
            (s.proxy_host, s.proxy_port, s.proxy_mode) =
                (h.trim().to_string(), p.trim().to_string(), 3);
            (s.proxy_bypass_on, s.proxy_bypass) = (
                !bypass.is_empty(),
                bypass
                    .replace(';', " ")
                    .replace("<local>", "")
                    .trim()
                    .to_string(),
            );
        }
        d2.check(13.0, 27.0, 243.0, 19.0, &mut s.proxy_browser, "Use proxy/socks from a browser's request if there are download errors for the downloads intercepted from the browser");
        d2.hline(8.0, 50.0, 254.0);
        if d2.radio(7.0, 55.0, 248.0, 10.0, s.proxy_mode == 0, "No proxy/socks") {
            s.proxy_mode = 0;
        }
        d2.hline(8.0, 69.0, 254.0);
        if d2.radio(
            7.0,
            73.0,
            249.0,
            10.0,
            s.proxy_mode == 1,
            "Use system settings",
        ) {
            s.proxy_mode = 1;
        }
        d2.hline(7.0, 86.0, 254.0);
        if d2.radio(
            7.0,
            90.0,
            234.0,
            10.0,
            s.proxy_mode == 2,
            "Use automatic configuration script",
        ) {
            s.proxy_mode = 2;
        }
        d2.label(13.0, 105.0, 35.0, 9.0, "Address");
        d2.edit(53.0, 102.0, 203.0, 14.0, &mut s.pac_url, s.proxy_mode == 2);
        d2.hline(7.0, 120.0, 254.0);
        if d2.radio(
            8.0,
            124.0,
            229.0,
            10.0,
            s.proxy_mode == 3,
            "Manual proxy/socks configuration",
        ) {
            s.proxy_mode = 3;
        }
        let m = s.proxy_mode == 3;
        d2.label(13.0, 136.0, 84.0, 10.0, "Proxy server address");
        d2.label(108.0, 136.0, 34.0, 10.0, "Port");
        d2.label(146.0, 136.0, 52.0, 10.0, "UserName");
        d2.label(202.0, 136.0, 54.0, 10.0, "Password");
        d2.edit(13.0, 147.0, 91.0, 15.0, &mut s.proxy_host, m);
        d2.edit(108.0, 147.0, 34.0, 15.0, &mut s.proxy_port, m);
        d2.edit(146.0, 147.0, 52.0, 15.0, &mut s.proxy_user, m);
        d2.password(201.0, 147.0, 55.0, 15.0, &mut s.proxy_pass, m);
        d2.label(
            13.0,
            164.0,
            235.0,
            10.0,
            "Use this proxy for the following protocols:",
        );
        d2.at(13.0, 176.0, 28.0, 10.0, |ui| {
            ui.add_enabled(m, dlu::checkbox(&mut s.proxy_http, "http"))
        });
        d2.at(58.0, 176.0, 31.0, 10.0, |ui| {
            ui.add_enabled(m, dlu::checkbox(&mut s.proxy_https, "https"))
        });
        d2.at(109.0, 176.0, 24.0, 10.0, |ui| {
            ui.add_enabled(m, dlu::checkbox(&mut s.proxy_ftp, "ftp"))
        });
        if d2.button(149.0, 179.0, 107.0, 14.0, "Advanced...", m) {
            *adv = Some(s.adv.clone());
        }
        d2.group(13.0, 197.0, 248.0, 41.0, "");
        d2.at(18.0, 194.0, 238.0, 12.0, |ui| {
            ui.add_enabled(
                m,
                dlu::checkbox(
                    &mut s.proxy_bypass_on,
                    "Do not use proxy server for addresses beginning with:",
                ),
            )
        });
        d2.edit(
            22.0,
            208.0,
            234.0,
            17.0,
            &mut s.proxy_bypass,
            m && s.proxy_bypass_on,
        );
        d2.label(20.0, 227.0, 228.0, 10.0, "Separate names by spaces");
        d2.hline(7.0, 241.0, 254.0);
        d2.check(
            11.0,
            244.0,
            180.0,
            10.0,
            &mut s.ftp_pasv,
            "Use FTP in PASV mode",
        );
    }
}
