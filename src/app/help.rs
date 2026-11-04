use crate::i18n::{self, tr, tr_os};
use crate::{cached_texture, glyph_strip, open_url, UPDATE_REPO};
use eframe::egui::{self, Align, Align2, Color32, FontId, Layout, Rect, RichText, Sense, Vec2};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// NDM's own help: `help\*.htm` next to the exe or in a parent folder.
fn help_dir() -> Option<PathBuf> {
    i18n::beside_exe("help", "index.htm")
}

/// The help page the UI should show next ("page.htm" or "page.htm#anchor"), set by `help`.
static HELP_REQ: Mutex<Option<String>> = Mutex::new(None);

/// Opens NDM's help at `page` in its own window (`Dialog::Help`). Returns whether the help exists, so Help
/// buttons grey out without it.
pub(crate) fn help(page: &str, open: bool) -> bool {
    let ok = help_dir().is_some();
    if ok && open {
        *HELP_REQ.lock().unwrap() = Some(page.to_string());
    }
    ok
}

pub(crate) fn take_help_request() -> Option<String> {
    HELP_REQ.lock().unwrap().take()
}

/// What a help block is: the page heading (h1), its lead-in (p class="lead"), a section heading (h2),
/// body text, a list item, a code line (pre) or a tip / note callout (p class="tip" / "note").
#[derive(Clone, Copy, Default, PartialEq, Debug)]
enum HelpKind {
    #[default]
    Text,
    Title,
    Lead,
    Heading,
    Item,
    Code,
    Tip,
    Note,
    Image,
}

/// A run of help text: bold, italic, key / code, link target.
#[derive(Clone, Default, PartialEq, Debug)]
struct HelpRun {
    text: String,
    bold: bool,
    italic: bool,
    code: bool,
    link: Option<String>,
}

/// A help block: its kind, its id (the anchor links point at) and its runs. An <ol> item has its
/// number; an image has its file (relative to help/) and its alt text, also its caption, as the runs.
#[derive(Default)]
struct HelpPara {
    kind: HelpKind,
    id: Option<String>,
    runs: Vec<HelpRun>,
    num: Option<u32>,
    src: Option<String>,
}

impl HelpPara {
    fn text(&self) -> String {
        self.runs.iter().map(|r| r.text.as_str()).collect::<String>().trim().to_string()
    }
}

