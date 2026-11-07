//! Popup areas use the context style, so they need the main window's text scale explicitly.
use super::egui;

#[derive(Clone, Copy)]
struct PopupBasis {
    width: f32,
    button_padding: egui::Vec2,
    scale: f32,
}

pub fn set_popup_scale(ctx: &egui::Context, scale: f32) {
    let scale = if scale.is_finite() {
        scale.clamp(0.5, 2.0)
    } else {
        1.0
    };
    ctx.data_mut(|data| data.insert_temp(egui::Id::new("ndm-popup-text-scale"), scale));
}

/// Use unscaled context metrics each frame: nested menus and repeated calls must not compound scaling.
pub fn scale_popup(ui: &mut egui::Ui) {
    let scale = ui
        .data(|data| data.get_temp::<f32>(egui::Id::new("ndm-popup-text-scale")))
        .unwrap_or(1.0);
    let id = ui.id().with("ndm-popup-basis");
    let previous = ui.data(|data| data.get_temp::<PopupBasis>(id));
    let mut basis = previous.unwrap_or_else(|| PopupBasis {
        width: if ui.available_width().is_finite() {
            ui.available_width()
        } else {
            ui.spacing().menu_width
        },
        button_padding: ui.spacing().button_padding,
        scale,
    });
    basis.scale = scale;
    ui.data_mut(|data| data.insert_temp(id, basis));
    let mut base = (*ui.ctx().style()).clone();
    crate::appearance::themes::region_style(&mut base, "popup", ui.visuals().dark_mode);
    let style = ui.style_mut();
    style.text_styles = base
        .text_styles
        .iter()
        .map(|(text_style, font)| {
            let mut font = font.clone();
            font.size *= scale;
            (text_style.clone(), font)
        })
        .collect();
    style.override_font_id = base.override_font_id.clone().map(|mut font| {
        font.size *= scale;
        font
    });
    style.wrap_mode = Some(egui::TextWrapMode::Extend);
    let spacing = &mut style.spacing;
    spacing.button_padding = basis.button_padding * scale;
    spacing.interact_size = base.spacing.interact_size * scale;
    spacing.item_spacing = base.spacing.item_spacing * scale;
    spacing.icon_width = base.spacing.icon_width * scale;
    spacing.icon_width_inner = base.spacing.icon_width_inner * scale;
    spacing.icon_spacing = base.spacing.icon_spacing * scale;
    spacing.indent = base.spacing.indent * scale;
    spacing.menu_width = base.spacing.menu_width * scale;
    if ui.is_sizing_pass() {
        // egui measures natural content widths in this pass. A scaled minimum
        // would instead stretch every menu to its initial available width.
        ui.set_max_width(basis.width * scale);
    } else if let Some(previous) = previous.filter(|previous| previous.scale != scale) {
        let anchored = ui
            .stack()
            .iter()
            .any(|stack| stack.kind() == Some(egui::UiKind::Popup));
        // ComboBox popups already have their field's minimum width. Ordinary
        // menus resize from their measured content, including when scaling down.
        let width = if anchored {
            basis.width
        } else {
            ui.available_width() * scale / previous.scale
        };
        ui.set_width(width);
    }
    crate::appearance::themes::background(ui, "popup");
}

#[cfg(test)]
#[path = "popup_tests.rs"]
mod tests;
