use super::*;
use egui::{Event, Modifiers, PointerButton, Pos2, Rect, Vec2};
use std::collections::HashMap;

#[derive(Clone)]
struct Label {
    rect: Rect,
    font: f32,
    clip: Rect,
}

struct Rendered {
    labels: HashMap<String, Label>,
    popups: HashMap<&'static str, Rect>,
}

struct Scene {
    ctx: egui::Context,
    time: f64,
    choice: usize,
}

impl Scene {
    fn new(dpi: f32) -> Self {
        let ctx = egui::Context::default();
        ctx.set_pixels_per_point(dpi);
        ctx.options_mut(|o| o.warn_on_id_clash = false);
        Self {
            ctx,
            time: 0.0,
            choice: 0,
        }
    }

    fn frame(&mut self, scale: f32, events: Vec<Event>) -> Rendered {
        set_popup_scale(&self.ctx, scale);
        self.time += 1.0 / 60.0;
        let mut popups = HashMap::new();
        let output = self.ctx.run(
            egui::RawInput {
                time: Some(self.time),
                events,
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(900.0, 700.0))),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    egui::menu::bar(ui, |ui| {
                        ui.menu_button("Menu", |ui| {
                            scale_popup(ui);
                            let _ = ui.button("Action one");
                            let _ = ui.button("Action two");
                            ui.menu_button("More", |ui| {
                                scale_popup(ui);
                                let _ = ui.button("Nested action");
                                popups.insert("nested", ui.min_rect());
                            });
                            popups.insert("menu", ui.min_rect());
                        });
                    });
                    egui::ComboBox::from_id_salt("scaled-combo")
                        .selected_text("Choice one")
                        .show_ui(ui, |ui| {
                            scale_popup(ui);
                            ui.selectable_value(&mut self.choice, 0, "Choice one");
                            ui.selectable_value(&mut self.choice, 1, "Choice two");
                            popups.insert("combo", ui.min_rect());
                        });
                });
            },
        );
        fn labels(shape: &egui::Shape, clip: Rect, out: &mut HashMap<String, Label>) {
            match shape {
                egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| labels(s, clip, out)),
                egui::Shape::Text(text) => {
                    out.insert(
                        text.galley.text().to_string(),
                        Label {
                            rect: text.galley.rect.translate(text.pos.to_vec2()),
                            font: text.galley.job.sections[0].format.font_id.size,
                            clip,
                        },
                    );
                }
                _ => {}
            }
        }
        let mut text = HashMap::new();
        for shape in output.shapes {
            labels(&shape.shape, shape.clip_rect, &mut text);
        }
        Rendered {
            labels: text,
            popups,
        }
    }

    fn settled(&mut self, scale: f32) -> Rendered {
        self.frame(scale, vec![]);
        self.frame(scale, vec![]);
        self.frame(scale, vec![])
    }

    fn click(&mut self, scale: f32, label: &str) {
        let at = self.settled(scale).labels[label].rect.center();
        self.frame(scale, vec![Event::PointerMoved(at)]);
        for pressed in [true, false] {
            self.frame(
                scale,
                vec![Event::PointerButton {
                    pos: at,
                    button: PointerButton::Primary,
                    pressed,
                    modifiers: Modifiers::NONE,
                }],
            );
        }
    }
}

fn assert_scaled(rendered: &Rendered, text: &str, scale: f32, base_font: f32) {
    let label = &rendered.labels[text];
    assert!(
        (label.font - base_font * scale).abs() < 0.01,
        "{text}: {} at {scale}",
        label.font
    );
    assert!(
        label.clip.expand(1.0).contains_rect(label.rect),
        "{text} is clipped at {scale}: label={:?}, clip={:?}, popups={:?}",
        label.rect,
        label.clip,
        rendered.popups
    );
}

#[test]
fn popup_menu_scaling_tracks_live_changes_without_compounding() {
    for dpi in [1.0, 2.0] {
        let mut scene = Scene::new(dpi);
        let font = egui::TextStyle::Button.resolve(&scene.ctx.style()).size;
        scene.click(1.0, "Menu");
        let initial = scene.settled(1.0).popups["menu"];
        for scale in [2.0, 0.5, 1.5, 1.0] {
            let rendered = scene.settled(scale);
            assert_scaled(&rendered, "Action two", scale, font);
            let size = rendered.popups["menu"].size();
            if scale > 1.0 {
                assert!(size.x > initial.width() * 1.3 && size.y > initial.height() * 1.3);
            }
            if scale < 1.0 {
                assert!(size.x < initial.width() * 0.8 && size.y < initial.height() * 0.8);
            }
            if scale == 1.0 {
                assert!(
                    (size - initial.size()).length() < 2.0,
                    "reset did not restore menu size"
                );
            }
            assert_eq!(
                egui::TextStyle::Button.resolve(&scene.ctx.style()).size,
                font
            );
        }
        // Each nested menu starts a fresh Area; it must receive the scale once too.
        for scale in [0.5, 1.0, 2.0] {
            let mut nested = Scene::new(dpi);
            nested.click(scale, "Menu");
            nested.click(scale, "More");
            let rendered = nested.settled(scale);
            assert_scaled(&rendered, "Nested action", scale, font);
        }
    }
}

#[test]
fn popup_combo_scaling_grows_labels_and_bounds() {
    for dpi in [1.0, 2.0] {
        let mut scene = Scene::new(dpi);
        let font = egui::TextStyle::Button.resolve(&scene.ctx.style()).size;
        scene.click(1.0, "Choice one");
        let initial = scene.settled(1.0).popups["combo"];
        for scale in [2.0, 0.5, 1.5, 1.0] {
            let rendered = scene.settled(scale);
            assert_scaled(&rendered, "Choice one", scale, font);
            let second = &rendered.labels["Choice two"];
            assert!((second.font - font * scale).abs() < 0.01);
            assert!(second.rect.right() <= second.clip.right() + 1.0);
            let size = rendered.popups["combo"].size();
            if scale > 1.0 {
                assert!(size.x > initial.width() * 1.3 && size.y > initial.height() * 1.3);
            }
            if scale < 1.0 {
                assert!(size.x < initial.width() * 0.8 && size.y < initial.height() * 0.8);
            }
            if scale == 1.0 {
                assert!(
                    (size - initial.size()).length() < 2.0,
                    "reset did not restore combo size"
                );
            }
        }
        // ComboBox limits its viewport height and scrolls taller content. Enlarged
        // rows must still be reachable and selecting one must close the popup.
        scene.settled(2.0);
        let at = scene.settled(2.0).labels["Choice one"].rect.center();
        scene.frame(
            2.0,
            vec![
                Event::PointerMoved(at),
                Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: Vec2::new(0.0, -200.0),
                    modifiers: Modifiers::NONE,
                },
            ],
        );
        for _ in 0..30 {
            scene.frame(2.0, vec![]);
        }
        assert_scaled(&scene.settled(2.0), "Choice two", 2.0, font);
        scene.click(2.0, "Choice two");
        assert_eq!(scene.choice, 1);
        assert!(!scene.settled(2.0).popups.contains_key("combo"));
    }
}
