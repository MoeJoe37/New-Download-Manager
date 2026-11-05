//! Absolute layout in Windows dialog units, so dialogs reproduce classic Win32 dialog templates.
//! Scale measured from the original dialogs rendered by Windows from their templates
//! (Tahoma 8pt): 1 DLU = 1.395 px across, 1.6 px down at 100% DPI.
mod widgets;

use crate::i18n::{self, tr, tr_os};
use eframe::egui::{
    self, Align, Color32, Layout, Pos2, Rect, Response, Sense, Stroke, Ui, UiBuilder, Vec2,
    WidgetText,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::Mutex;

/// Static text for the helpers: plain strings (English, or text put together in reading order) are
/// translated (`i18n::tr`), RichText is used as given (call sites translate before styling).
pub trait Txt {
    fn txt(self) -> WidgetText;
    /// The translated text in reading order, for the helpers that lay right-to-left text out themselves
    /// (RichText: none).
    fn plain(&self) -> Option<String> {
        None
    }
}
impl Txt for &str {
    fn txt(self) -> WidgetText {
        tr(self).into()
    }
    fn plain(&self) -> Option<String> {
        Some(tr_os(self))
    }
}
impl Txt for String {
    fn txt(self) -> WidgetText {
        tr(&self).into()
    }
    fn plain(&self) -> Option<String> {
        Some(tr_os(self))
    }
}
impl Txt for &String {
    fn txt(self) -> WidgetText {
        tr(self).into()
    }
    fn plain(&self) -> Option<String> {
        Some(tr_os(self))
    }
}
impl Txt for egui::RichText {
    fn txt(self) -> WidgetText {
        self.into()
    }
}

/// The smallest a translation is drawn to fit a control laid out for its English text.
const MIN_FIT: f32 = 0.75;

/// The format egui lays a RichText out with in `ui`, `style` being the widget's default text style.
fn format_of(ui: &Ui, rt: &egui::RichText, style: &egui::TextStyle) -> egui::TextFormat {
    let job = WidgetText::RichText(rt.clone()).into_layout_job(
        ui.style(),
        egui::FontSelection::Style(style.clone()),
        Align::Center,
    );
    job.sections
        .first()
        .map(|s| s.format.clone())
        .unwrap_or_default()
}

fn width_of(ui: &Ui, text: &str, f: &egui::TextFormat) -> f32 {
    ui.fonts(|fo| {
        fo.layout_job(egui::text::LayoutJob::single_section(
            text.to_string(),
            f.clone(),
        ))
        .size()
        .x
    })
}

/// The type size, down to `MIN_FIT` of `size`, at which a text `width_at` measures fits `width`. Glyph
/// widths don't scale exactly with the size, so it steps down from the proportional guess until it fits.
fn fit_size(size: f32, width: f32, width_at: impl Fn(f32) -> f32) -> f32 {
    let w = width_at(size);
    if w <= width || width <= 0.0 || w <= 0.0 {
        return size;
    }
    let min = size * MIN_FIT;
    let mut s = (size * width / w).max(min);
    while s > min && width_at(s) > width {
        s = (s * 0.97).max(min);
    }
    s
}

/// One line of text for a box `width` wide: at its own size, or smaller (down to `MIN_FIT`) when the
/// translation is wider than the English the box was laid out for.
pub fn fit(ui: &Ui, t: WidgetText, width: f32, style: egui::TextStyle) -> WidgetText {
    let WidgetText::RichText(rt) = t else {
        return t;
    };
    let f = format_of(ui, &rt, &style);
    let size = f.font_id.size;
    let fitted = fit_size(size, width, |s| {
        width_of(
            ui,
            rt.text(),
            &egui::TextFormat {
                font_id: egui::FontId::new(s, f.font_id.family.clone()),
                ..f.clone()
            },
        )
    });
    if fitted == size {
        return WidgetText::RichText(rt);
    }
    WidgetText::RichText(rt.size(fitted))
}

/// `fit`, cut short with "…" where it still doesn't fit. egui cuts the end of the text, which in a
/// right-to-left language (display order) is the start of the sentence: there the left end goes instead.
pub fn fit_cut(ui: &Ui, t: WidgetText, width: f32, style: egui::TextStyle) -> WidgetText {
    let t = fit(ui, t, width, style.clone());
    let WidgetText::RichText(rt) = &t else {
        return t;
    };
    let f = format_of(ui, rt, &style);
    if !i18n::is_rtl() || width <= 0.0 || width_of(ui, rt.text(), &f) <= width {
        return t;
    }
    let text = cut_left(rt.text(), width, |s| width_of(ui, s, &f));
    WidgetText::LayoutJob(egui::text::LayoutJob::single_section(text, f))
}

/// Display-order right-to-left text cut short from the left (the end of its reading order) with "…" to be
/// `width` wide as `measure` sizes it.
pub fn cut_left(text: &str, width: f32, measure: impl Fn(&str) -> f32) -> String {
    if measure(text) <= width {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let cut = |k: usize| {
        std::iter::once('\u{2026}')
            .chain(chars[k..].iter().copied())
            .collect::<String>()
    };
    // the fewest characters dropped
    let (mut lo, mut hi) = (1, chars.len());
    while lo < hi {
        let mid = (lo + hi) / 2;
        if measure(&cut(mid)) <= width {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    cut(lo)
}

/// Right-to-left text (reading order) wrapped to `width` in `font` and laid out for display, one string per
/// line (`i18n::wrap_rtl`); remembered, as dialogs draw every frame.
fn rtl_lines(ui: &Ui, text: &str, width: f32, font: &egui::FontId) -> Vec<String> {
    static DONE: Mutex<Option<HashMap<(String, u32, u32), Vec<String>>>> = Mutex::new(None);
    let key = (text.to_string(), width.to_bits(), font.size.to_bits());
    if let Some(v) = DONE.lock().unwrap().as_ref().and_then(|m| m.get(&key)) {
        return v.clone();
    }
    let measure = |l: &str| {
        ui.fonts(|f| {
            f.layout_no_wrap(l.to_string(), font.clone(), Color32::WHITE)
                .size()
                .x
        })
    };
    let lines: Vec<String> = i18n::wrap_rtl(text, width, measure)
        .split('\n')
        .map(str::to_string)
        .collect();
    let mut done = DONE.lock().unwrap();
    let m = done.get_or_insert_with(HashMap::new);
    if m.len() > 512 {
        m.clear();
    }
    m.insert(key, lines.clone());
    lines
}

/// A checkbox or radio button for a right-to-left language, drawn as egui draws its own but with the box
/// on the right of the text. `lines` are display-order lines (one, or more for a box two lines high).
fn rtl_toggle(
    ui: &mut Ui,
    (lines, font): (Vec<String>, egui::FontId),
    on: bool,
    radio: bool,
) -> Response {
    let sp = ui.spacing().clone();
    let galleys: Vec<_> = lines
        .iter()
        .map(|l| {
            ui.painter()
                .layout_no_wrap(l.clone(), font.clone(), Color32::PLACEHOLDER)
        })
        .collect();
    let text = galleys.iter().fold(Vec2::ZERO, |a, g| {
        Vec2::new(a.x.max(g.size().x), a.y + g.size().y)
    });
    let mut size = (Vec2::new(sp.icon_width + sp.icon_spacing, 0.0) + text).max(sp.interact_size);
    size.y = size.y.max(sp.icon_width);
    let (rect, mut resp) = ui.allocate_exact_size(size, Sense::click());
    let kind = if radio {
        egui::WidgetType::RadioButton
    } else {
        egui::WidgetType::Checkbox
    };
    let label = lines.join(" ");
    resp.widget_info(|| egui::WidgetInfo::selected(kind, ui.is_enabled(), on, &label));
    if resp.clicked() {
        resp.mark_changed();
    }
    if ui.is_rect_visible(rect) {
        let v = ui.style().interact(&resp);
        let big = Rect::from_center_size(
            egui::pos2(rect.right() - sp.icon_width / 2.0, rect.center().y),
            Vec2::splat(sp.icon_width),
        );
        let small = Rect::from_center_size(big.center(), Vec2::splat(sp.icon_width_inner));
        let p = ui.painter();
        if radio {
            p.add(egui::epaint::CircleShape {
                center: big.center(),
                radius: big.width() / 2.0 + v.expansion,
                fill: v.bg_fill,
                stroke: v.bg_stroke,
            });
            if on {
                p.add(egui::epaint::CircleShape {
                    center: small.center(),
                    radius: small.width() / 3.0,
                    fill: v.fg_stroke.color,
                    stroke: Stroke::NONE,
                });
            }
        } else {
            p.add(egui::epaint::RectShape::new(
                big.expand(v.expansion),
                v.corner_radius,
                v.bg_fill,
                v.bg_stroke,
                egui::StrokeKind::Inside,
            ));
            if on {
                let pts = vec![
                    egui::pos2(small.left(), small.center().y),
                    egui::pos2(small.center().x, small.bottom()),
                    egui::pos2(small.right(), small.top()),
                ];
                p.add(egui::Shape::line(pts, v.fg_stroke));
            }
        }
        let mut y = rect.center().y - text.y / 2.0;
        for g in galleys {
            let h = g.size().y;
            p.galley(
                egui::pos2(big.left() - sp.icon_spacing - g.size().x, y),
                g,
                v.text_color(),
            );
            y += h;
        }
    }
    resp
}

/// View -> Toolbar -> "NDM Style": NDM's own look for every window (set by main's `style`).
pub static NDM_STYLE: AtomicBool = AtomicBool::new(false);

pub fn ndm() -> bool {
    NDM_STYLE.load(Relaxed)
}

/// NDM Style's colours: a cool grey ground, white (or slate) surfaces, one blue accent.
pub struct Theme {
    pub bg: Color32,
    pub surface: Color32,
    pub raised: Color32,
    pub border: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub accent: Color32,
    pub accent_hot: Color32,
    pub accent_soft: Color32,
    pub on_accent: Color32,
    pub danger: Color32,
    pub success: Color32,
}

pub fn theme(dark: bool) -> Theme {
    let c = Color32::from_rgb;
    if dark {
        Theme {
            bg: c(0x15, 0x17, 0x1c),
            surface: c(0x1d, 0x20, 0x27),
            raised: c(0x27, 0x2b, 0x34),
            border: c(0x34, 0x39, 0x46),
            text: c(0xe6, 0xe8, 0xec),
            muted: c(0x9a, 0xa1, 0xae),
            accent: c(0x4c, 0x8d, 0xff),
            accent_hot: c(0x6a, 0xa1, 0xff),
            accent_soft: c(0x23, 0x34, 0x52),
            on_accent: Color32::WHITE,
            danger: c(0xf0, 0x62, 0x62),
            success: c(0x3f, 0xbf, 0x7a),
        }
    } else {
        Theme {
            bg: c(0xf3, 0xf5, 0xf9),
            surface: Color32::WHITE,
            raised: c(0xec, 0xef, 0xf4),
            border: c(0xd9, 0xde, 0xe7),
            text: c(0x1c, 0x23, 0x30),
            muted: c(0x67, 0x70, 0x81),
            accent: c(0x2f, 0x6f, 0xeb),
            accent_hot: c(0x24, 0x59, 0xc4),
            accent_soft: c(0xe2, 0xeb, 0xfd),
            on_accent: Color32::WHITE,
            danger: c(0xd9, 0x3a, 0x3a),
            success: c(0x1f, 0x9d, 0x55),
        }
    }
}

/// NDM Style's corner radii: controls, and cards (groups, lists, tab pages).
pub const R_CTRL: f32 = 5.0;
pub const R_CARD: f32 = 7.0;

/// Buttons NDM Style fills with the accent colour: the action a dialog is for.
const PRIMARY: [&str; 6] = [
    "OK",
    "Start Download",
    "Download Now",
    "Yes",
    "Save",
    "Install",
];

pub const DX: f32 = 1.395;
pub const DY: f32 = 1.6;
pub const WIN_GREEN: Color32 = Color32::from_rgb(6, 176, 37);

pub fn size(w: f32, h: f32) -> Vec2 {
    Vec2::new(w * DX, h * DY)
}

/// Colours for the classic controls, taken from the current light/dark visuals.
pub struct Pal {
    pub field: Color32,
    pub text: Color32,
    pub edge: Color32,
    pub etch_dark: Color32,
    pub etch_light: Color32,
    pub tab: Color32,
    pub tab_hot: Color32,
    pub line: Color32,
    pub panel: Color32,
    /// List view background: NDM's dark palette uses the dialog colour (0x393939), light mode white.
    pub list: Color32,
}

pub fn pal(ui: &Ui) -> Pal {
    let v = ui.visuals();
    if ndm() {
        let t = theme(v.dark_mode);
        return Pal {
            field: t.surface,
            text: t.text,
            edge: t.border,
            etch_dark: t.border,
            etch_light: t.border,
            tab: t.raised,
            tab_hot: t.accent_soft,
            line: t.border,
            panel: v.panel_fill,
            list: t.surface,
        };
    }
    if v.dark_mode {
        Pal {
            field: v.extreme_bg_color,
            text: v.text_color(),
            edge: Color32::from_gray(90),
            etch_dark: Color32::from_gray(70),
            etch_light: Color32::from_gray(45),
            tab: Color32::from_gray(43),
            tab_hot: Color32::from_gray(60),
            line: Color32::from_gray(64),
            panel: v.panel_fill,
            list: v.panel_fill,
        }
    } else {
        Pal {
            field: Color32::WHITE,
            text: Color32::BLACK,
            edge: Color32::from_gray(160),
            etch_dark: Color32::from_gray(160),
            etch_light: Color32::WHITE,
            tab: Color32::from_gray(240),
            tab_hot: Color32::from_rgb(216, 234, 249),
            line: Color32::from_gray(217),
            panel: v.panel_fill,
            list: Color32::WHITE,
        }
    }
}

/// `font`, smaller (down to `MIN_FIT`) where `text` is wider than `width` in it.
pub fn fit_font(ui: &Ui, text: &str, font: egui::FontId, width: f32) -> egui::FontId {
    let at = |s: f32| {
        ui.fonts(|f| {
            f.layout_no_wrap(
                text.to_string(),
                egui::FontId::new(s, font.family.clone()),
                Color32::WHITE,
            )
            .size()
            .x
        })
    };
    egui::FontId::new(fit_size(font.size, width, at), font.family.clone())
}

/// The layout a dialog row reads in: from the left, or from the right in right-to-left languages.
fn reading() -> Layout {
    if i18n::is_rtl() {
        Layout::right_to_left(Align::Center)
    } else {
        Layout::left_to_right(Align::Center)
    }
}

/// A checkbox for a list row or a dialog spot (`ui.add` / `add_sized`), at the start of the space it gets:
/// egui's, or with its box on the right in right-to-left languages. `t` is English (translated here).
pub fn checkbox<'a>(on: &'a mut bool, t: &'a str) -> impl egui::Widget + 'a {
    move |ui: &mut Ui| {
        ui.with_layout(reading(), |ui| {
            if !i18n::is_rtl() {
                return ui.add(egui::Checkbox::new(on, tr(t)));
            }
            let text = (vec![tr(t)], egui::TextStyle::Button.resolve(ui.style()));
            let r = rtl_toggle(ui, text, *on, false);
            if r.clicked() {
                *on = !*on;
            }
            r
        })
        .inner
    }
}

/// A radio button for a dialog row (`ui.add`): egui's, or with its circle on the right in right-to-left
/// languages. `t` is English (translated here).
pub fn radio_button<'a>(on: bool, t: &'a str) -> impl egui::Widget + 'a {
    move |ui: &mut Ui| {
        ui.with_layout(reading(), |ui| {
            if !i18n::is_rtl() {
                return ui.add(egui::RadioButton::new(on, tr(t)));
            }
            rtl_toggle(
                ui,
                (vec![tr(t)], egui::TextStyle::Button.resolve(ui.style())),
                on,
                true,
            )
        })
        .inner
    }
}

/// The sizes a translation that wraps is tried at, so it stays in the box laid out for the English.
const STEPS: [f32; 5] = [1.0, 0.94, 0.88, 0.82, MIN_FIT];

/// Wrapping text for a box `width` x `height`: the largest of `STEPS` its lines fit in (the smallest when
/// none does).
fn fit_block(
    ui: &Ui,
    rt: egui::RichText,
    width: f32,
    height: f32,
    style: egui::TextStyle,
) -> WidgetText {
    let size = format_of(ui, &rt, &style).font_id.size;
    for k in STEPS {
        let t = rt.clone().size(size * k);
        let g = WidgetText::RichText(t.clone()).into_galley(
            ui,
            Some(egui::TextWrapMode::Wrap),
            width,
            style.clone(),
        );
        if g.size().y <= height + 1.0 || k == MIN_FIT {
            return WidgetText::RichText(t);
        }
    }
    WidgetText::RichText(rt)
}

/// `fit_block` for right-to-left text in reading order: display-order lines and their font.
fn fit_block_rtl(
    ui: &Ui,
    text: &str,
    width: f32,
    height: f32,
    style: egui::TextStyle,
) -> (Vec<String>, egui::FontId) {
    let font = style.resolve(ui.style());
    let row = ui.fonts(|f| f.row_height(&font));
    for k in STEPS {
        let f = egui::FontId::new(font.size * k, font.family.clone());
        let lines = rtl_lines(ui, text, width, &f);
        if lines.len() as f32 * row * k <= height + 1.0 || k == MIN_FIT {
            return (lines, f);
        }
    }
    (vec![text.to_string()], font)
}

/// A checkbox's or radio button's text for `rtl_toggle` (display-order lines and their font): one line
/// made to fit `width`, or for a box two lines high, wrapped in reading order to fit it.
fn toggle_text(ui: &Ui, t: &str, width: f32, height: Option<f32>) -> (Vec<String>, egui::FontId) {
    if let Some(h) = height {
        return fit_block_rtl(ui, &tr_os(t), width, h, egui::TextStyle::Button);
    }
    let line = tr(t);
    let font = fit_font(
        ui,
        &line,
        egui::TextStyle::Button.resolve(ui.style()),
        width,
    );
    (vec![line], font)
}

pub struct D<'a> {
    pub ui: &'a mut Ui,
    o: Pos2,
    /// px per DLU across: DX, or wider for pages NDM creates with their own font metrics.
    dx: f32,
    /// Right-to-left languages: every rect is mirrored across the window (left + right edge x).
    mirror: Option<f32>,
}

#[test]
fn combo_closes_after_selecting_a_choice() {
    let ctx = egui::Context::default();
    let frame = |selected: &mut String, events| {
        ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(480.0, 300.0))),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    D::new(ui).combo(
                        20.0,
                        20.0,
                        130.0,
                        15.0,
                        "quality",
                        selected,
                        &["Option-720p", "Option-MP3"],
                        true,
                    );
                });
            },
        )
    };
    let label = |out: &egui::FullOutput, value: &str| {
        out.shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Text(t) if t.galley.text() == value => {
                    Some(t.pos + t.galley.size() / 2.0)
                }
                _ => None,
            })
            .max_by(|a, b| a.y.total_cmp(&b.y))
            .expect("choice is visible")
    };
    let click = |selected: &mut String, pos| {
        frame(selected, vec![egui::Event::PointerMoved(pos)]);
        for pressed in [true, false] {
            frame(
                selected,
                vec![egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                }],
            );
        }
    };

    let mut selected = "Option-720p".to_string();
    let closed = frame(&mut selected, vec![]);
    click(&mut selected, label(&closed, "Option-720p"));
    assert!(ctx.memory(|memory| memory.any_popup_open()));
    let open = frame(&mut selected, vec![]);
    click(&mut selected, label(&open, "Option-MP3"));
    assert_eq!(selected, "Option-MP3");
    assert!(!ctx.memory(|memory| memory.any_popup_open()));

    // Selecting the current item should close the menu too.
    let closed = frame(&mut selected, vec![]);
    click(&mut selected, label(&closed, "Option-MP3"));
    assert!(ctx.memory(|memory| memory.any_popup_open()));
    let open = frame(&mut selected, vec![]);
    click(&mut selected, label(&open, "Option-MP3"));
    assert_eq!(selected, "Option-MP3");
    assert!(!ctx.memory(|memory| memory.any_popup_open()));
}
