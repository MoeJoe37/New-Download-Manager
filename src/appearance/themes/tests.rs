use super::{
    color,
    load::{create, install, load},
    model::Base,
    render::patch,
};
use eframe::egui::{self, Color32, Style};
use serde_json::json;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "ndm-theme-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
    fn file(&self, value: serde_json::Value) -> PathBuf {
        let p = self.0.join("theme.json");
        std::fs::write(&p, serde_json::to_vec(&value).unwrap()).unwrap();
        p
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn partial_theme_inherits_and_overrides_only_selected_fields() {
    let t = Temp::new();
    let theme=load(&t.file(json!({"version":1,"name":"Partial","base":"modern",
        "common":{"style":{"spacing":{"button_padding":{"x":15.0}},"visuals":{"panel_fill":"#123456"}}},
        "dark":{"style":{"visuals":{"panel_fill":"#20304080"}}}}))).unwrap();
    assert_eq!(theme.file.base, Base::Modern);
    let base = Style::default();
    let mut light = base.clone();
    for l in theme.layers(false) {
        patch(&mut light, &l.style).unwrap();
    }
    assert_eq!(light.spacing.button_padding.x, 15.0);
    assert_eq!(
        light.spacing.button_padding.y,
        base.spacing.button_padding.y
    );
    assert_eq!(
        light.visuals.panel_fill,
        Color32::from_rgb(0x12, 0x34, 0x56)
    );
    assert_eq!(light.visuals.window_fill, base.visuals.window_fill);
    let mut dark = base.clone();
    for l in theme.layers(true) {
        patch(&mut dark, &l.style).unwrap();
    }
    assert_eq!(dark.visuals.panel_fill, color("#20304080").unwrap());
    assert_eq!(dark.spacing.button_padding.x, 15.0);
}

#[test]
fn theme_import_copies_referenced_assets_and_survives_source_removal() {
    let t = Temp::new();
    let source = t.0.join("source");
    std::fs::create_dir_all(source.join("icons")).unwrap();
    let png = include_bytes!("../../../assets/ndm.png");
    std::fs::write(source.join("background.data"), png).unwrap();
    std::fs::write(source.join("icons/add-url.png"), png).unwrap();
    std::fs::write(
        source.join("unrelated-private-file.txt"),
        "must not be copied",
    )
    .unwrap();
    let fonts = egui::FontDefinitions::default();
    std::fs::write(
        source.join("font.ttf"),
        fonts.font_data.values().next().unwrap().font.as_ref(),
    )
    .unwrap();
    let json = json!({"version":1,"name":"Portable","fonts":{"proportional":"font.ttf"},
        "toolbar_icons":"icons","common":{"regions":{"toolbar":{"image":"background.data","image_mode":"cover","image_opacity":0.5}}}});
    std::fs::write(
        source.join("theme.json"),
        serde_json::to_vec(&json).unwrap(),
    )
    .unwrap();
    let installed = t.0.join("installed");
    let (folder, theme) = install(&source.join("theme.json"), &installed).unwrap();
    assert_eq!(theme.file.name, "Portable");
    assert_eq!(theme.images.len(), 1);
    assert_eq!(theme.fonts.len(), 1);
    assert!(installed.join(&folder).join("icons/add-url.png").is_file());
    assert!(!installed
        .join(&folder)
        .join("unrelated-private-file.txt")
        .exists());
    std::fs::remove_dir_all(source).unwrap();
    assert!(load(&installed.join(folder).join("theme.json")).is_ok());
}

#[test]
fn invalid_themes_are_rejected_before_installation() {
    let t = Temp::new();
    for patch in [
        json!({"version":2,"name":"Bad"}),
        json!({"version":1,"name":"Bad","unknown":true}),
        json!({"version":1,"name":"Bad","common":{"palette":{"accent":"orange"}}}),
        json!({"version":1,"name":"Bad","common":{"palette":{"accnet":"#123456"}}}),
        json!({"version":1,"name":"Bad","common":{"style":{"visuals":{"unknown_fill":"#123456"}}}}),
        json!({"version":1,"name":"Bad","common":{"style":{"text_styles":{"Body":{"size":0}}}}}),
        json!({"version":1,"name":"Bad","common":{"style":{"override_font_id":{"size":12,"family":"Proportional","typo":1}}}}),
        json!({"version":1,"name":"Bad","common":{"regions":{"toolbar":{"image_opacity":2}}}}),
        json!({"version":1,"name":"Bad","common":{"regions":{"toolbar":{"image":"../outside.png"}}}}),
        json!({"version":1,"name":"Bad","fonts":{"proportional":"missing.ttf"}}),
        json!({"version":1,"name":"Bad","common":{"palette":{"categories.":"#123456"}}}),
        json!({"version":1,"name":"Bad","common":{"palette":{"categories.vi deo":"#123456"}}}),
    ] {
        let path = t.file(patch);
        assert!(
            load(&path).is_err(),
            "accepted {}",
            std::fs::read_to_string(&path).unwrap()
        );
        assert!(install(&path, &t.0.join("installed")).is_err());
        assert!(
            !t.0.join("installed").exists(),
            "failed validation must not install anything"
        );
    }
}

#[test]
fn theme_template_contains_valid_partial_theme_and_style_references() {
    let t = Temp::new();
    let style = Style::default();
    let a = create(&t.0, Base::Ndm, [&style, &style]).unwrap();
    let b = create(&t.0, Base::Modern, [&style, &style]).unwrap();
    assert_ne!(a, b);
    assert!(load(&a.join("theme.json")).is_ok());
    let reference: serde_json::Value =
        serde_json::from_slice(&std::fs::read(a.join("style-reference-light.json")).unwrap())
            .unwrap();
    assert!(reference["visuals"]["widgets"]["hovered"].is_object());
    assert!(reference["text_styles"]["Body"].is_object());
    assert_eq!(
        reference["spacing"]["button_padding"],
        json!({"x":4.0,"y":1.0})
    );
}

#[test]
#[cfg(unix)]
fn theme_assets_cannot_escape_through_symlinks() {
    let t = Temp::new();
    let theme_dir = t.0.join("theme");
    std::fs::create_dir(&theme_dir).unwrap();
    std::fs::write(
        t.0.join("outside.png"),
        include_bytes!("../../../assets/ndm.png"),
    )
    .unwrap();
    std::os::unix::fs::symlink(t.0.join("outside.png"), theme_dir.join("inside.png")).unwrap();
    let json =
        json!({"version":1,"name":"Outside","common":{"regions":{"app":{"image":"inside.png"}}}});
    std::fs::write(
        theme_dir.join("theme.json"),
        serde_json::to_vec(&json).unwrap(),
    )
    .unwrap();
    assert!(load(&theme_dir.join("theme.json")).is_err());
}

#[test]
fn bundled_theme_and_readme_examples_load() {
    let t = Temp::new();
    for dir in ["images", "fonts", "toolbar", "categories"] {
        std::fs::create_dir_all(t.0.join(dir)).unwrap();
    }
    std::fs::write(
        t.0.join("images/toolbar.png"),
        include_bytes!("../../../assets/ndm.png"),
    )
    .unwrap();
    let fonts = egui::FontDefinitions::default();
    std::fs::write(
        t.0.join("fonts/MyFont.ttf"),
        fonts.font_data.values().next().unwrap().font.as_ref(),
    )
    .unwrap();
    let readme = include_str!("../../../README.md").replace("\r\n", "\n");
    let guide = readme
        .split("## Custom themes\n")
        .nth(1)
        .unwrap()
        .split("## Custom toolbars\n")
        .next()
        .unwrap();
    let mut examples = 0;
    for block in guide.split("```json\n").skip(1) {
        let json = block.split("```").next().unwrap();
        std::fs::write(t.0.join("theme.json"), json).unwrap();
        load(&t.0.join("theme.json"))
            .unwrap_or_else(|e| panic!("README theme example failed: {e}\n{json}"));
        examples += 1;
    }
    assert_eq!(examples, 4);
    std::fs::write(
        t.0.join("theme.json"),
        include_bytes!("../../../examples/themes/Aurora/theme.json"),
    )
    .unwrap();
    assert_eq!(load(&t.0.join("theme.json")).unwrap().file.name, "Aurora");
}

/// NDM Style's category tree icons can be tinted per item, as the toolbar's can.
#[test]
fn category_icon_colors_are_palette_keys() {
    let t = Temp::new();
    let theme = load(&t.file(json!({"version":1,"name":"Neon","common":{"palette":{
        "categories.video":"#112233","categories.queue-running":"#445566","categories.my-files":"#778899"}}})));
    assert!(theme.is_ok(), "{:?}", theme.err());
}
