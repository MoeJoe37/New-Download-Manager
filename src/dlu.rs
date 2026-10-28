//! Absolute layout in Windows dialog units, so dialogs reproduce classic Win32 dialog templates.
//! Scale measured from the original dialogs rendered by Windows from their templates
//! (Tahoma 8pt): 1 DLU = 1.395 px across, 1.6 px down at 100% DPI.
use crate::i18n::tr;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use eframe::egui::{self, Align, Color32, Layout, Pos2, Rect, Response, Sense, Stroke, Ui, UiBuilder, Vec2, WidgetText};

/// Static text for the helpers: plain strings are translated (`i18n::tr`), RichText is used as given
/// (call sites translate before styling).
pub trait Txt {
    fn txt(self) -> WidgetText;
}
impl Txt for &str {
    fn txt(self) -> WidgetText {
        tr(self).into()
    }
}
impl Txt for String {
    fn txt(self) -> WidgetText {
        tr(&self).into()
    }
}
impl Txt for &String {
    fn txt(self) -> WidgetText {
        tr(self).into()
    }
}
impl Txt for egui::RichText {
    fn txt(self) -> WidgetText {
        self.into()
    }
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
const PRIMARY: [&str; 6] = ["OK", "Start Download", "Download Now", "Yes", "Save", "Install"];

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

pub struct D<'a> {
    pub ui: &'a mut Ui,
    o: Pos2,
    /// px per DLU across: DX, or wider for pages NDM creates with their own font metrics.
    dx: f32,
    /// Right-to-left languages: every rect is mirrored across the window (left + right edge x).
    mirror: Option<f32>,
}

