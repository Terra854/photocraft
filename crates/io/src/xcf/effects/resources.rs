//! Shared native-conversion helpers; mapping registration follows separately.

#[allow(unused_imports)]
use super::{ArgumentValue, ColorLiteral, Config, ConfigExpr, MappingContext, MappingFailure, MappingResult, NativeMapping, StoredEffect, native_filter};

#[allow(unused_imports)]
use photocraft_cms::{Builtin, ColorSpace, Curve, Intent, Profile, Transform};

#[allow(unused_imports)]
use serde_json::json;

/// Decode only portable byte encodings. Native-endian source float blobs have no
/// byte-order marker; guessing from the importing machine would corrupt colors.
pub(in crate::xcf) fn literal_rgb(value: &ColorLiteral<'_>) -> Result<[f32; 4], String> {
    // IEEE positive zero has identical bytes in either byte order. This exact
    // transparent default is portable without guessing the author's host.
    if matches!(value.encoding, "RGBA float" | "R'G'B'A float" | "R~G~B~A float")
        && value.profile.is_empty()
        && value.pixels.len() == 16
        && value.pixels.iter().all(|byte| *byte == 0)
    {
        return Ok([0.0; 4]);
    }
    let (channels, alpha, linear, gray) = match value.encoding {
        "R'G'B'A u8" | "R~G~B~A u8" => (3, true, false, false),
        "R'G'B' u8" | "R~G~B~ u8" => (3, false, false, false),
        "RGBA u8" => (3, true, true, false),
        "RGB u8" => (3, false, true, false),
        "Y'A u8" | "Y~A u8" => (1, true, false, true),
        "Y' u8" | "Y~ u8" => (1, false, false, true),
        "YA u8" => (1, true, true, true),
        "Y u8" => (1, false, true, true),
        _ => {
            return Err(format!(
                "stored color encoding {} is not independently portable; native-endian float colors need source byte-order evidence",
                value.encoding
            ));
        }
    };
    if value.pixels.len() != channels + usize::from(alpha) {
        return Err("stored color byte count does not match its encoding".into());
    }
    let fallback = if gray {
        Builtin::SGray
    } else if linear {
        Builtin::LinearSrgb
    } else {
        Builtin::Srgb
    };
    let mut source = if value.profile.is_empty() {
        fallback.profile().clone()
    } else {
        Profile::parse(value.profile).map_err(|error| format!("stored literal ICC: {error}"))?
    };
    let expected = if gray { ColorSpace::Gray } else { ColorSpace::Rgb };
    if source.color_space != expected {
        return Err("stored literal profile model does not match its encoding".into());
    }
    // Encoding TRCs are independent of assigned colorants, exactly as for source pixels.
    if linear || value.encoding.contains('~') {
        if !source.is_matrix_shaper() || source.b2a.iter().any(Option::is_some) {
            return Err("stored literal ICC has no separable native encoding representation".into());
        }
        let curve = if linear { Curve::Identity } else { photocraft_cms::curve::srgb_trc() };
        if gray {
            source.gray_trc = Some(curve);
        } else {
            source.trc = Some([curve.clone(), curve.clone(), curve]);
        }
    }
    let transform = Transform::new(&source, Builtin::Srgb.profile(), Intent::RelativeColorimetric, false)
        .map_err(|error| format!("stored literal native color conversion: {error}"))?;
    let samples: Vec<_> = value.pixels.iter().take(channels).map(|v| f32::from(*v) / 255.0).collect();
    let mut rgb = [0.0; 3];
    transform.convert_f32(&samples, channels, &mut rgb, 3, false);
    if rgb.iter().any(|v| !v.is_finite()) {
        return Err("stored literal conversion is nonfinite".into());
    }
    let [r, g, b] = rgb;
    let a = if alpha { f32::from(*value.pixels.get(channels).ok_or("stored alpha is missing")?) / 255.0 } else { 1.0 };
    Ok([r, g, b, a])
}

/// Decode portable typed literal colors into sRGB coordinates, rejecting unavailable data, nonfinite samples,
/// or invalid alpha.
pub(super) fn color(effect: &StoredEffect<'_>, name: &str) -> Result<[f32; 4], String> {
    match effect.argument(name)? {
        Some(ArgumentValue::Color(Some(value))) => literal_rgb(value),
        Some(ArgumentValue::LiteralColor { rgba, linear }) => {
            if rgba.iter().any(|value| !value.is_finite()) || !(0.0..=1.0).contains(&rgba[3]) {
                return Err(format!("source color {name} has nonfinite components or invalid alpha"));
            }
            if !linear {
                return Ok(*rgba);
            }
            // The graph parser has already decoded portable numeric components; these
            // are linear-light sRGB, not host-endian bytes or CSS integer channels.
            let transform = Transform::new(Builtin::LinearSrgb.profile(), Builtin::Srgb.profile(), Intent::RelativeColorimetric, false)
                .map_err(|error| format!("literal native color conversion: {error}"))?;
            let mut rgb = [0.; 3];
            transform.convert_f32(&rgba[..3], 3, &mut rgb, 3, false);
            if rgb.iter().any(|value| !value.is_finite()) {
                return Err("literal native color conversion is nonfinite".into());
            }
            Ok([rgb[0], rgb[1], rgb[2], rgba[3]])
        }
        Some(ArgumentValue::Color(None)) | None => Err(format!("resource unavailable: source color {name} is absent")),
        _ => Err(format!("source color {name} has the wrong stored type")),
    }
}

