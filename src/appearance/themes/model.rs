use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Base {
    Modern,
    #[default]
    Ndm,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ThemeFile {
    pub version: u32,
    pub name: String,
    #[serde(default)]
    pub base: Base,
    #[serde(default)]
    pub common: Layer,
    #[serde(default)]
    pub light: Layer,
    #[serde(default)]
    pub dark: Layer,
    /// Family -> TTF/OTF file. Special family names: proportional, monospace, segoe, list.
    #[serde(default)]
    pub fonts: BTreeMap<String, String>,
    #[serde(default)]
    pub toolbar_icons: Option<String>,
    #[serde(default)]
    pub category_icons: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Layer {
    pub palette: BTreeMap<String, String>,
    pub metrics: BTreeMap<String, f32>,
    pub style: BTreeMap<String, Value>,
    pub regions: BTreeMap<String, Region>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Region {
    pub fill: Option<String>,
    pub image: Option<String>,
    pub image_mode: Option<ImageMode>,
    pub image_opacity: Option<f32>,
    pub border: Option<String>,
    pub border_width: Option<f32>,
    pub corner_radius: Option<f32>,
    pub margin: Option<[i8; 4]>,
    pub style: BTreeMap<String, Value>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ImageMode {
    #[default]
    Cover,
    Contain,
    Stretch,
    Tile,
}

pub(crate) const REGIONS: &[&str] = &[
    "app", "menu", "toolbar", "sidebar", "list", "status", "dialog", "grabber", "help", "popup",
];
pub(crate) const PALETTE: &[&str] = &[
    "bg",
    "surface",
    "raised",
    "border",
    "text",
    "muted",
    "accent",
    "accent_hot",
    "accent_soft",
    "on_accent",
    "danger",
    "success",
];
pub(crate) const METRICS: &[&str] = &[
    "control_radius",
    "card_radius",
    "menu_font",
    "toolbar_font",
    "categories_font",
    "list_font",
    "status_font",
    "toolbar_icon_size",
    "list_row_height",
];