impl<'a> D<'a> {
    pub fn new(ui: &'a mut Ui) -> D<'a> {
        let o = ui.max_rect().min;
        let mirror = crate::i18n::is_rtl().then(|| ui.max_rect().left() + ui.max_rect().right());
        D { ui, o, dx: DX, mirror }
    }

    /// Layout whose (0,0) is at DLU (x, y) of the dialog — for property pages inside a tab control.
    pub fn new_offset(ui: &'a mut Ui, x: f32, y: f32) -> D<'a> {
        let o = ui.max_rect().min + Vec2::new(x * DX, y * DY);
        // pages sit inside an already mirrored tab control: mirror across the whole window as well
        let mirror = crate::i18n::is_rtl().then(|| ui.ctx().screen_rect().width());
        D { ui, o, dx: DX, mirror }
    }

    /// Left-to-right reading order flips with the window.
    fn row(&self) -> Layout {
        if self.mirror.is_some() { Layout::right_to_left(Align::Center) } else { Layout::left_to_right(Align::Center) }
    }

    /// Horizontal scale of this layout (`DX` by default).
    pub fn with_dx(mut self, dx: f32) -> Self {
        self.dx = dx;
        self
    }

    pub fn rect(&self, x: f32, y: f32, w: f32, h: f32) -> Rect {
        let r = Rect::from_min_size(self.o + Vec2::new(x * self.dx, y * DY), Vec2::new(w * self.dx, h * DY));
        match self.mirror {
            Some(axis) => Rect::from_min_max(Pos2::new(axis - r.max.x, r.min.y), Pos2::new(axis - r.min.x, r.max.y)),
            None => r,
        }
    }

    /// Run `f` in a child Ui confined to the DLU rect, laid out left-to-right, vertically centred.
    pub fn at<R>(&mut self, x: f32, y: f32, w: f32, h: f32, f: impl FnOnce(&mut Ui) -> R) -> R {
        let r = self.rect(x, y, w, h);
        let l = self.row();
        self.ui.scope_builder(UiBuilder::new().max_rect(r).layout(l), f).inner
    }

    pub fn label(&mut self, x: f32, y: f32, w: f32, h: f32, t: impl Txt) {
        self.at(x, y, w, h, |ui| ui.add(egui::Label::new(t.txt()).truncate().selectable(false)));
    }

    /// SS_RIGHT static text.
    pub fn label_r(&mut self, x: f32, y: f32, w: f32, h: f32, t: impl Txt) {
        let r = self.rect(x, y, w, h);
        let l = if self.mirror.is_some() { Layout::left_to_right(Align::Center) } else { Layout::right_to_left(Align::Center) };
        self.ui.scope_builder(UiBuilder::new().max_rect(r).layout(l), |ui| ui.add(egui::Label::new(t.txt()).selectable(false)));
    }

    /// Wrapping multi-line static text, top-left aligned.
    pub fn text(&mut self, x: f32, y: f32, w: f32, h: f32, t: impl Txt) {
        let r = self.rect(x, y, w, h);
        let l = Layout::top_down(if self.mirror.is_some() { Align::Max } else { Align::Min });
        self.ui.scope_builder(UiBuilder::new().max_rect(r).layout(l), |ui| ui.add(egui::Label::new(t.txt()).wrap().selectable(false)));
    }

    pub fn button(&mut self, x: f32, y: f32, w: f32, h: f32, t: &str, enabled: bool) -> bool {
        let r = self.rect(x, y, w, h);
        let primary = ndm() && PRIMARY.contains(&t);
        self.ui
            .put(r, |ui: &mut Ui| {
                ui.spacing_mut().button_padding = Vec2::new(2.0, 1.0);
                let b = if primary {
                    let th = theme(ui.visuals().dark_mode);
                    let v = ui.visuals_mut();
                    v.widgets.hovered.weak_bg_fill = th.accent_hot;
                    v.widgets.active.weak_bg_fill = th.accent_hot;
                    (v.widgets.hovered.bg_stroke, v.widgets.active.bg_stroke) = (Stroke::NONE, Stroke::NONE);
                    egui::Button::new(egui::RichText::new(tr(t)).color(th.on_accent)).fill(th.accent).stroke(Stroke::NONE)
                } else {
                    egui::Button::new(tr(t))
                };
                ui.add_enabled(enabled, b.wrap_mode(egui::TextWrapMode::Extend).min_size(r.size()))
            })
            .clicked()
    }

    pub fn edit(&mut self, x: f32, y: f32, w: f32, h: f32, s: &mut String, enabled: bool) -> Response {
        self.edit_ex(x, y, w, h, s, enabled, false)
    }

    pub fn password(&mut self, x: f32, y: f32, w: f32, h: f32, s: &mut String, enabled: bool) -> Response {
        self.edit_ex(x, y, w, h, s, enabled, true)
    }

    fn edit_ex(&mut self, x: f32, y: f32, w: f32, h: f32, s: &mut String, enabled: bool, password: bool) -> Response {
        let r = self.rect(x, y, w, h);
        let multi = h > 16.0;
        self.ui.put(r, |ui: &mut Ui| {
            let te = if multi { egui::TextEdit::multiline(s) } else { egui::TextEdit::singleline(s).password(password) };
            ui.add_enabled(enabled, te.desired_width(r.width()).min_size(r.size()).margin(egui::Margin::symmetric(3, 2)))
        })
    }

    /// Read-only edit box (ES_READONLY): selectable text on the dialog background, sunken border.
    pub fn readonly(&mut self, x: f32, y: f32, w: f32, h: f32, s: &str) {
        let r = self.rect(x, y, w, h);
        let c = pal(self.ui);
        if ndm() {
            let t = theme(self.ui.visuals().dark_mode);
            self.ui.painter().rect(r, R_CTRL, t.raised, Stroke::new(1.0_f32, c.edge), egui::StrokeKind::Inside);
        } else {
            self.ui.painter().rect_stroke(r, 0.0, Stroke::new(1.0_f32, c.edge), egui::StrokeKind::Inside);
        }
        let inner = r.shrink2(Vec2::new(4.0, 0.0));
        let l = self.row();
        self.ui.scope_builder(UiBuilder::new().max_rect(inner).layout(l), |ui| ui.add(egui::Label::new(s).truncate()));
    }

    /// Win32 drop-down combo: a field-coloured box with an arrow (egui's default is a grey button).
    pub fn combo(&mut self, x: f32, y: f32, w: f32, h: f32, id: &str, selected: &mut String, choices: &[&str], enabled: bool) -> bool {
        let before = selected.clone();
        let width = w * self.dx;
        self.at(x, y, w, h, |ui| {
            let v = ui.visuals_mut();
            let field = v.extreme_bg_color;
            for wv in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
                (wv.bg_fill, wv.weak_bg_fill) = (field, field);
            }
            ui.add_enabled_ui(enabled, |ui| {
                egui::ComboBox::from_id_salt(id).width(width - 4.0).selected_text(selected.as_str()).show_ui(ui, |ui| {
                    for c in choices {
                        ui.selectable_value(selected, c.to_string(), *c);
                    }
                });
            });
        });
        before != *selected
    }

    pub fn check(&mut self, x: f32, y: f32, w: f32, h: f32, v: &mut bool, t: &str) -> Response {
        self.at(x, y, w, h, |ui| {
            // a box two lines high wraps its text, as the dialog was laid out for
            if h >= 17.0 {
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
            }
            ui.checkbox(v, tr(t))
        })
    }

    /// A checkbox for an option this app doesn't implement: drawn, disabled.
    pub fn check_off(&mut self, x: f32, y: f32, w: f32, h: f32, t: &str) {
        self.at(x, y, w, h, |ui| ui.add_enabled(false, egui::Checkbox::new(&mut false, tr(t))));
    }

    pub fn radio(&mut self, x: f32, y: f32, w: f32, h: f32, on: bool, t: &str) -> bool {
        self.at(x, y, w, h, |ui| ui.radio(on, tr(t))).clicked()
    }

    pub fn radio_off(&mut self, x: f32, y: f32, w: f32, h: f32, t: &str) {
        self.at(x, y, w, h, |ui| ui.add_enabled(false, egui::RadioButton::new(false, tr(t))));
    }

    pub fn drag<N: egui::emath::Numeric>(&mut self, x: f32, y: f32, w: f32, h: f32, v: &mut N, lo: N, hi: N) -> Response {
        let r = self.rect(x, y, w, h);
        self.ui.put(r, egui::DragValue::new(v).range(lo..=hi))
    }

    /// BS_GROUPBOX: etched frame with the caption breaking the top edge.
    pub fn group(&mut self, x: f32, y: f32, w: f32, h: f32, t: &str) {
        let r = self.rect(x, y, w, h);
        let c = pal(self.ui);
        let frame = Rect::from_min_max(r.min + Vec2::new(0.0, 6.0), r.max);
        let p = self.ui.painter();
        if ndm() {
            // a rounded card; the caption breaks its top edge in the accent colour (the edge is left out under
            // it, so the caption needs no background of its own: groups sit on tab pages and dialogs alike)
            let stroke = Stroke::new(1.0_f32, c.edge);
            if t.is_empty() {
                p.rect_stroke(frame, R_CARD, stroke, egui::StrokeKind::Inside);
                return;
            }
            let font = egui::TextStyle::Body.resolve(self.ui.style());
            let ink = theme(self.ui.visuals().dark_mode).accent;
            let g = p.layout_no_wrap(tr(t), font, ink);
            let cap = Rect::from_min_size(r.min + Vec2::new(8.0, 0.0), g.size() + Vec2::new(8.0, 0.0));
            let clip = p.clip_rect();
            let all = frame.expand(2.0);
            for part in [
                Rect::from_min_max(all.min, egui::pos2(cap.left(), all.bottom())),
                Rect::from_min_max(egui::pos2(cap.right(), all.top()), all.max),
                Rect::from_min_max(egui::pos2(cap.left(), cap.bottom()), egui::pos2(cap.right(), all.bottom())),
            ] {
                p.with_clip_rect(part.intersect(clip)).rect_stroke(frame, R_CARD, stroke, egui::StrokeKind::Inside);
            }
            p.galley(cap.min + Vec2::new(4.0, 0.0), g, ink);
            return;
        }
        p.rect_stroke(frame, 0.0, Stroke::new(1.0_f32, c.etch_dark), egui::StrokeKind::Inside);
        if !t.is_empty() {
            let font = egui::TextStyle::Body.resolve(self.ui.style());
            let g = p.layout_no_wrap(tr(t), font, c.text);
            let tr = Rect::from_min_size(r.min + Vec2::new(8.0, 0.0), g.size() + Vec2::new(4.0, 0.0));
            p.rect_filled(tr, 0.0, c.panel);
            p.galley(tr.min + Vec2::new(2.0, 0.0), g, c.text);
        }
    }

    /// Etched horizontal line (a 1-DLU-high SS_ETCHEDHORZ static).
    pub fn hline(&mut self, x: f32, y: f32, w: f32) {
        let r = self.rect(x, y, w, 1.0);
        let c = pal(self.ui);
        let p = self.ui.painter();
        p.hline(r.x_range(), r.top(), Stroke::new(1.0_f32, c.etch_dark));
        if ndm() {
            return; // one hairline
        }
        p.hline(r.x_range(), r.top() + 1.0, Stroke::new(1.0_f32, c.etch_light));
    }

    /// Etched vertical line (SS_ETCHEDVERT).
    pub fn vline(&mut self, x: f32, y: f32, h: f32) {
        let r = self.rect(x, y, 1.0, h);
        let c = pal(self.ui);
        let p = self.ui.painter();
        p.vline(r.left(), r.y_range(), Stroke::new(1.0_f32, c.etch_dark));
        if ndm() {
            return;
        }
        p.vline(r.left() + 1.0, r.y_range(), Stroke::new(1.0_f32, c.etch_light));
    }

    /// Windows progress bar (visual-styles green).
    pub fn progress(&mut self, x: f32, y: f32, w: f32, h: f32, frac: f32) {
        let r = self.rect(x, y, w, h);
        let c = pal(self.ui);
        let p = self.ui.painter();
        if ndm() {
            // a rounded track with the accent filling it
            let t = theme(self.ui.visuals().dark_mode);
            let rad = (r.height() / 2.0).min(R_CTRL);
            p.rect_filled(r, rad, t.raised);
            let fill = Rect::from_min_size(r.min, Vec2::new(r.width() * frac.clamp(0.0, 1.0), r.height()));
            if fill.width() >= 1.0 {
                p.rect_filled(fill, rad, t.accent);
            }
            let _ = c;
            return;
        }
        // msctls_progress32 isn't darkened: NDM's bars stay light in dark mode (E6E6E6 in a BCBCBC frame)
        let _ = c;
        p.rect_filled(r, 0.0, Color32::from_gray(230));
        p.rect_stroke(r, 0.0, Stroke::new(1.0_f32, Color32::from_gray(188)), egui::StrokeKind::Inside);
        let fill = Rect::from_min_size(r.min + Vec2::splat(1.0), Vec2::new((r.width() - 2.0) * frac.clamp(0.0, 1.0), r.height() - 2.0));
        p.rect_filled(fill, 0.0, WIN_GREEN);
    }

    /// Win32 tab control. Tabs wrap into rows like a multiline tab control, with the selected
    /// row moved next to the page; an empty name forces a row break. Returns the page
    /// rect in DLU.
    pub fn tabs(&mut self, x: f32, y: f32, w: f32, h: f32, tab: &mut u8, names: &[&str]) -> (f32, f32, f32, f32) {
        let mut brk = vec![false; names.iter().filter(|n| !n.is_empty()).count()];
        let mut k = 0;
        for n in names {
            if n.is_empty() {
                if let Some(b) = brk.get_mut(k) {
                    *b = true; // break before tab k
                }
            } else {
                k += 1;
            }
        }
        let names: Vec<String> = names.iter().filter(|n| !n.is_empty()).map(|n| tr(n)).collect();
        let c = pal(self.ui);
        let font = egui::TextStyle::Body.resolve(self.ui.style());
        let origin = self.rect(x, y, w, 0.0);
        let widths: Vec<f32> = names.iter().map(|n| self.ui.painter().layout_no_wrap(n.clone(), font.clone(), c.text).size().x + 14.0).collect();
        let mut rows: Vec<Vec<usize>> = vec![vec![]];
        let mut used = 2.0;
        for (k, tw) in widths.iter().enumerate() {
            if (brk[k] || used + tw > origin.width() - 2.0) && !rows.last().unwrap().is_empty() {
                rows.push(vec![]);
                used = 2.0;
            }
            rows.last_mut().unwrap().push(k);
            used += tw - 1.0;
        }
        let last = rows.len() - 1;
        let sel_row = rows.iter().position(|r| r.contains(&(*tab as usize))).unwrap_or(0);
        rows.swap(sel_row, last);
        let row_h = 13.0 * DY;
        let head = rows.len() as f32 * 13.0;
        let body = self.rect(x, y + head, w, h - head);
        let mut active = None;
        for (ri, row) in rows.iter().enumerate() {
            let row_top = origin.top() + ri as f32 * row_h;
            let mut left = origin.left() + 2.0;
            let natural: f32 = row.iter().map(|k| widths[*k] - 1.0).sum();
            let stretch = if rows.len() > 1 { (origin.width() - 4.0) / natural } else { 1.0 };
            for k in row {
                let tw = (widths[*k] - 1.0) * stretch + 1.0;
                let sel = *tab as usize == *k;
                let top = row_top + if sel { 0.0 } else { 2.0 };
                let bottom = if ri == last { body.top() + if sel { 1.0 } else { 0.0 } } else { row_top + row_h };
                let mut r = Rect::from_min_max(egui::pos2(left, top), egui::pos2(left + tw, bottom));
                if self.mirror.is_some() {
                    // right-to-left: the first tab at the right edge
                    let axis = origin.left() + origin.right();
                    r = Rect::from_min_max(egui::pos2(axis - r.max.x, r.min.y), egui::pos2(axis - r.min.x, r.max.y));
                }
                let resp = self.ui.interact(r, self.ui.id().with(("tab", k)), Sense::click());
                if resp.clicked() {
                    *tab = *k as u8;
                }
                if sel {
                    active = Some(r);
                }
                let p = self.ui.painter();
                if ndm() {
                    // flat tabs: the selected one in the accent colour, underlined
                    let t = theme(self.ui.visuals().dark_mode);
                    let full = Rect::from_min_max(egui::pos2(r.left(), row_top), egui::pos2(r.right(), row_top + row_h - 1.0));
                    if resp.hovered() && !sel {
                        p.rect_filled(full.shrink2(Vec2::new(1.0, 2.0)), R_CTRL, t.raised);
                    }
                    let ink = if sel { t.accent } else { t.muted };
                    p.text(egui::pos2(r.center().x, row_top + (row_h - 2.0) / 2.0), egui::Align2::CENTER_CENTER, &names[*k], font.clone(), ink);
                    if sel {
                        p.rect_filled(Rect::from_min_max(egui::pos2(full.left() + 3.0, full.bottom() - 2.0), egui::pos2(full.right() - 3.0, full.bottom())), 1.0, t.accent);
                    }
                    left += tw - 1.0;
                    continue;
                }
                let fill = if sel { c.field } else if resp.hovered() { c.tab_hot } else { c.tab };
                p.rect_filled(r, 0.0, fill);
                p.rect_stroke(r, 0.0, Stroke::new(1.0_f32, c.line), egui::StrokeKind::Inside);
                p.text(egui::pos2(r.center().x, top + (row_h - 2.0) / 2.0), egui::Align2::CENTER_CENTER, &names[*k], font.clone(), c.text);
                left += tw - 1.0;
            }
        }
        let p = self.ui.painter();
        if ndm() {
            p.rect(body, R_CARD, c.field, Stroke::new(1.0_f32, c.line), egui::StrokeKind::Inside);
            return (x + 4.0, y + head + 3.0, w - 8.0, h - head - 6.0);
        }
        p.rect_filled(body, 0.0, c.field);
        p.rect_stroke(body, 0.0, Stroke::new(1.0_f32, c.line), egui::StrokeKind::Inside);
        if let Some(a) = active {
            p.hline(a.x_range().shrink(1.0), body.top(), Stroke::new(1.0_f32, c.field));
        }
        (x + 4.0, y + head + 3.0, w - 8.0, h - head - 6.0)
    }

    /// Property page header: 32 px icon at (7,3), title text, etched line under it.
    pub fn page_header(&mut self, icon: Option<egui::TextureId>, tx: f32, title: &str) {
        if let Some(t) = icon {
            let r = self.rect(7.0, 3.0, 20.0, 20.0);
            self.ui.put(Rect::from_min_size(r.min, Vec2::splat(32.0)), egui::Image::new((t, Vec2::splat(32.0))));
        }
        // NDM's title static is right-aligned and ends at 255 DLU; the etched line ends at 263
        self.label_r(tx, 7.0, 255.0 - tx, 11.0, title);
        self.hline(tx - 14.0, 19.0, 277.0 - tx);
    }

    /// A list-view box with a header row; returns the inner rect for rows.
    /// px per DLU across, as this dialog lays out.
    pub fn dx(&self) -> f32 {
        self.dx
    }

    pub fn listview(&mut self, x: f32, y: f32, w: f32, h: f32, cols: &[(&str, f32)]) -> Rect {
        let r = self.rect(x, y, w, h);
        let c = pal(self.ui);
        let font = egui::TextStyle::Body.resolve(self.ui.style());
        let p = self.ui.painter();
        let (rad, ndm) = if ndm() { (R_CTRL, Some(theme(self.ui.visuals().dark_mode))) } else { (0.0, None) };
        p.rect(r, rad, c.list, Stroke::new(1.0_f32, c.edge), egui::StrokeKind::Inside);
        if cols.is_empty() {
            return r.shrink(1.0); // tree view: no header
        }
        let hh = 20.0;
        if let Some(t) = &ndm {
            let head = Rect::from_min_size(r.min + Vec2::splat(1.0), Vec2::new(r.width() - 2.0, hh - 1.0));
            p.rect_filled(head, egui::CornerRadius { nw: rad as u8, ne: rad as u8, sw: 0, se: 0 }, t.raised);
        }
        let ink = ndm.as_ref().map_or(c.text, |t| t.muted);
        let mut cx = r.left() + 1.0;
        for (name, cw) in cols {
            let cr = Rect::from_min_size(egui::pos2(cx, r.top() + 1.0), Vec2::new(cw * self.dx, hh));
            p.vline(cr.right() - 1.0, cr.y_range().shrink(3.0), Stroke::new(1.0_f32, c.line));
            p.text(cr.left_center() + Vec2::new(6.0, 0.0), egui::Align2::LEFT_CENTER, tr(name), font.clone(), ink);
            cx += cw * self.dx;
        }
        p.hline(r.x_range().shrink(1.0), r.top() + hh, Stroke::new(1.0_f32, c.line));
        Rect::from_min_max(egui::pos2(r.left() + 1.0, r.top() + hh + 1.0), r.max - Vec2::splat(1.0))
    }
}
