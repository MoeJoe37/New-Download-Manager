use crate::app::{links, tr_os, App, Info};

/// One entry of an NDM export file.
#[derive(Debug, PartialEq)]
struct Ef2 {
    url: String,
    referrer: String,
    cookies: String,
}

/// NDM export file (.ef2, CUrlExporter): "<", address, "referer: ", "cookie: ", "User-Agent: " lines, ">".
fn ef2_write(items: &[Info], ua: &str) -> String {
    let mut s = String::new();
    for i in items {
        s += &format!("<\r\n{}\r\n", i.url);
        for (k, v) in [
            ("referer", &i.referrer),
            ("cookie", &i.cookies),
            ("User-Agent", &ua.to_string()),
        ] {
            if !v.is_empty() {
                s += &format!("{k}: {v}\r\n");
            }
        }
        s += ">\r\n";
    }
    s
}

fn ef2_read(text: &str) -> Vec<Ef2> {
    let mut out = Vec::new();
    for block in text.split('<').skip(1) {
        let mut lines = block
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && *l != ">");
        let Some(url) = lines.next().filter(|u| u.contains("://")) else {
            continue;
        };
        let mut e = Ef2 {
            url: url.to_string(),
            referrer: String::new(),
            cookies: String::new(),
        };
        for l in lines {
            if let Some((k, v)) = l.split_once(": ") {
                match k.to_ascii_lowercase().as_str() {
                    "referer" => e.referrer = v.to_string(),
                    "cookie" => e.cookies = v.to_string(),
                    _ => {}
                }
            }
        }
        out.push(e);
    }
    out
}

#[test]
fn ef2_round_trip() {
    let i = Info {
        url: "https://a.com/f.zip".into(),
        referrer: "https://a.com/".into(),
        cookies: "k=v".into(),
        ..Default::default()
    };
    let text = ef2_write(
        &[
            i,
            Info {
                url: "http://b.org/x".into(),
                ..Default::default()
            },
        ],
        "UA",
    );
    let back = ef2_read(&text);
    assert_eq!(back.len(), 2);
    assert_eq!(
        back[0],
        Ef2 {
            url: "https://a.com/f.zip".into(),
            referrer: "https://a.com/".into(),
            cookies: "k=v".into()
        }
    );
    assert_eq!(back[1].url, "http://b.org/x");
}

impl App {
    /// File -> Import: a text file of links, or an NDM export file (.ef2) with referer and cookies.
    pub(in crate::app) fn import(&self, ef2: bool) {
        let (name, ext) = if ef2 {
            ("NDM export files", "ef2")
        } else {
            ("Text Files", "txt")
        };
        let Some(p) = rfd::FileDialog::new()
            .add_filter(tr_os(name), &[ext])
            .pick_file()
        else {
            return;
        };
        let text = std::fs::read_to_string(p).unwrap_or_default();
        if !ef2 {
            self.add_queued(links(&text));
            return;
        }
        for e in ef2_read(&text) {
            let mut i = self.core.new_info(&e.url);
            (i.referrer, i.cookies, i.queued) = (e.referrer, e.cookies, true);
            self.core.add(i);
        }
    }

    /// File -> Export after "Export download list" (template 191): the queued, selected or all downloads.
    pub(in crate::app) fn export(&self, ef2: bool, mode: u8) {
        let ext = if ef2 { "ef2" } else { "txt" };
        let Some(p) = rfd::FileDialog::new()
            .set_file_name(format!("downloads.{ext}"))
            .add_filter(ext, &[ext])
            .save_file()
        else {
            return;
        };
        let items: Vec<Info> = self
            .core
            .jobs()
            .iter()
            .map(|j| j.info().clone())
            .filter(|i| match mode {
                0 => i.queued,
                1 => self.sel.contains(&i.id),
                _ => true,
            })
            .collect();
        let ua = self.core.settings().user_agent.clone();
        let out = if ef2 {
            ef2_write(&items, &ua)
        } else {
            items
                .iter()
                .map(|i| i.url.as_str())
                .collect::<Vec<_>>()
                .join("\r\n")
        };
        let _ = std::fs::write(p, out);
    }
}
