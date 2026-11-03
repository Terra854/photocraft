//! Original documented-wire regressions, not historical GIMP real-file qualification.
//! Framing: https://developer.gimp.org/core/standards/xcf/ (PATHS, PARASITES,
//! LINKED and LINK_LAYER). Point tags follow the declarative enum in official
//! GIMP_3_2_6 app/path/gimppath-compat.c: anchor1,control2,new-stroke3;
//! the older prose table's0/1 tags conflict with that primary declaration.
//! No historical source implementation or rendering algorithm is copied.
use photocraft_doc::{Document, LayerContent, import_conversion::ImportConversion};
use photocraft_geom::Rect;

struct Wire {
    bytes: Vec<u8>,
    wide: bool,
}
impl Wire {
    /// Starts an independent historical-wire writer without importing any existing encoded XCF data.
    fn empty() -> Self {
        Self { bytes: Vec::new(), wide: false }
    }
    /// Writes a big-endian framing word used by the documented historical records.
    fn word(&mut self, n: u32) {
        self.bytes.extend(n.to_be_bytes());
    }
    /// Writes historical length-prefixed text with its terminating NUL byte.
    fn string(&mut self, s: &str) {
        self.word(s.len() as u32 + 1);
        self.bytes.extend(s.as_bytes());
        self.bytes.push(0);
    }
    /// Writes a fixture pointer using the selected historical XCF version's pointer width.
    fn pointer(&mut self, n: usize) {
        if self.wide { self.bytes.extend((n as u64).to_be_bytes()) } else { self.word(n as u32) }
    }
    /// Reserves a historical pointer field for a record written later in the fixture.
    fn slot(&mut self) -> usize {
        let slot = self.bytes.len();
        self.pointer(0);
        slot
    }
    /// Patches a reserved historical pointer to the current independently authored record.
    fn set(&mut self, slot: usize) {
        let at = self.bytes.len();
        if self.wide {
            self.bytes[slot..slot + 8].copy_from_slice(&(at as u64).to_be_bytes())
        } else {
            self.bytes[slot..slot + 4].copy_from_slice(&(at as u32).to_be_bytes())
        }
    }
    /// Writes an exact historical property header and payload for parser-boundary regressions.
    fn property(&mut self, kind: u32, raw: &[u8]) {
        self.word(kind);
        self.word(raw.len() as u32);
        self.bytes.extend(raw);
    }
    /// Writes the documented terminator for a historical property list.
    fn end(&mut self) {
        self.word(0);
        self.word(0);
    }
}
/// Encodes a sequence of framing words independently of source parser helpers.
fn words(values: &[u32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_be_bytes()).collect()
}

/// Builds a complete historical XCF fixture with controlled image, layer and parasite records.
fn image(version: u32, image_props: &[(u32, Vec<u8>)], layer_props: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let header = if version == 0 { "gimp xcf file\0".to_string() } else { format!("gimp xcf v{version:03}\0") };
    let mut w = Wire { bytes: header.into_bytes(), wide: version >= 11 };
    for n in [16, 12, 0] {
        w.word(n)
    }
    if version >= 4 {
        w.word(150)
    }
    w.property(17, &[0]);
    w.property(19, &[144f32.to_be_bytes(), 144f32.to_be_bytes()].concat());
    for (kind, raw) in image_props {
        w.property(*kind, raw)
    }
    w.end();
    let layer = w.slot();
    w.pointer(0);
    w.pointer(0);
    if version >= 18 {
        w.pointer(0)
    }
    w.set(layer);
    for n in [1, 1, 1] {
        w.word(n)
    }
    w.string("Historical owner");
    w.property(8, &words(&[1]));
    w.property(15, &words(&[(-3i32) as u32, 7]));
    for (kind, raw) in layer_props {
        w.property(*kind, raw)
    }
    w.end();
    let hierarchy = w.slot();
    w.pointer(0);
    if version >= 20 {
        w.pointer(0)
    }
    w.set(hierarchy);
    for n in [1, 1, 4] {
        w.word(n)
    }
    let level = w.slot();
    w.pointer(0);
    w.set(level);
    w.word(1);
    w.word(1);
    let tile = w.slot();
    w.pointer(0);
    w.set(tile);
    w.bytes.extend([17, 31, 57, 255]);
    w.bytes
}
/// Requires the documented-wire fixture to produce a usable native document through public import.
fn open(bytes: &[u8]) -> Document {
    let imported = photocraft_io::import("historical.xcf", bytes).expect("documented-wire import");
    assert!(imported.source_read_only);
    imported.document
}
/// Checks native save/reopen preserves the converted document and its historical import metadata.
fn roundtrip(doc: &Document) {
    let bytes = photocraft_format::save_to_bytes(doc, &Default::default()).unwrap();
    let reopened = photocraft_format::load_from_bytes(&bytes).unwrap();
    assert!(reopened.layers == doc.layers, "native layers survive source-independent save/reopen");
    assert_eq!(reopened.paths, doc.paths);
    assert!(reopened.metadata == doc.metadata, "historical metadata survives native save/reopen");
}