/// A fixed, small native table sampled from explicit parameter conversion, never
/// source-image pixels or a source renderer. Red varies fastest, as native CUBE expects.
pub(super) fn lookup(size: u32, label: &str, notes: Vec<String>, sample: impl Fn([f64; 3]) -> [f64; 3]) -> Result<NativeMapping, String> {
    use std::fmt::Write;
    if !(2..=33).contains(&size) {
        return Err("native parameter lookup size is outside 2..33".into());
    }
    if label.len() > 128 || label.chars().any(|c| c.is_control() || matches!(c, '"' | '\\')) {
        return Err("native lookup title is not plain bounded text".into());
    }
    let mut data = format!("TITLE \"{label}\"\nLUT_3D_SIZE {size}\n");
    let denominator = f64::from(size - 1);
    for b in 0..size {
        for g in 0..size {
            for r in 0..size {
                let [r, g, b] = sample([f64::from(r) / denominator, f64::from(g) / denominator, f64::from(b) / denominator]);
                if !r.is_finite() || !g.is_finite() || !b.is_finite() {
                    return Err("native resource lookup contains nonfinite colors".into());
                }
                writeln!(&mut data, "{:.9} {:.9} {:.9}", r.clamp(0.0, 1.0), g.clamp(0.0, 1.0), b.clamp(0.0, 1.0)).map_err(|error| error.to_string())?;
            }
        }
    }
    Ok(NativeMapping {
        filters: vec![native_filter(
            "image.adjustments.colorLookup",
            json!({"data": data, "fileName": "imported-colors.cube", "interpolation": "tetrahedral", "dither": false}),
        )],
        notes,
        layer_effects: Vec::new(),
        ..Default::default()
    })
}

/// Reusable ordinary CMS conversion for bounded native paints/gradient stops.
/// No source identity, resource lookup or operation executes through this mapper.
pub(super) struct NativeColorMapper {
    transform: Transform,
    mode: photocraft_color::ColorMode,
}

impl NativeColorMapper {
    /// Create an sRGB-to-document paint transform, validating RGB/Gray profile compatibility and the document
    /// transfer policy.
    pub(super) fn new(context: &MappingContext<'_>) -> Result<Self, String> {
        use photocraft_color::ColorMode;
        let fallback = match context.mode {
            ColorMode::Rgb => if context.linear { Builtin::LinearSrgb } else { Builtin::Srgb }.profile().clone(),
            ColorMode::Grayscale => {
                if context.linear {
                    photocraft_cms::builtin::linear_gray().clone()
                } else {
                    Builtin::SGray.profile().clone()
                }
            }
            _ => return Err("native paint requires an RGB or Gray target model".into()),
        };
        let destination = match &context.target_profile {
            Some(profile) => Profile::parse(profile).map_err(|error| format!("native effect destination ICC: {error}"))?,
            None => fallback,
        };
        let expected = if context.mode == ColorMode::Grayscale { ColorSpace::Gray } else { ColorSpace::Rgb };
        if destination.color_space != expected {
            return Err("native paint target profile differs from document model".into());
        }
        let transform = Transform::new(Builtin::Srgb.profile(), &destination, Intent::RelativeColorimetric, false)
            .map_err(|error| format!("native paint target conversion: {error}"))?;
        Ok(Self { transform, mode: context.mode })
    }

    /// Convert one finite portable paint into native model/profile coordinates while preserving validated
    /// alpha.
    pub(super) fn convert(&self, [r, g, b, a]: [f32; 4]) -> Result<photocraft_color::Color, String> {
        if [r, g, b, a].iter().any(|v| !v.is_finite()) || !(0.0..=1.0).contains(&a) {
            return Err("native paint has nonfinite color or invalid alpha".into());
        }
        let mut converted = [0.0; 4];
        self.transform.convert_f32(&[r, g, b], 3, &mut converted, if self.mode == photocraft_color::ColorMode::Grayscale { 1 } else { 3 }, false);
        if converted.iter().any(|v| !v.is_finite()) {
            return Err("native paint color conversion is nonfinite".into());
        }
        Ok(photocraft_color::Color { mode: self.mode, c: converted, alpha: a })
    }
}

/// Convert an already decoded sRGB literal to the actual native document model/profile.
pub(super) fn native_rgba(rgba: [f32; 4], context: &MappingContext<'_>) -> Result<photocraft_color::Color, String> {
    NativeColorMapper::new(context)?.convert(rgba)
}

/// Resolve an actual stored or already decoded portable literal. Defaults belong to the caller.
pub(super) fn native_color(effect: &StoredEffect<'_>, name: &str, context: &MappingContext) -> Result<photocraft_color::Color, String> {
    native_rgba(color(effect, name)?, context)
}

/// Convert hue-sector control coordinates back to RGB while building static native lookup parameters.
fn rgb([h, s, v]: [f64; 3]) -> [f64; 3] {
    let h = h.rem_euclid(360.0) / 60.0;
    let c = v * s;
    let x = c * (1.0 - (h.rem_euclid(2.0) - 1.0).abs());
    let m = v - c;
    let [r, g, b] = if h < 1.0 {
        [c, x, 0.0]
    } else if h < 2.0 {
        [x, c, 0.0]
    } else if h < 3.0 {
        [0.0, c, x]
    } else if h < 4.0 {
        [0.0, x, c]
    } else if h < 5.0 {
        [x, 0.0, c]
    } else {
        [c, 0.0, x]
    };
    [r + m, g + m, b + m]
}
