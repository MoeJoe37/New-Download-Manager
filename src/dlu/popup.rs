//! Popup areas use the context style, so they need the main window's text scale explicitly.
use super::egui;

#[derive(Clone, Copy)]
struct PopupBasis {
    width: f32,
    button_padding: egui::Vec2,
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
    let basis = ui
        .data(|data| data.get_temp::<PopupBasis>(id))
        .unwrap_or_else(|| PopupBasis {
            width: if ui.available_width().is_finite() {
                ui.available_width()
            } else {
                ui.spacing().menu_width
            },
            button_padding: ui.spacing().button_padding,
        });
    ui.data_mut(|data| data.insert_temp(id, basis));
    let base = ui.ctx().style();
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
    // Reset the available width too, so an already-open menu can shrink after a live scale change.
    ui.set_width(basis.width * scale);
}

#[cfg(test)]
#[path = "popup_tests.rs"]
mod tests;
