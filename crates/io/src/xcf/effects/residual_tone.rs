//! Shared native-conversion helpers; mapping registration follows separately.

#[allow(unused_imports)]
use super::{ArgumentValue, MappingContext, MappingFailure, MappingResult, NativeBranch, NativeMapping, NativeStructure, StoredEffect, native_filter};

#[allow(unused_imports)]
use photocraft_color::{BlendMode, Color, ColorMode, PixelFormat, SampleType};

#[allow(unused_imports)]
use photocraft_doc::effects::{Bevel, BevelStyle, BevelTechnique, Contour, Effect, FxCommon};

#[allow(unused_imports)]
use photocraft_doc::{Fill, Layer, LayerContent, Pattern};

#[allow(unused_imports)]
use photocraft_geom::{Rect, Size};

#[allow(unused_imports)]
use photocraft_raster::Surface;

#[allow(unused_imports)]
use serde_json::{Value, json};

type Result<T> = std::result::Result<T, MappingFailure>;

/// Package a single ordinary native filter with the note documenting its translation policy.
fn one(command: &str, params: Value, note: &str) -> NativeMapping {
    NativeMapping { filters: vec![native_filter(command, params)], notes: vec![note.into()], ..Default::default() }
}

/// Generate a native Pattern resource only after checking dimensions, padded tiles, live allowances, and
/// cancellation.
pub(super) fn pattern_result(
    name: &str,
    size: Size,
    phase: (f32, f32),
    angle: f32,
    c: &MappingContext,
    mut sample: impl FnMut(u32, u32) -> Result<[f32; 4]>,
    note: &str,
) -> Result<NativeMapping> {
    use photocraft_cms::{Builtin, ColorSpace, Intent, Profile, Transform};
    let (width, height) = (size.width, size.height);
    let count = u64::from(width).checked_mul(u64::from(height)).ok_or("native pattern area overflow")?;
    if width == 0
        || height == 0
        || width > photocraft_doc::pattern::MAX_EDGE
        || height > photocraft_doc::pattern::MAX_EDGE
        || count > photocraft_doc::pattern::max_paint_pixels(SampleType::F32)
    {
        return Err("source pattern exceeds ordinary native pattern edge/paint limits".into());
    }
    c.interrupt.check().map_err(|error| error.to_string())?;
    if !matches!(c.mode, ColorMode::Rgb | ColorMode::Grayscale) {
        return Err("native generated patterns require an RGB or Gray target".into());
    }
    let format = PixelFormat::new(c.mode, SampleType::F32, true);
    let channels = format.channels();
    let row_width = usize::try_from(width).map_err(|_| "native pattern row width overflow")?;
    let source_len = row_width.checked_mul(4).ok_or("native source pattern row overflow")?;
    let converted_len = row_width.checked_mul(channels).ok_or("native converted pattern row overflow")?;
    // The surface allocates complete native tiles, even for a one-pixel image.
    // Count their actual padded storage before sampling or allocating anything.
    let edge = u32::try_from(photocraft_geom::TILE_SIZE).map_err(|_| "native tile edge")?;
    let tiles = usize::try_from(width.div_ceil(edge))
        .ok()
        .and_then(|x| usize::try_from(height.div_ceil(edge)).ok().and_then(|y| x.checked_mul(y)))
        .ok_or("native pattern tile count overflow")?;
    let tile_pixels = usize::try_from(edge).ok().and_then(|v| v.checked_mul(v)).ok_or("native tile area overflow")?;
    let surface_bytes = tiles.checked_mul(tile_pixels).and_then(|v| v.checked_mul(format.bytes_per_pixel())).ok_or("native pattern tile storage overflow")?;
    let scratch_bytes =
        source_len.checked_add(converted_len).and_then(|v| v.checked_mul(std::mem::size_of::<f32>())).ok_or("native pattern row scratch overflow")?;
    let peak = surface_bytes.checked_add(scratch_bytes).ok_or("native pattern working storage overflow")?;
    if peak > c.remaining_generation_bytes {
        return Err("native pattern padded tiles and row scratch exceed remaining generation allowance".into());
    }
    let fallback = if c.mode == ColorMode::Grayscale {
        if c.linear { photocraft_cms::builtin::linear_gray().clone() } else { Builtin::SGray.profile().clone() }
    } else {
        if c.linear { Builtin::LinearSrgb } else { Builtin::Srgb }.profile().clone()
    };
    let target = match &c.target_profile {
        Some(bytes) => Profile::parse(bytes).map_err(|e| e.to_string())?,
        None => fallback,
    };
    let expected = if c.mode == ColorMode::Grayscale { ColorSpace::Gray } else { ColorSpace::Rgb };
    if target.color_space != expected {
        return Err("native pattern target ICC model mismatch".into());
    }
    let mut rgba = Vec::new();
    rgba.try_reserve_exact(source_len).map_err(|_| "native source pattern row allocation unavailable")?;
    let mut pixels = Vec::new();
    pixels.try_reserve_exact(converted_len).map_err(|_| "native converted pattern allocation unavailable")?;
    pixels.resize(converted_len, 0.);
    let transform = Transform::new(Builtin::Srgb.profile(), &target, Intent::RelativeColorimetric, false).map_err(|e| e.to_string())?;
    let mut surface = Surface::new(format);
    for y in 0..height {
        c.interrupt.check().map_err(|error| error.to_string())?;
        rgba.clear();
        for x in 0..width {
            if x % 256 == 0 {
                c.interrupt.check().map_err(|error| error.to_string())?;
            }
            let px = sample(x, y)?;
            if px.iter().any(|v| !v.is_finite()) || !(0.0..=1.0).contains(&px[3]) {
                return Err("generated native pattern has nonfinite colors or invalid coverage".into());
            }
            rgba.extend_from_slice(&px);
        }
        c.interrupt.check().map_err(|error| error.to_string())?;
        transform.convert_f32(&rgba, 4, &mut pixels, channels, true);
        if pixels.iter().any(|v| !v.is_finite()) {
            return Err("native pattern CMS conversion produced nonfinite samples".into());
        }
        c.interrupt.check().map_err(|error| error.to_string())?;
        surface.try_write_region(Rect::new(0, y as i32, width as i32, y as i32 + 1), &pixels).map_err(|error| error.to_string())?;
    }
    // Hashing needs only one converted row after both CMS scratch rows are gone.
    drop(rgba);
    drop(pixels);
    let hash_allowance = c.remaining_generation_bytes.checked_sub(surface_bytes).ok_or("native pattern hash allowance exhausted")?;
    let pattern = Pattern::try_new_in(name, surface, width, height, hash_allowance, &c.interrupt).map_err(|error| error.to_string())?;
    let layer = Layer::new(name, LayerContent::Fill(Fill::Pattern { name: pattern.name.clone(), id: pattern.id.clone(), scale: 1., angle, link: true, phase }));
    Ok(NativeMapping {
        structure: Some(NativeStructure::Replace { layer: Box::new(layer), patterns: vec![pattern] }),
        notes: vec![note.into()],
        ..Default::default()
    })
}