/// The small HTML the help pages are written in (h1, h2, p, p class=lead/tip/note, ul/ol/li, pre, img,
/// b, i, kbd/code, a href) as blocks; returns the page's <title> too.
fn help_parse(html: &str) -> (String, Vec<HelpPara>) {
    let lower = html.to_ascii_lowercase();
    let title = lower.find("<title>").and_then(|a| lower[a..].find("</title>").map(|b| html[a + 7..a + b].trim().to_string())).unwrap_or_default();
    let body = lower.find("<body").map_or(html, |i| &html[i..]);
    let attr = |tag: &str, name: &str| {
        let l = tag.to_ascii_lowercase();
        let i = l.find(&format!("{name}="))? + name.len() + 1;
        let v = tag[i..].trim_start_matches('"');
        let end = if tag[i..].starts_with('"') { v.find('"') } else { v.find([' ', '>']) };
        Some(v[..end.unwrap_or(v.len())].to_string())
    };
    // the item number of the list being read (None in a <ul>)
    let mut ol: Option<u32> = None;
    let (mut paras, mut cur) = (vec![], HelpPara::default());
    let (mut bold, mut italic, mut code, mut link) = (0, 0, 0, None::<String>);
    let flush = |cur: &mut HelpPara, paras: &mut Vec<HelpPara>| {
        let kind = cur.kind;
        if !cur.text().is_empty() {
            paras.push(std::mem::take(cur));
        }
        (cur.kind, cur.num) = (kind, None);
    };
    for chunk in body.split('<').skip(1) {
        let (tag, text) = chunk.split_once('>').unwrap_or((chunk, ""));
        let name = tag.split_whitespace().next().unwrap_or("").to_ascii_lowercase();
        let block = match name.as_str() {
            "h1" => Some(HelpKind::Title),
            "h2" | "h3" => Some(HelpKind::Heading),
            "li" => Some(HelpKind::Item),
            "pre" => Some(HelpKind::Code),
            "p" | "div" => Some(match attr(tag, "class").as_deref() {
                Some("lead") => HelpKind::Lead,
                Some("tip") => HelpKind::Tip,
                Some("note") => HelpKind::Note,
                _ => HelpKind::Text,
            }),
            _ => None,
        };
        match name.as_str() {
            _ if block.is_some() => {
                flush(&mut cur, &mut paras);
                (cur.kind, cur.id) = (block.unwrap(), attr(tag, "id"));
                if name == "li" {
                    ol = ol.map(|n| n + 1);
                    cur.num = ol;
                }
            }
            "/h1" | "/h2" | "/h3" | "/li" | "/pre" | "/p" | "/div" | "ul" | "/ul" | "ol" | "/ol" => {
                flush(&mut cur, &mut paras);
                cur.kind = HelpKind::Text;
                ol = match name.as_str() {
                    "ol" => Some(0),
                    "ul" | "/ul" | "/ol" => None,
                    _ => ol,
                };
            }
            "img" => {
                flush(&mut cur, &mut paras);
                let alt = attr(tag, "alt").unwrap_or_default().replace("&quot;", "\"").replace("&amp;", "&");
                paras.push(HelpPara { kind: HelpKind::Image, id: attr(tag, "id"), runs: vec![HelpRun { text: alt, ..Default::default() }], num: None, src: attr(tag, "src") });
            }
            "br" => flush(&mut cur, &mut paras),
            "b" | "strong" => bold += 1,
            "/b" | "/strong" => bold -= 1,
            "i" | "em" => italic += 1,
            "/i" | "/em" => italic -= 1,
            "kbd" | "code" => code += 1,
            "/kbd" | "/code" => code -= 1,
            "a" => link = attr(tag, "href"),
            "/a" => link = None,
            _ => {}
        }
        let t = text.replace("&nbsp;", " ").replace("&gt;", ">").replace("&lt;", "<").replace("&quot;", "\"").replace("&amp;", "&");
        let t = if cur.kind == HelpKind::Code { t.trim().to_string() } else { t.split_whitespace().collect::<Vec<_>>().join(" ") };
        if t.is_empty() {
            continue;
        }
        // keep the space the source had between runs
        let space = if text.starts_with(char::is_whitespace) && !cur.runs.is_empty() { " " } else { "" };
        let end = if text.ends_with(char::is_whitespace) { " " } else { "" };
        cur.runs.push(HelpRun { text: format!("{space}{t}{end}"), bold: bold > 0, italic: italic > 0, code: code > 0, link: link.clone() });
    }
    flush(&mut cur, &mut paras);
    (title, paras)
}

