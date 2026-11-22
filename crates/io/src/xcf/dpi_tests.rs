use super::tests::{TestImage, TestLayer, W};
use super::*;

/// Author source pixel-sized text and a physical-sized box, so both conversions must use document DPI.
fn text_at_resolution(properties: Vec<Vec<u8>>) -> ImportResult {
    import_text(
        properties,
        br#"(text "Ag") (font "Inter") (font-size 24) (font-size-unit pixels) (box-mode fixed) (box-unit inches) (box-width 0.5) (box-height 0.25)"#,
    )
}

fn import_text(properties: Vec<Vec<u8>>, config: &[u8]) -> ImportResult {
    let mut source = TestImage::rgb8(320, 200);
    source.image_props.extend(properties.into_iter().map(|payload| (19, payload)));
    let mut parasite = W { out: Vec::new(), wide: true };
    parasite.string("gimp-text-layer");
    parasite.u32(0);
    parasite.u32(config.len() as u32);
    parasite.out.extend_from_slice(config);
    let mut layer = TestLayer::rgba("DPI text", 1, 1, vec![255, 0, 0, 255]);
    layer.props.push((26, 0u32.to_be_bytes().to_vec()));
    layer.props.push((21, parasite.out));
    source.layers.push(layer);
    import("resolution.xcf", &source.write(), &Interrupt::NONE).unwrap()
}

fn pair(x: f32, y: f32) -> Vec<u8> {
    [x.to_be_bytes(), y.to_be_bytes()].concat()
}

fn assert_matching_resolution(properties: Vec<Vec<u8>>, dpi: f32) {
    let imported = text_at_resolution(properties);
    let document = imported.document;
    assert_eq!(document.resolution_dpi, dpi);
    let LayerContent::Text(text) = &document.layers[0].content else { panic!("editable source text") };
    assert!((text.size_pt - 24. * 72. / dpi).abs() < 1e-5);
    assert!((text.runs[0].style.size_pt * dpi / 72. - 24.).abs() < 1e-4, "source pixel-sized text must stay 24 native pixels");
    let photocraft_doc::text::TextShape::Box { width, height, .. } = text.shape else { panic!("fixed source box") };
    assert!((width - 0.5 * dpi).abs() < 1e-4 && (height - 0.25 * dpi).abs() < 1e-4, "physical box dimensions must use the final document DPI");
}

#[test]
fn malformed_resolution_pairs_keep_default_dpi_for_text_and_physical_boxes() {
    assert_matching_resolution(Vec::new(), 72.);
    for malformed in [pair(300., f32::NAN), pair(300., f32::INFINITY), pair(-1., 300.), pair(1.0e6, 300.), 300f32.to_be_bytes().to_vec()] {
        assert_matching_resolution(vec![malformed], 72.);
    }
}

#[test]
fn duplicate_resolution_properties_use_the_last_valid_pair_for_all_native_units() {
    for (properties, expected) in [
        (vec![pair(300., 300.), pair(144., 144.)], 144.),
        (vec![pair(300., 300.), pair(144., f32::NAN)], 300.),
        (vec![pair(300., f32::NAN), pair(144., 144.)], 144.),
        (vec![pair(72., 72.), pair(300., f32::NAN)], 72.),
    ] {
        assert_matching_resolution(properties, expected);
    }
}

#[test]
fn unequal_resolution_keeps_editable_physical_boxes_and_saved_pixels_through_native_and_psd_roundtrips() {
    let imported = text_at_resolution(vec![pair(144., 72.), pair(288., f32::NAN)]);
    let doc = imported.document;
    assert_eq!(doc.resolution_dpi, 144.);
    let check_text = |doc: &Document| {
        let LayerContent::Text(text) = &doc.layers[0].content else { panic!("expected editable text") };
        assert_eq!(text.text, "Ag");
        assert_eq!(text.size_pt, 12.);
        assert_eq!(text.transform.m, [2., 0., 0., 1., 0., 0.]);
        let photocraft_doc::text::TextShape::Box { width, height, .. } = text.shape else { panic!("expected fixed text box") };
        assert_eq!((width * 2., height), (72., 18.));
        assert_eq!(text.cache.as_ref().unwrap().pixel(0, 0), [1., 0., 0., 1.]);
    };
    check_text(&doc);
    let saved = photocraft_format::save_to_bytes(&doc, &Default::default()).unwrap();
    check_text(&photocraft_format::load_from_bytes(&saved).unwrap());
    let exported = crate::export(&doc, "editable.psd", &Default::default()).unwrap();
    check_text(&crate::import("editable.psd", &exported.bytes).unwrap().document);
    assert!(imported.warnings.iter().any(|warning| warning.contains("vertical resolution")));
    assert!(imported.warnings.iter().any(|warning| warning.contains("editable native text")));
}

