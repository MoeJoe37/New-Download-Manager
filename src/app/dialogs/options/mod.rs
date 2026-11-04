mod pages;

use super::*;

impl App {
    pub(super) fn dialog_options(&mut self, ctx: &egui::Context, ui: &mut egui::Ui, tab: &mut u8, s: &mut Box<Settings>) -> bool {
        // CPropertySheet: tab control at (7,7) holding 268x257 DLU pages, OK/Cancel/Help below.
        let res = (self.res.icon(277), self.res.icon(295), self.res.icon(276), self.res.icon(293), self.res.icon(273), self.res.icon(278));
        let ext_dir = extension_dir();
        let mut d = dlu::D::new(ui);
        let names = ["General", "File types", "Save to", "Downloads", "Connection", "", "Proxy / Socks", "Sites Logins", "Dial Up / VPN", "Sounds"];
        if let Some(mut a) = self.adv_done.take() {
            if let Some((on, h, p, u, pw)) = a.http.take() {
                (s.proxy_http, s.proxy_host, s.proxy_port, s.proxy_user, s.proxy_pass) = (on, h, p, u, pw);
                if on {
                    s.proxy_mode = 3;
                }
            }
            s.adv = a;
        }
        for f in self.opt_patch.drain(..) {
            f(s);
        }
        let mut adv = None;
        let mut open = None;
        let mut new_cat: Option<Option<String>> = None;
        let mut integrate: Vec<core::settings::Browser> = Vec::new();
        // measured on NDM (125%): 570 px wide sheet, tab rows General..Connection / Proxy..Sounds,
        // pages laid out at PAGE_DX
        d.tabs(4.6, 5.5, 316.0, 280.5, tab, &names);
        let origin = (6.3, 30.5);
        match *tab {
            0 => self.options_general(d.ui, s, origin, ctx, res.0, &mut open, &mut integrate, ext_dir.is_some()),
            1 => Self::options_file_types(d.ui, s, origin, res.1, &mut open),
            2 => self.options_save_to(d.ui, s, origin, res.2, &mut new_cat),
            3 => Self::options_downloads(d.ui, s, origin, res.3, &mut open),
            4 => self.options_connection(d.ui, s, origin, res.4, &mut open),
            6 => self.options_site_logins(d.ui, s, origin),
            7 => self.options_dial_up(d.ui, s, origin),
            8 => self.options_sounds(d.ui, s, origin),
            _ => Self::options_proxy(d.ui, s, origin, res.5, &mut adv),
        }
        if let Some(mut adv) = adv {
            adv.http = Some((s.proxy_mode == 3 && s.proxy_http, s.proxy_host.clone(), s.proxy_port.clone(), s.proxy_user.clone(), s.proxy_pass.clone()));
            self.open(Dialog::AdvProxy { adv });
        }
        if let Some(o) = open {
            self.open(o);
        }
        if let Some(c) = new_cat {
            self.edit_category(c);
        }
        for b in integrate {
            self.open(Dialog::Integrate { b });
        }
        // "Add version..." in Browser details
        for b in self.browser_add.drain(..) {
            s.browsers.push(b);
        }
        // Sheet buttons
        // NDM's sheet has OK and Help only; closing it (x) cancels
        let ok = d.button(203.0, 291.5, 56.0, 13.0, "OK", true);
        if d.button(265.0, 291.5, 56.0, 13.0, "Help", help("index.htm", false)) {
            help("options.htm", true);
        }
        let cancel = false;
        if ok {
            let autostart_changed = self.core.settings().autostart != s.autostart;
            let dark_changed = self.core.settings().dark_support != s.dark_support;
            *self.core.settings() = (**s).clone();
            self.core.apply_settings();
            #[cfg(not(target_os = "macos"))]
            self.core.apply_integration(s.adv_integration);
            self.core.save();
            if autostart_changed {
                win::set_autostart(s.autostart);
            }
            if dark_changed {
                self.theme_check = u32::MAX;
            }
        }
        !(ok || cancel)
    }
}