#[test]
/// Checks link naming/monitor flags remain inert while independent legacy movement linkage maps to native
/// data.
fn link_rename_monitor_bits_stay_inert_while_legacy_movement_is_a_separate_native_field() {
    // LINK_LAYER bit1 preserves names, bit2 disables monitoring. Neither grants
    // filesystem permission, and neither is the independent legacy LINKED flag.
    for flags in [0, 1, 2, 3] {
        let mut link = Wire::empty();
        link.word(flags);
        link.string("../not-authorized/picture.svg");
        link.word(40);
        link.word(30);
        let raw = link.bytes;
        let doc = open(&image(25, &[], &[(48, raw.clone()), (9, words(&[1]))]));
        let owner = &doc.layers[0];
        assert_eq!(owner.link_group, Some(1));
        let LayerContent::Raster(pixels) = &owner.content else { panic!("stored native pixels, no live link") };
        assert_eq!(pixels.read_region(Rect::new(-3, 7, -2, 8)), [17. / 255., 31. / 255., 57. / 255., 1.]);
        let Some(ImportConversion::Report(report)) = doc.metadata.import_conversion.as_deref() else { panic!("durable report") };
        assert!(report.attachments.iter().any(|a| a.bytes.as_slice() == raw));
        assert!(report.entries.iter().any(|e| e.feature_code == "xcf.link_layer"));
        assert!(report.entries.iter().all(|e| !e.source_description.contains("not-authorized")));
        roundtrip(&doc);
    }
}