#[test]
fn unequal_resolution_pixel_boxes_do_not_resize_and_use_native_wrapping_on_edit() {
    let config = br#"(text "HHHH HHHH HHHH HHHH") (font "monospace") (font-size 20) (font-size-unit pixels) (box-mode fixed) (box-unit pixels) (box-width 145) (box-height 160)"#;
    let equal = import_text(vec![pair(72., 72.)], config);
    let mut wide = import_text(vec![pair(144., 72.)], config);
    let tall = import_text(vec![pair(72., 144.)], config);
    for (doc, scale) in [(&equal.document, 1.), (&wide.document, 2.), (&tall.document, 0.5)] {
        let LayerContent::Text(t) = &doc.layers[0].content else { panic!("expected editable text") };
        let photocraft_doc::text::TextShape::Box { width, height, .. } = t.shape else { panic!("expected fixed text box") };
        assert_eq!((width * scale, height), (145., 160.));
        assert_eq!(t.runs[0].style.size_pt * doc.resolution_dpi / 72., 20.);
        assert_eq!(t.transform.m[0], f64::from(scale));
    }
    let LayerContent::Text(original) = &equal.document.layers[0].content else { panic!("expected editable equal-DPI text") };
    let LayerContent::Text(editable) = &mut wide.document.layers[0].content else { panic!("expected editable unequal-DPI text") };
    let mut engine = photocraft_text::TextEngine::new();
    let before = engine.layout(original, 72.);
    let rescaled = engine.layout(editable, 144.);
    assert!(rescaled.lines.len() > before.lines.len(), "native text reflows inside the original box after scaling");
    editable.text.replace_range(..1, "X");
    let fmt = PixelFormat::RGBA8;
    engine.render_layer(editable, 144., fmt);
    assert!(!editable.cache_uses_raster_blending);
    assert!(editable.cache.is_some());
    assert_eq!(editable.transform.m, [2., 0., 0., 1., 0., 0.]);
}

#[test]
fn unequal_dpi_maps_markup_point_sizes_and_leading_before_honoring_source_affine() {
    let config = br#"(markup "<markup><span size=\"12pt\" rise=\"3072\">Ag</span></markup>") (font "Inter") (font-size 20) (font-size-unit points) (line-spacing 2) (transformation (matrix 1 0.25 0.5 1)) (box-mode fixed) (box-unit pixels) (box-width 145) (box-height 85)"#;
    let imported = import_text(vec![pair(72., 144.)], config);
    let LayerContent::Text(text) = &imported.document.layers[0].content else { panic!("expected editable source text") };
    assert_eq!(text.transform.m, [0.5, 0.25, 0.25, 1., 0., 0.]);
    assert_eq!(text.runs[0].style.size_pt, 24.);
    assert_eq!(text.runs[0].style.baseline_shift_pt, 3.);
    assert!((text.runs[0].style.leading_pt.unwrap() - 32.8).abs() < 0.001);
}

#[test]
fn unequal_dpi_pins_modern_and_legacy_point_text_to_the_source_layer_origin() {
    let mut engine = photocraft_text::TextEngine::new();
    for legacy in [false, true] {
        let ctl = &Interrupt::NONE;
        let mut layout_work = usize::MAX;
        let mut decode_work = text::DecodeWork::default();
        let resolution = (144., 72.);
        let origin = (7, 9);
        let decoded = if legacy {
            let config = br"GDT10{Ag}{1}{0}{0}{0}{112233}{0}{-*-sans-medium-r-normal-*-20-*-*-*-*-*-iso8859-1}";
            text::decode_gdyntext_with_work(config, resolution, origin, ctl, &mut layout_work, &mut decode_work)
        } else {
            let config = br#"(text "Ag") (font "sans") (font-size 20) (font-size-unit pixels)"#;
            text::decode_with_work(config, resolution, origin, ctl, &mut layout_work, &mut decode_work)
        }
        .unwrap()
        .unwrap();
        let layer = decoded.layer;
        assert_eq!(layer.size_pt, 10.);
        assert_eq!(&layer.transform.m[..4], &[2., 0., 0., 1.]);
        let bounds = engine.layout(&layer, resolution.0).bounds().unwrap();
        let top_left = layer.transform.apply(photocraft_geom::Point::new(bounds[0].into(), bounds[1].into()));
        assert_eq!((top_left.x, top_left.y), (7., 9.));
    }
}

#[test]
fn extreme_unequal_dpi_keeps_saved_pixels_when_the_native_transform_cannot_represent_it() {
    let imported = text_at_resolution(vec![pair(144., 0.005)]);
    let LayerContent::Raster(pixels) = &imported.document.layers[0].content else { panic!("saved raster text") };
    assert_eq!(pixels.pixel(0, 0), [1., 0., 0., 1.]);
    assert!(imported.warnings.iter().any(|warning| warning.contains("opens as pixels")));
    let Some(photocraft_doc::import_conversion::ImportConversion::Report(report)) = imported.document.metadata.import_conversion.as_deref() else {
        panic!("expected conversion report");
    };
    assert!(report.entries.iter().any(|entry| entry.source_description.contains("unequal source DPI exceeds the editable text transform bounds")));
}