#[test]
fn help_pages() {
    let (_, p) = help_parse(r#"<body><h1>T</h1><ol><li>One</li><li>Two</li></ol><ul><li>Dot</li></ul><p><img src="images/a.png" alt="The main window"></p></body>"#);
    assert_eq!(p.iter().map(|p| p.num).collect::<Vec<_>>(), [None, Some(1), Some(2), None, None]);
    assert_eq!((p[4].kind, p[4].src.as_deref(), p[4].text().as_str()), (HelpKind::Image, Some("images/a.png"), "The main window"));
    let (title, p) = help_parse(r#"<html><head><title>T</title></head><body><h1>Head</h1><p class="lead">Lead</p><h2 id="x">Sec</h2><p><b>Bold</b> and <a href="b.htm#y">a link</a> -&gt; <kbd>Del</kbd></p><ul><li>One</li></ul><pre>a  b</pre><p class="tip">Tip</p></body></html>"#);
    assert_eq!(title, "T");
    let kinds: Vec<_> = p.iter().map(|p| p.kind).collect();
    use HelpKind::*;
    assert_eq!(kinds, [Title, Lead, Heading, Text, Item, Code, Tip]);
    assert_eq!(p[2].id.as_deref(), Some("x"));
    let runs: Vec<_> = p[3].runs.iter().map(|r| (r.text.as_str(), r.bold, r.code, r.link.as_deref())).collect();
    assert_eq!(runs, [("Bold", true, false, None), (" and ", false, false, None), ("a link", false, false, Some("b.htm#y")), (" -> ", false, false, None), ("Del", false, true, None)]);
    assert_eq!(p[5].text(), "a  b");
    // every shipped page parses, has a heading, and its links point at pages and sections that exist
    let dir = help_dir().expect("help folder");
    for f in std::fs::read_dir(&dir).unwrap().flatten().filter(|f| f.path().extension().is_some_and(|e| e == "htm")) {
        let (t, paras) = help_parse(&std::fs::read_to_string(f.path()).unwrap());
        assert!(!t.is_empty() && paras.first().is_some_and(|p| p.kind == Title), "{:?}", f.path());
        // screenshots: there, readable, small enough, with a caption
        for img in paras.iter().filter(|p| p.kind == Image) {
            let src = img.src.as_deref().unwrap_or_default();
            let file = dir.join(src);
            let size = std::fs::metadata(&file).unwrap_or_else(|_| panic!("{src} in {:?}", f.path())).len();
            let pic = image::open(&file).unwrap_or_else(|e| panic!("{src}: {e}"));
            assert!(pic.width() <= 1400 && size <= 300 * 1024, "{src}: {} px, {size} bytes", pic.width());
            assert!(!img.text().is_empty(), "{src} has no alt text");
        }
        for l in paras.iter().flat_map(|p| &p.runs).filter_map(|r| r.link.as_deref()).filter(|l| !l.contains("://")) {
            let (file, anchor) = l.split_once('#').map_or((l, None), |(f, a)| (f, Some(a)));
            let (_, target) = help_parse(&std::fs::read_to_string(dir.join(file)).unwrap_or_else(|_| panic!("{l} in {:?}", f.path())));
            assert!(anchor.is_none_or(|a| target.iter().any(|p| p.id.as_deref() == Some(a))), "{l} in {:?}", f.path());
        }
    }
}

/// The help window's three icons (Segoe Fluent Icons / MDL2 glyphs, drawn white and tinted when shown).
const HELP_SEARCH: usize = 0;
const HELP_BACK: usize = 1;
const HELP_FWD: usize = 2;
const HELP_CONTROLS: [u32; 3] = [0xE721, 0xE72B, 0xE72A];

fn help_icons(ctx: &egui::Context) -> Option<egui::TextureHandle> {
    cached_texture(ctx, "help_icons", || {
        let cells: Vec<(u32, [u8; 3])> = HELP_CONTROLS.iter().map(|g| (*g, [255; 3])).collect();
        glyph_strip(&cells, 64, 64, 46.0)
    })
}

/// A help page's screenshot (`src` relative to help/), loaded once.
fn help_image(ctx: &egui::Context, dir: &Path, src: &str) -> Option<egui::TextureHandle> {
    cached_texture(ctx, &format!("help:{src}"), || {
        let i = image::open(dir.join(src)).ok()?.into_rgba8();
        Some((i.width() as usize, i.height() as usize, i.into_raw()))
    })
}

/// Paints icon `i` of the help strip in `rect`, tinted `tint`.
fn help_icon(ui: &egui::Ui, tex: &egui::TextureHandle, i: usize, rect: Rect, tint: Color32) {
    let n = HELP_CONTROLS.len() as f32;
    let uv = Rect::from_min_max(egui::pos2(i as f32 / n, 0.0), egui::pos2((i + 1) as f32 / n, 1.0));
    egui::Image::new((tex.id(), rect.size())).uv(uv).tint(tint).paint_at(ui, rect);
}

/// A help paragraph's runs, wrapped: links in `link` (a click sets `go`), keys and code on `chip`.
fn help_runs(ui: &mut egui::Ui, runs: &[HelpRun], size: f32, color: Color32, strong: Color32, (link, chip): (Color32, Color32), go: &mut Option<String>) -> egui::Response {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = Vec2::new(0.0, 5.0);
        for r in runs {
            let mut t = if r.code {
                RichText::new(format!("\u{2009}{}\u{2009}", r.text.trim())).monospace().size(size - 1.5).color(strong).background_color(chip)
            } else {
                RichText::new(r.text.as_str()).size(size).color(if r.bold { strong } else { color })
            };
            if r.bold {
                t = t.strong();
            }
            if r.italic {
                t = t.italics();
            }
            match &r.link {
                Some(l) => {
                    if ui.add(egui::Link::new(t.color(link))).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                        *go = Some(l.clone());
                    }
                }
                None => {
                    ui.label(t);
                }
            }
        }
    })
    .response
}