#[cfg(feature = "corpus")]
#[test]
#[ignore = "Requires independently authored historical sources absent from the official corpus pin (T088)"]
/// Verifies historical high-depth import uses explicit byte-order provenance and preserves editable native
/// data after reopen.
fn genuine_historical_highdepth_sources_use_explicit_provenance_order_and_editable_native_reopen() {
    use photocraft_color::{BlendMode, ColorMode, SampleType};
    use photocraft_io::{HistoricalXcfSampleOrder, ImportOptions, IoError};
    // Producer provenance and every raw public PDB pixel are recorded in the
    // authoring corpus tools/gimp-xcf/historical_2_9_6.provenance.json.
    // The ordinary Save As better-compression v8 F32 witness and the stock PDB
    // v8 U16 derivative are recorded in historical_2_9_6_v008.provenance.json
    // and historical_2_9_6_v008_u16.provenance.json, with independent readbacks.
    // The v10 pair comes from unmodified GIMP development commit0874703,
    // whose stock loader fixes its predecessor's missing version10 entry.
    // Both qualified x86-64 producers are externally verified LE; no in-file
    // discriminator is claimed for other historical producers.
    // Local producer qualification can point directly at an authoring checkout;
    // ordinary corpus tests and fetch defaults keep their official pinned inputs.
    let root = std::env::var_os("PHOTOCRAFT_XCF_SOURCE_CORPUS_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/gimp"));
    for (version, producer, compression) in
        [(7, "2-9-6", "rle"), (8, "2-9-6", "zlib"), (9, "2-9-6", "rle"), (10, "2-9-5-0874703", "zlib"), (11, "2-9-6", "rle")]
    {
        for (suffix, depth) in [("u16-asymmetric", SampleType::U16), ("float", SampleType::F32)] {
            let name = format!("historical-{producer}-v{version:03}-{suffix}-{compression}.xcf");
            let path = root.join(&name);
            let source = std::fs::read(&path)
                .unwrap_or_else(|e| panic!("{}: {e}; needs original producer fixtures absent from the official corpus pin", path.display()));
            assert_eq!(&source[..14], format!("gimp xcf v{version:03}\0").as_bytes());
            assert!(matches!(photocraft_io::import(&name, &source), Err(IoError::HistoricalXcfSampleOrderRequired(_))));
            let options = ImportOptions { historical_xcf_sample_order: Some(HistoricalXcfSampleOrder::LittleEndian), ..Default::default() };
            let imported = photocraft_io::import_with_options(&name, &source, &options).unwrap();
            assert!(imported.source_read_only);
            let mut doc = imported.document;
            assert_eq!((doc.size.width, doc.size.height, doc.mode, doc.depth), (8, 6, ColorMode::Rgb, depth));
            assert_eq!(doc.layers.len(), 1);
            assert_eq!(
                doc.layers[0].blend,
                match version {
                    9 => BlendMode::Overlay,
                    10 => BlendMode::Multiply,
                    _ => BlendMode::Normal,
                }
            );
            let LayerContent::Raster(surface) = &doc.layers[0].content else { panic!("native editable original pixels") };
            for y in 0..6 {
                for x in 0..8 {
                    let expected = if depth == SampleType::U16 {
                        [4660 + x * 259 + y * 517, 13398 + x * 257 + y * 775, 30874 + x * 515 + y * 265, 16384 + ((x + y) % 4) * 8192]
                            .map(|v| v as f32 / 65535.)
                    } else {
                        // Independently authored integer pattern and actual
                        // producer U8→F32 normalization; all48 public source
                        // byte vectors independently verify this exact formula.
                        [(x * 23 + 17) % 256, (y * 37 + 13) % 256, (x * 11 + y * 19 + 29) % 256, 64 + ((x + y) % 4) * 48].map(|v| v as f32 * (1. / 255.))
                    };
                    assert_eq!(surface.pixel(x, y).iter().map(|v| v.to_bits()).collect::<Vec<_>>(), expected.map(f32::to_bits), "{name} pixel{x},{y}");
                }
            }
            let Some(ImportConversion::Report(report)) = doc.metadata.import_conversion.as_deref() else { panic!("durable native conversion provenance") };
            assert!(
                report.entries.iter().any(|entry| entry.feature_code == "xcf.historical_sample_order" && entry.native_description.contains("little-endian"))
            );
            let LayerContent::Raster(surface) = &mut doc.layers[0].content else { panic!("editable source raster") };
            surface.write_pixel(0, 0, &[0.125, 0.25, 0.5, 0.75]);
            let edited = surface.pixel(0, 0);
            // Imported source bytes are no longer available to native loading;
            // the saved document consists of native editable raster data.
            drop(source);
            let saved = photocraft_format::save_to_bytes(&doc, &Default::default()).unwrap();
            let reopened = photocraft_format::load_from_bytes(&saved).unwrap();
            assert_eq!(reopened.layers[0].surface().unwrap().pixel(0, 0), edited);
            assert_eq!(reopened.layers, doc.layers);
            assert_eq!(reopened.icc_profile, doc.icc_profile);
            assert_eq!(reopened.metadata.import_conversion, doc.metadata.import_conversion);
        }
    }
}
