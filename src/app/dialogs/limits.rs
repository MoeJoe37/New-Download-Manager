use super::*;

impl App {
    pub(super) fn dialog_http1_hosts(&mut self, ui: &mut egui::Ui, hosts: &mut String) -> bool {
        let mut d = dlu::D::new(ui);
        d.text(7.0, 7.0, 256.0, 23.0, "Use HTTP/1.1 for direct HTTPS downloads from these exact hosts. Separate hosts with spaces.");
        d.edit(7.0, 32.0, 256.0, 14.0, hosts, true);
        let valid = hosts.split_whitespace().all(|host| url::Host::parse(host).is_ok());
        if d.button(155.0, 53.0, 50.0, 14.0, "OK", valid) {
            let hosts = hosts.split_whitespace().collect::<Vec<_>>().join(" ");
            self.opt_patch.push(Box::new(move |settings| settings.http1_hosts.clone_from(&hosts)));
            return false;
        }
        !d.button(213.0, 53.0, 50.0, 14.0, "Cancel", true)
    }

    pub(super) fn dialog_networks(&mut self, ui: &mut egui::Ui, nics: &mut [net::Nic], off: &mut Vec<String>) -> bool {
        let mut d = dlu::D::new(ui);
        d.text(7.0, 5.0, 226.0, 26.0, "NDM spreads each download's connections over the ticked networks. Two networks on the same router add no speed.");
        if nics.is_empty() {
            d.label(15.0, 34.0, 210.0, 10.0, "No network with an internet connection was found");
        }
        for (k, n) in nics.iter().enumerate() {
            let mut on = !off.contains(&n.name);
            let ips = n.ips.iter().map(|ip| ip.to_string()).collect::<Vec<_>>().join(", ");
            d.at(15.0, 34.0 + 12.0 * k as f32, 210.0, 10.0, |ui| ui.add(dlu::checkbox(&mut on, &format!("{}   {ips}", n.name))));
            if on {
                off.retain(|o| *o != n.name);
            } else if !off.contains(&n.name) {
                off.push(n.name.clone());
            }
        }
        let y = 44.0 + 12.0 * nics.len().max(1) as f32;
        let ok = d.button(64.0, y, 50.0, 14.0, "OK", true);
        if ok {
            let off = off.clone();
            self.opt_patch.push(Box::new(move |s: &mut Settings| s.nets_off = off));
        }
        !(ok || d.button(126.0, y, 50.0, 14.0, "Cancel", true))
    }

    pub(super) fn dialog_server_conn(&mut self, ui: &mut egui::Ui, orig: &mut Option<usize>, server: &mut String, n: &mut String) -> bool {
        let mut d = dlu::D::new(ui);
        d.label(50.0, 3.0, 150.0, 9.0, "Server");
        let mut scheme = "http://".to_string();
        d.combo(7.0, 15.0, 40.0, 13.0, "sc_scheme", &mut scheme, &["http://"], false);
        d.edit(50.0, 15.0, 164.0, 13.0, server, true);
        d.label(50.0, 29.0, 164.0, 9.0, "You may use asterisk as a wildcard pattern");
        d.label_r(33.0, 49.0, 128.0, 9.0, "Max. connections number");
        d.combo(166.0, 47.0, 48.0, 14.0, "sc_n", n, &["1", "2", "4", "8", "16", "24", "32"], true);
        let host = server.trim().trim_start_matches("http://").trim_start_matches("https://").trim_start_matches("ftp://").trim_end_matches('/').to_string();
        let ok = d.button(53.0, 68.0, 50.0, 14.0, "OK", !host.is_empty());
        if ok {
            let (orig, v) = (*orig, (host, n.parse().unwrap_or(8)));
            self.opt_patch.push(Box::new(move |s: &mut Settings| match orig {
                Some(k) if k < s.server_conns.len() => s.server_conns[k] = v,
                _ => s.server_conns.push(v),
            }));
        }
        !(ok || d.button(119.0, 68.0, 50.0, 14.0, "Cancel", true))
    }

    pub(super) fn dialog_limiter(&mut self, ui: &mut egui::Ui, kb: &mut String, startup: &mut bool, sched: &mut bool, from: &mut String, to: &mut String) -> bool {
        let mut d = dlu::D::new(ui);
        d.label_r(7.0, 7.0, 170.0, 9.0, "Maximum download speed for one file");
        d.edit(183.0, 4.0, 33.0, 14.0, kb, true);
        d.label(224.0, 7.0, 65.0, 9.0, "KBytes/sec");
        d.text(7.0, 23.0, 288.0, 26.0, "Note: Internet servers may break connection when the speed is too limited! Thus it's not recommended to use Speed Limiter to download from servers that do not support \"resume\" feature.");
        d.check(7.0, 52.0, 288.0, 10.0, startup, "Always turn on Speed Limiter on New Download Manager startup");
        // its hours: on from .. to, off the rest of the day
        d.check(7.0, 69.0, 170.0, 10.0, sched, "Turn on Speed Limiter only between");
        d.edit(181.0, 67.0, 30.0, 14.0, from, *sched);
        d.label(214.0, 69.0, 18.0, 9.0, "and");
        d.edit(234.0, 67.0, 30.0, 14.0, to, *sched);
        let v = kb.trim().parse::<u64>().ok().filter(|v| *v > 0);
        let times = !*sched || [&*from, &*to].iter().all(|t| sched_time(t).is_some());
        let ok = d.button(189.0, 86.0, 50.0, 14.0, "OK", v.is_some() && times);
        if ok {
            let hm = |t: &str| sched_time(t).map_or_else(|| t.to_string(), |t| t.format("%H:%M").to_string());
            let mut s = self.core.settings();
            (s.limit_kb, s.limit_startup, s.limit_sched, s.limit_from, s.limit_to) = (v.unwrap(), *startup, *sched, hm(from), hm(to));
            drop(s);
            self.core.apply_settings();
            self.core.save();
        }
        !(ok || d.button(245.0, 86.0, 50.0, 14.0, "Cancel", true))
    }

    pub(super) fn dialog_speed_limit(&mut self, ui: &mut egui::Ui, ids: &[u64], on: &mut bool, kb: &mut String) -> bool {
        let mut d = dlu::D::new(ui);
        d.check(7.0, 8.0, 120.0, 10.0, on, "Limit the speed of this download to");
        d.edit(130.0, 6.0, 40.0, 14.0, kb, *on);
        d.label(174.0, 8.0, 50.0, 9.0, "KBytes/sec");
        let v = kb.trim().parse::<u64>().ok().filter(|v| (1..=1_000_000).contains(v));
        let ok = d.button(118.0, 27.0, 50.0, 14.0, "OK", !*on || v.is_some());
        if ok {
            for j in ids.iter().filter_map(|id| self.core.find(*id)) {
                self.core.set_job_limit(&j, if *on { v.unwrap_or(0) } else { 0 });
            }
            self.core.save();
        }
        !(ok || d.button(173.0, 27.0, 50.0, 14.0, "Cancel", true))
    }
}