/// Help search: every section whose text has all the words, under its page and heading.
fn help_search(
    ui: &mut egui::Ui,
    topics: &[(String, String, String)],
    read: &dyn Fn(&str) -> (String, Vec<HelpPara>),
    query: &str,
    shown: &str,
    (text, strong, weak, link, row): (Color32, Color32, Color32, Color32, Color32),
    go: &mut Option<String>,
) {
    ui.label(RichText::new(i18n::trf("Results for \"%s\"", &[&shown])).size(22.0).strong().color(strong));
    ui.add_space(12.0);
    let mut found = 0;
    for (f, name, _) in topics.iter().skip(1) {
        let (_, paras) = read(f);
        let mut seen = HashSet::new();
        let mut head: (Option<String>, String) = (None, String::new());
        for p in &paras {
            if p.kind == HelpKind::Heading {
                head = (p.id.clone(), p.text());
            }
            let t = p.text();
            let low = t.to_lowercase();
            if !query.split_whitespace().all(|w| low.contains(w)) || !seen.insert(head.0.clone()) {
                continue;
            }
            found += 1;
            let r = egui::Frame::new().corner_radius(3).inner_margin(egui::Margin::symmetric(8, 7)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                let at = if head.1.is_empty() { tr(name) } else { i18n::line(&format!("{}  \u{203A}  {}", tr_os(name), tr_os(&head.1))) };
                ui.label(RichText::new(at).size(14.0).color(link));
                let snip = if t.chars().count() > 180 { t.chars().take(180).collect::<String>() + "\u{2026}" } else { t.clone() };
                ui.label(RichText::new(snip).size(13.0).color(text));
            });
            let r = r.response.interact(Sense::click());
            if r.hovered() {
                ui.painter().rect_filled(r.rect, 3.0, row.gamma_multiply(0.5));
            }
            if r.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                *go = Some(head.0.as_ref().map_or(f.clone(), |id| format!("{f}#{id}")));
            }
        }
    }
    if found == 0 {
        ui.label(RichText::new(tr("Nothing matches. Try fewer or different words.")).size(14.0).color(weak));
    }
}

