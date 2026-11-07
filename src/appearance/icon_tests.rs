use super::{ico, import, pixels, png, read_image, square, stored};
use image::{ImageFormat, Rgba, RgbaImage};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "ndm-icon-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn icon_conversion_detects_content_and_keeps_transparency() {
    let t = Temp::new();
    let source = t.0.join("picture.wrong-extension");
    let original = RgbaImage::from_pixel(80, 40, Rgba([180, 60, 20, 128]));
    std::fs::write(&source, png(&original).unwrap()).unwrap();
    let name = import(&t.0, &source, true).unwrap();
    let output = stored(&t.0, &name).unwrap();
    let converted = read_image(&output, 256).unwrap();
    assert_eq!(converted.dimensions(), (256, 256));
    assert_eq!(converted.get_pixel(0, 0).0, [0, 0, 0, 0]);
    assert_eq!(converted.get_pixel(128, 128).0, [180, 60, 20, 128]);
    let windows = std::fs::read(output.with_extension("ico")).unwrap();
    assert_eq!(&windows[..6], &[0, 0, 1, 0, 7, 0]);
    let decoded = image::load_from_memory_with_format(&windows, ImageFormat::Ico).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (256, 256));
    assert_eq!(
        import(&t.0, &source, true).unwrap(),
        name,
        "same content has a stable icon name"
    );
    std::fs::remove_file(&source).unwrap();
    assert!(
        read_image(&output, 256).is_ok(),
        "import must not depend on the source file"
    );
    let _ = pixels(converted);
}

#[test]
fn icon_conversion_accepts_jpeg_svg_and_icon_files() {
    let t = Temp::new();
    let rgb = image::RgbImage::from_pixel(48, 24, image::Rgb([200, 50, 25]));
    let source = t.0.join("image.bin");
    rgb.save_with_format(&source, ImageFormat::Jpeg).unwrap();
    assert!(import(&t.0, &source, false).is_ok());
    let source = t.0.join("vector.data");
    std::fs::write(&source,r##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="12"><rect width="24" height="12" fill="#c81432" fill-opacity="0.5"/></svg>"##).unwrap();
    let name = import(&t.0, &source, true).unwrap();
    let i = read_image(&stored(&t.0, &name).unwrap(), 256).unwrap();
    let [r, g, b, a] = i.get_pixel(128, 128).0;
    assert!((125..=130).contains(&a));
    assert!(
        (195..=205).contains(&r) && (15..=25).contains(&g) && (45..=55).contains(&b),
        "SVG premultiplied colors changed: {r} {g} {b} {a}"
    );
    let source = t.0.join("icon.bin");
    std::fs::write(&source, ico(&square(&i, 256)).unwrap()).unwrap();
    assert!(import(&t.0, &source, false).is_ok());
}

#[test]
fn invalid_icon_does_not_replace_prepared_icons() {
    let t = Temp::new();
    let source = t.0.join("valid.png");
    std::fs::write(
        &source,
        png(&RgbaImage::from_pixel(2, 2, Rgba([20, 30, 40, 255]))).unwrap(),
    )
    .unwrap();
    let name = import(&t.0, &source, true).unwrap();
    let path = stored(&t.0, &name).unwrap();
    let before = std::fs::read(&path).unwrap();
    std::fs::write(&source, b"not an image").unwrap();
    assert!(import(&t.0, &source, true).is_err());
    assert_eq!(std::fs::read(path).unwrap(), before);
    assert!(stored(&t.0, "../state.json").is_none());
    assert!(stored(&t.0, "/tmp/outside.png").is_none());
    let svg = t.0.join("external.svg");
    std::fs::write(&svg,r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24"><image href="../private.png"/></svg>"#).unwrap();
    assert!(read_image(&svg, 256).is_err());
}