pub(crate) fn help_body(ctx: &egui::Context, ui: &mut egui::Ui, page: &mut String, anchor: &mut Option<String>, back: &mut Vec<String>, fwd: &mut Vec<String>, search: &mut String) -> bool {
    let Some(dir) = help_dir() else { return false };
    let read = |f: &str| help_parse(&std::fs::read_to_string(dir.join(f)).unwrap_or_default());
    let mut go = None::<String>;
    // the topics: Home, then index.htm's list (link, name, summary)
    let (_, index) = read("index.htm");
    let topics: Vec<(String, String, String)> = std::iter::once(("index.htm".to_string(), "Home".to_string(), String::new()))
        .chain(index.iter().filter(|p| p.kind == HelpKind::Item).filter_map(|p| {
            let first = p.runs.first()?;
            let link = first.link.clone().filter(|l| !l.contains("://"))?;
            Some((link, first.text.trim().to_string(), p.runs[1..].iter().map(|r| r.text.as_str()).collect::<String>().trim().to_string()))
        }))
        .collect();
    let file = page.split('#').next().unwrap_or("index.htm").to_string();
    let (_, paras) = read(&file);
    let icons = help_icons(ctx);
    let v = ui.visuals().clone();
    // blue is for links only; everything else is the text colours and greys
    let link = if v.dark_mode { Color32::from_rgb(120, 175, 255) } else { Color32::from_rgb(0, 95, 184) };
    let (text, strong, weak) = (v.text_color(), v.strong_text_color(), v.weak_text_color());
    let (side, paper, line, row, chip) = if v.dark_mode {
        (Color32::from_gray(30), Color32::from_gray(22), Color32::from_gray(55), Color32::from_gray(48), Color32::from_gray(55))
    } else {
        (Color32::from_gray(246), Color32::WHITE, Color32::from_gray(222), Color32::from_gray(230), Color32::from_gray(236))
    };
    let hand = egui::CursorIcon::PointingHand;
    let searching = !search.trim().is_empty();

    // sidebar: search, then the contents, the current topic's sections under it
    egui::SidePanel::left("help_nav").resizable(false).exact_width(228.0).show_separator_line(true)
        .frame(egui::Frame::new().fill(side).inner_margin(egui::Margin::symmetric(12, 14)))
        .show_inside(ui, |ui| {
            egui::Frame::new().fill(paper).stroke(egui::Stroke::new(1.0_f32, line)).corner_radius(4).inner_margin(egui::Margin::symmetric(8, 5)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    if let Some(t) = &icons {
                        let (r, _) = ui.allocate_exact_size(Vec2::splat(14.0), Sense::hover());
                        help_icon(ui, t, HELP_SEARCH, r, weak);
                    }
                    ui.add(egui::TextEdit::singleline(search).hint_text(tr("Search help")).frame(false).desired_width(f32::INFINITY));
                });
            });
            ui.add_space(14.0);
            ui.label(RichText::new(tr("Contents")).size(13.0).strong().color(strong));
            ui.add_space(4.0);
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                for (f, name, _) in &topics {
                    let on = !searching && *f == file;
                    let (rect, r) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 26.0), Sense::click());
                    if on {
                        ui.painter().rect_filled(rect, 3.0, row);
                    } else if r.hovered() {
                        ui.painter().rect_filled(rect, 3.0, row.gamma_multiply(0.5));
                    }
                    ui.painter().text(rect.left_center() + Vec2::new(8.0, 0.0), Align2::LEFT_CENTER, tr(name), FontId::proportional(13.5), if on { strong } else { text });
                    if on {
                        // no bold face is loaded: a second pass a hair to the right makes it bold
                        ui.painter().text(rect.left_center() + Vec2::new(8.5, 0.0), Align2::LEFT_CENTER, tr(name), FontId::proportional(13.5), strong);
                    }
                    if r.on_hover_cursor(hand).clicked() {
                        go = Some(f.clone());
                    }
                    if on {
                        for h in paras.iter().filter(|p| p.kind == HelpKind::Heading && p.id.is_some()) {
                            ui.horizontal(|ui| {
                                ui.add_space(20.0);
                                if ui.add(egui::Link::new(RichText::new(tr(&h.text())).size(12.5).color(link))).on_hover_cursor(hand).clicked() {
                                    *anchor = h.id.clone();
                                }
                            });
                        }
                        ui.add_space(4.0);
                    }
                }
                ui.add_space(12.0);
                if ui.link(RichText::new(i18n::line(&format!("{} \u{2197}", tr_os("NDM on GitHub")))).size(12.5).color(link)).clicked() {
                    go = Some(format!("https://github.com/{UPDATE_REPO}"));
                }
            });
        });

    // top bar: back, forward, where you are
    let pos = topics.iter().position(|t| t.0 == file);
    egui::TopBottomPanel::top("help_bar").show_separator_line(true)
        .frame(egui::Frame::new().fill(paper).inner_margin(egui::Margin::symmetric(14, 8)))
        .show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                for (i, is_back) in [(HELP_BACK, true), (HELP_FWD, false)] {
                    let on = if is_back { !back.is_empty() } else { !fwd.is_empty() };
                    let (rect, r) = ui.allocate_exact_size(Vec2::splat(28.0), if on { Sense::click() } else { Sense::hover() });
                    if on && r.hovered() {
                        ui.painter().rect_filled(rect, 3.0, row);
                    }
                    if let Some(t) = &icons {
                        help_icon(ui, t, i, Rect::from_center_size(rect.center(), Vec2::splat(14.0)), if on { strong } else { weak.gamma_multiply(0.5) });
                    }
                    if on && r.on_hover_text(tr(if is_back { "Back" } else { "Forward" })).clicked() {
                        let (from, to) = if is_back { (&mut *back, &mut *fwd) } else { (&mut *fwd, &mut *back) };
                        if let Some(p) = from.pop() {
                            to.push(page.clone());
                            (*page, *anchor) = (p, None);
                            search.clear();
                        }
                    }
                }
                ui.add_space(8.0);
                if ui.link(RichText::new(tr("Help")).size(13.0).color(link)).clicked() {
                    go = Some("index.htm".into());
                }
                if file != "index.htm" || searching {
                    let here = if searching { tr("Search") } else { pos.map_or(String::new(), |i| tr(&topics[i].1)) };
                    ui.label(RichText::new("\u{203A}").size(13.0).color(weak));
                    ui.label(RichText::new(here).size(13.0).color(text));
                }
            });
        });

    // the page, in a reading column of about 65 characters
    egui::CentralPanel::default().frame(egui::Frame::new().fill(paper)).show_inside(ui, |ui| {
        let query = search.trim().to_lowercase();
        egui::ScrollArea::vertical().auto_shrink(false).id_salt(("help", file.clone(), query.is_empty())).show(ui, |ui| {
            let width = (ui.available_width() - 64.0).clamp(300.0, 600.0);
            let pad = ((ui.available_width() - width) / 2.0).max(24.0);
            ui.horizontal(|ui| {
                ui.add_space(pad);
                ui.vertical(|ui| {
                    ui.set_width(width);
                    ui.add_space(22.0);
                    if !query.is_empty() {
                        help_search(ui, &topics, &read, &query, search.trim(), (text, strong, weak, link, row), &mut go);
                        return;
                    }
                    for p in &paras {
                        let resp = match p.kind {
                            HelpKind::Title => {
                                let r = ui.label(RichText::new(tr(&p.text())).size(26.0).strong().color(strong));
                                ui.add_space(4.0);
                                r
                            }
                            HelpKind::Lead => {
                                ui.add_space(4.0);
                                help_runs(ui, &p.runs, 15.0, text, strong, (link, chip), &mut go)
                            }
                            HelpKind::Heading => {
                                ui.add_space(22.0);
                                let r = ui.label(RichText::new(tr(&p.text())).size(18.0).strong().color(strong));
                                ui.add_space(2.0);
                                r
                            }
                            HelpKind::Text => {
                                ui.add_space(7.0);
                                help_runs(ui, &p.runs, 14.0, text, strong, (link, chip), &mut go)
                            }
                            HelpKind::Tip | HelpKind::Note => {
                                // an ordinary paragraph that starts with a bold "Tip:" / "Note:"
                                ui.add_space(7.0);
                                let label = tr(if p.kind == HelpKind::Tip { "Tip:" } else { "Note:" });
                                let runs: Vec<HelpRun> = std::iter::once(HelpRun { text: format!("{label} "), bold: true, ..Default::default() }).chain(p.runs.iter().cloned()).collect();
                                help_runs(ui, &runs, 14.0, text, strong, (link, chip), &mut go)
                            }
                            HelpKind::Item => {
                                ui.add_space(3.0);
                                ui.horizontal(|ui| {
                                    let mark = p.num.map_or("\u{2022}".to_string(), |n| format!("{n}."));
                                    let (r, _) = ui.allocate_exact_size(Vec2::new(22.0, 19.0), Sense::hover());
                                    ui.painter().text(egui::pos2(r.right() - 7.0, r.top() + 1.0), Align2::RIGHT_TOP, mark, FontId::proportional(14.0), text);
                                    ui.vertical(|ui| help_runs(ui, &p.runs, 14.0, text, strong, (link, chip), &mut go)).inner
                                })
                                .inner
                            }
                            HelpKind::Code => {
                                ui.add_space(8.0);
                                let r = egui::Frame::new().fill(chip).corner_radius(3).inner_margin(egui::Margin::symmetric(12, 8)).show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    ui.label(RichText::new(p.text()).monospace().size(13.0).color(strong));
                                });
                                r.response
                            }
                            HelpKind::Image => {
                                // scaled to the column, never above its own size; a thin border and the caption below
                                ui.add_space(12.0);
                                let tex = p.src.as_deref().and_then(|src| help_image(ctx, &dir, src));
                                let r = match &tex {
                                    Some(t) => {
                                        let natural = t.size_vec2() / ctx.pixels_per_point();
                                        let size = natural * (ui.available_width() / natural.x).min(1.0);
                                        let r = ui.add(egui::Image::new((t.id(), size)));
                                        ui.painter().rect_stroke(r.rect, 0.0, egui::Stroke::new(1.0_f32, line), egui::StrokeKind::Outside);
                                        r
                                    }
                                    None => ui.label(RichText::new(format!("[{}]", p.text())).size(12.5).color(weak)),
                                };
                                ui.add_space(4.0);
                                ui.label(RichText::new(tr(&p.text())).size(12.5).color(weak));
                                ui.add_space(14.0);
                                r
                            }
                        };
                        if p.id.is_some() && p.id == *anchor {
                            resp.scroll_to_me(Some(Align::TOP));
                            *anchor = None;
                        }
                    }
                    // previous / next topic: one line under a rule
                    if let Some(i) = pos {
                        ui.add_space(28.0);
                        ui.painter().hline(ui.max_rect().x_range(), ui.cursor().top(), egui::Stroke::new(1.0_f32, line));
                        ui.add_space(12.0);
                        ui.horizontal(|ui| {
                            if let Some(j) = i.checked_sub(1) {
                                if ui.add(egui::Link::new(RichText::new(i18n::line(&format!("\u{2039} {}", tr_os(&topics[j].1)))).size(13.5).color(link))).on_hover_cursor(hand).clicked() {
                                    go = Some(topics[j].0.clone());
                                }
                            }
                            if let Some(j) = Some(i + 1).filter(|j| *j < topics.len()) {
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if ui.add(egui::Link::new(RichText::new(i18n::line(&format!("{} \u{203A}", tr_os(&topics[j].1)))).size(13.5).color(link))).on_hover_cursor(hand).clicked() {
                                        go = Some(topics[j].0.clone());
                                    }
                                });
                            }
                        });
                    }
                    ui.add_space(28.0);
                });
            });
        });
    });
    if let Some(l) = go {
        if l.contains("://") {
            open_url(&l);
        } else {
            let (f, a) = l.split_once('#').map_or((l.clone(), None), |(f, a)| (f.to_string(), Some(a.to_string())));
            if f != file || !search.is_empty() {
                back.push(page.clone());
                fwd.clear();
            }
            (*page, *anchor) = (f, a);
            search.clear();
        }
    }
    true
}

#[test]
fn help_view_renders_page_and_search() {
    let draw = |mut search: String| {
        let ctx = egui::Context::default();
        let output = ctx.run(egui::RawInput {
            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(980.0, 660.0))),
            ..Default::default()
        }, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                assert!(help_body(ctx, ui, &mut "index.htm".into(), &mut None, &mut vec![], &mut vec![], &mut search));
            });
        });
        output.shapes.into_iter().filter_map(|shape| match shape.shape {
            egui::Shape::Text(text) => Some(text.galley.text().to_string()),
            _ => None,
        }).collect::<Vec<_>>()
    };
    let page = draw(String::new());
    assert!(page.iter().any(|s| s == "New Download Manager"));
    let results = draw("zzzxxyy-no-help-results".into());
    assert!(results.iter().any(|s| s.contains("zzzxxyy-no-help-results")));
    assert!(results.iter().any(|s| s == &tr("Nothing matches. Try fewer or different words.")));
}
