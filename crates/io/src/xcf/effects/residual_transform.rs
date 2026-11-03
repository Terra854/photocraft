//! Shared native-conversion helpers; mapping registration follows separately.

#[allow(unused_imports)]
use super::{ArgumentValue, MappingContext, MappingFailure, MappingResult, NativeBranch, NativeMapping, NativeStructure, StoredEffect, native_filter};

#[allow(unused_imports)]
use photocraft_algo::transform::Homography;

#[allow(unused_imports)]
use photocraft_color::BlendMode;

#[allow(unused_imports)]
use photocraft_doc::effects::{Effect, FxCommon};

#[allow(unused_imports)]
use photocraft_geom::{Affine, Rect};

#[allow(unused_imports)]
use serde_json::{Value, json};

type Result<T> = std::result::Result<T, MappingFailure>;

/// Read a finite stored number inside the conversion domain; reject out-of-range values instead of silently
/// clamping native parameters.
fn number(e: &StoredEffect<'_>, key: &str, default: f64, low: f64, high: f64) -> Result<f64> {
    let value = e.number(key)?.unwrap_or(default);
    if !value.is_finite() || value < f64::from(low as f32) || value > f64::from(high as f32) {
        return Err(format!("{} {key} outside bounded conversion range", e.operation).into());
    }
    Ok(value)
}

/// Require an integral stored value within the requested unsigned range before converting its representation.
fn integer(e: &StoredEffect<'_>, key: &str, default: u32, low: u32, high: u32) -> Result<u32> {
    let value = number(e, key, f64::from(default), f64::from(low), f64::from(high))?;
    if value.fract() != 0.0 || value > f64::from(high) {
        return Err(format!("{key} must be a bounded integer").into());
    }
    Ok(value as u32)
}

/// Reject properties this adapter cannot translate so source controls are never silently discarded.
fn fields(e: &StoredEffect<'_>, keys: &[&str]) -> Result<()> {
    for argument in &e.arguments {
        if !keys.contains(&argument.name) {
            return Err(format!("unmapped {} field {}", e.operation, argument.name).into());
        }
    }
    Ok(())
}

/// Package one ordinary native Smart Filter together with the policy explaining its approximation.
fn mapped(command: &str, params: Value, note: &str) -> NativeMapping {
    NativeMapping { filters: vec![native_filter(command, params)], notes: vec![note.into()], ..Default::default() }
}

/// Price all intersecting padded mask tiles and coordinate work with checked arithmetic before allocating a
/// mask.
pub(super) fn coordinate_mask_work_bytes(frame: Rect) -> Result<usize> {
    if frame.is_empty() {
        return Err("Panorama mask requires a nonempty frame".into());
    }
    let tile = i64::from(photocraft_geom::TILE_SIZE);
    let columns = (i64::from(frame.x1) - 1).div_euclid(tile) - i64::from(frame.x0).div_euclid(tile) + 1;
    let rows = (i64::from(frame.y1) - 1).div_euclid(tile) - i64::from(frame.y0).div_euclid(tile) + 1;
    let tiles = columns
        .checked_mul(rows)
        .and_then(|n| n.checked_mul(tile * tile * 4))
        .and_then(|n| usize::try_from(n).ok())
        .ok_or("Panorama mask tile size overflow")?;
    let row = usize::try_from(frame.width()).ok().and_then(|width| width.checked_mul(4)).ok_or("Panorama mask row size overflow")?;
    tiles.checked_add(row).ok_or_else(|| "Panorama mask scratch overflow".into())
}

/// Generate a static native coordinate mask only within both remaining allowances, checking cancellation
/// during traversal.
pub(super) fn coordinate_mask(context: &MappingContext, frame: Rect, visible: impl Fn(f64, f64) -> bool) -> Result<photocraft_doc::LayerMask> {
    use photocraft_color::{ColorMode, PixelFormat, SampleType};
    let bytes = coordinate_mask_work_bytes(frame)?;
    if bytes > context.remaining_generation_bytes || bytes > context.remaining_metadata_bytes {
        return Err(MappingFailure::resource("Native coordinate mask exceeds remaining working/metadata allowance"));
    }
    context.interrupt.check().map_err(|e| e.to_string())?;
    let mut surface = photocraft_raster::Surface::new(PixelFormat::new(ColorMode::Grayscale, SampleType::F32, false));
    let mut row = Vec::new();
    row.try_reserve_exact(frame.width() as usize).map_err(|_| "Panorama mask row allocation unavailable")?;
    row.resize(frame.width() as usize, 0.);
    for y in 0..frame.height() {
        context.interrupt.check().map_err(|e| e.to_string())?;
        for (x, value) in row.iter_mut().enumerate() {
            if x % 256 == 0 {
                context.interrupt.check().map_err(|e| e.to_string())?;
            }
            *value = if visible((x as f64 + 0.5) / f64::from(frame.width()), (f64::from(y) + 0.5) / f64::from(frame.height())) { 1. } else { 0. };
        }
        let y = frame.y0 + y as i32;
        surface.try_write_region(Rect::new(frame.x0, y, frame.x1, y + 1), &row).map_err(|e| e.to_string())?;
    }
    Ok(photocraft_doc::LayerMask { surface, ..photocraft_doc::LayerMask::reveal_all() })
}

/// Author an ordinary native Displace resource from a deterministic coordinate map.
/// Destination indices are local to native_filter_bounds. Returned source coordinates
/// use the same local pixel-centre convention: the first source centre is (0.5,0.5).
/// No callback or source operation survives import; only native U8 map data is stored.
pub(super) fn coordinate_map(context: &MappingContext, coordinate: impl Fn(u32, u32) -> (f64, f64), note: &str) -> Result<NativeMapping> {
    let bounds = context.native_filter_bounds;
    let width = bounds.width();
    let height = bounds.height();
    context.interrupt.check().map_err(|error| error.to_string())?;
    let count =
        usize::try_from(width).ok().and_then(|w| usize::try_from(height).ok().and_then(|h| w.checked_mul(h))).ok_or("native coordinate map area overflow")?;
    if count == 0 || count > 1 << 24 {
        return Err("Coordinate map exceeds native embedded displacement dimensions".into());
    }
    // Native params encode two U8 planes in a descriptor hex string, then native
    // contents/JSON may wrap those bytes again. Reserve conservatively for those
    // concurrent copies before creating either the float map or its descriptor.
    let metadata = count.checked_mul(8).and_then(|n| n.checked_add(16384)).ok_or("native coordinate parameter size overflow")?;
    let work = count.checked_mul(64).and_then(|n| n.checked_add(16384)).ok_or("native coordinate work size overflow")?;
    if metadata > (32 << 20).min(context.remaining_metadata_bytes) || work > context.remaining_generation_bytes {
        return Err(MappingFailure {
            reason: photocraft_doc::import_conversion::ConversionReason::RetentionLimit,
            message: "Native coordinate map/hex/base64 scratch or parameters exceed the current generation/32MiB JSON allowance".into(),
        });
    }
    let displacement = |x, y| {
        let (u, v) = coordinate(x, y);
        (u - (f64::from(x) + 0.5), v - (f64::from(y) + 0.5))
    };
    let mut range_x = 0f64;
    let mut range_y = 0f64;
    for y in 0..height {
        context.interrupt.check().map_err(|error| error.to_string())?;
        for x in 0..width {
            if x % 256 == 0 {
                context.interrupt.check().map_err(|error| error.to_string())?;
            }
            let (dx, dy) = displacement(x, y);
            if !dx.is_finite() || !dy.is_finite() {
                return Err("Native coordinate callback produced nonfinite UV offsets".into());
            }
            range_x = range_x.max(dx.abs());
            range_y = range_y.max(dy.abs());
        }
    }
    let max_displacement = 999. * 128. / 100.;
    if !range_x.is_finite() || !range_y.is_finite() || range_x > max_displacement || range_y > max_displacement {
        return Err(
            "Coordinate UV offsets exceed this native Displace approximation's ±1278.72 pixel range; wider native warp conversion remains unfinished".into()
        );
    }
    let samples = count.checked_mul(3).ok_or("native coordinate sample count overflow")?;
    let mut data = Vec::new();
    data.try_reserve_exact(samples).map_err(|_| "native coordinate map allocation unavailable")?;
    for y in 0..height {
        context.interrupt.check().map_err(|error| error.to_string())?;
        for x in 0..width {
            if x % 256 == 0 {
                context.interrupt.check().map_err(|error| error.to_string())?;
            }
            let (dx, dy) = displacement(x, y);
            if !dx.is_finite() || !dy.is_finite() {
                return Err("Native coordinate callback produced nonfinite UV offsets".into());
            }
            if dx.abs() > range_x || dy.abs() > range_y {
                return Err("Native coordinate callback changed between bounded authoring passes".into());
            }
            data.extend_from_slice(&[
                if range_x == 0. { 0.5 } else { (0.5 + dx / (2. * range_x)) as f32 },
                if range_y == 0. { 0.5 } else { (0.5 + dy / (2. * range_y)) as f32 },
                0.5,
            ]);
        }
    }
    let image = photocraft_algo::Image { rect: Rect::new(0, 0, width as i32, height as i32), ch: 3, data };
    let settings = json!({"horizontal":range_x*100./128.,"vertical":range_y*100./128.,"fit":"stretch","undefinedAreas":"wrap"});
    let params = crate::smart_map::displace_embed_map(&settings, &image).ok_or("native coordinate map could not be embedded")?;
    context.interrupt.check().map_err(|error| error.to_string())?;
    Ok(mapped("filter.distort.displace", params, note))
}

/// Construct an ordinary rectangular vector mask in the requested native coordinate frame.
fn extent_mask(x: f64, y: f64, width: u32, height: u32) -> photocraft_doc::VectorMask {
    photocraft_doc::VectorMask::new(photocraft_doc::Path::new(vec![photocraft_doc::Subpath::polygon(&[
        (x, y),
        (x + f64::from(width), y),
        (x + f64::from(width), y + f64::from(height)),
        (x, y + f64::from(height)),
    ])]))
}

/// Represent an empty generator as editable transparent native fill data, preserving an explicitly empty
/// result.
fn empty_pattern() -> NativeMapping {
    NativeMapping {
        structure: Some(NativeStructure::Replace {
            layer: Box::new(photocraft_doc::Layer::new(
                "Empty source generator",
                photocraft_doc::LayerContent::Fill(photocraft_doc::Fill::Solid(photocraft_color::Color::TRANSPARENT)),
            )),
            patterns: Vec::new(),
        }),
        notes: vec!["Zero-sized source generator becomes ordinary transparent native fill".into()],
        ..Default::default()
    }
}

/// Original smooth value noise: integer lattice hash and cubic smoothstep.
/// This import-only native pattern authoring primitive does not evaluate a source node.
pub(super) fn lattice(x: i64, y: i64, z: i64, seed: u32) -> f64 {
    let mut value = (x as u64).wrapping_mul(0x9e3779b185ebca87)
        ^ (y as u64).wrapping_mul(0xc2b2ae3d27d4eb4f)
        ^ (z as u64).wrapping_mul(0x165667b19e3779f9)
        ^ u64::from(seed);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d049bb133111eb);
    (value ^ (value >> 31)) as f64 / u64::MAX as f64
}

/// Sample the importer-authored deterministic lattice field used to create static native noise Pattern
/// pixels.
fn value_noise(x: f64, y: f64, z: f64, seed: u32) -> f64 {
    let (ix, iy, iz) = (x.floor() as i64, y.floor() as i64, z.floor() as i64);
    let smooth = |v: f64| v * v * (3. - 2. * v);
    let (fx, fy, fz) = (smooth(x - x.floor()), smooth(y - y.floor()), smooth(z - z.floor()));
    let mut value = 0.;
    for dz in 0..2 {
        for dy in 0..2 {
            for dx in 0..2 {
                value += lattice(ix.saturating_add(dx), iy.saturating_add(dy), iz.saturating_add(dz), seed)
                    * if dx == 0 { 1. - fx } else { fx }
                    * if dy == 0 { 1. - fy } else { fy }
                    * if dz == 0 { 1. - fz } else { fz };
            }
        }
    }
    value
}

/// Translate this finite noise-generator family into bounded static native patterns with explicit seed,
/// octave, color, and extent policies.
fn noise(e: &StoredEffect<'_>, context: &MappingContext) -> Result<NativeMapping> {
    let (width, height, x, y, seed, octaves, frequency, multiplier, decay, z, colored) = match e.operation {
        "gegl:plasma" => {
            fields(e, &["turbulence", "x", "y", "width", "height", "seed"])?;
            let turbulence = number(e, "turbulence", 1., 0., 7.)?;
            (
                integer(e, "width", 1024, 0, i32::MAX as u32)?,
                integer(e, "height", 768, 0, i32::MAX as u32)?,
                number(e, "x", 0., f64::from(i32::MIN), f64::from(i32::MAX))?,
                number(e, "y", 0., f64::from(i32::MIN), f64::from(i32::MAX))?,
                integer(e, "seed", 0, 0, u32::MAX)?,
                6,
                1. / 128.,
                2.,
                0.25 + turbulence * 0.075,
                0.,
                true,
            )
        }
        "gegl:perlin-noise" => {
            fields(e, &["alpha", "scale", "zoff", "n"])?;
            let alpha = number(e, "alpha", 1.2, 0.01, 100.)?;
            let scale = number(e, "scale", 1.8, 0.01, 4.)?;
            (
                context.native_filter_bounds.width(),
                context.native_filter_bounds.height(),
                f64::from(context.native_filter_bounds.x0),
                f64::from(context.native_filter_bounds.y0),
                0,
                integer(e, "n", 3, 0, 20)?,
                1. / 64.,
                scale,
                1. / alpha,
                number(e, "zoff", -1., -1_000_000., 1_000_000.)?,
                false,
            )
        }
        _ => {
            fields(e, &["scale", "iterations", "seed"])?;
            (
                context.native_filter_bounds.width(),
                context.native_filter_bounds.height(),
                f64::from(context.native_filter_bounds.x0),
                f64::from(context.native_filter_bounds.y0),
                integer(e, "seed", 0, 0, u32::MAX)?,
                integer(e, "iterations", 1, 1, 20)?,
                number(e, "scale", 1., 0., 20.)? / 64.,
                2.,
                0.5,
                0.,
                false,
            )
        }
    };
    if width == 0 || height == 0 {
        return Ok(empty_pattern());
    }
    if x.fract() != 0. || y.fract() != 0. {
        return Err("Source procedural extent coordinates must be integral".into());
    }
    let work = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|n| n.checked_mul(u64::from(octaves.max(1))))
        .and_then(|n| n.checked_mul(if colored { 3 } else { 1 }))
        .ok_or("native noise work count overflow")?;
    if work > 32_000_000 {
        return Err("Native procedural pattern exceeds bounded import work allowance".into());
    }
    let last_frequency = frequency * multiplier.powi(octaves as i32);
    if !last_frequency.is_finite()
        || last_frequency * (x.abs() + y.abs() + f64::from(width) + f64::from(height) + 1.) > 1e14
        || !decay.powi(octaves as i32).is_finite()
    {
        return Err("Native procedural controls exceed finite lattice/amplitude bounds".into());
    }
    let mut converted = super::residual_tone::pattern_result(
        "Imported native fractal pattern",
        photocraft_geom::Size::new(width, height),
        (x as f32, y as f32),
        0.,
        context,
        |px, py| {
            let mut output = [0., 0., 0., 1.];
            for channel in 0..if colored { 3 } else { 1 } {
                let mut sum = 0.;
                let mut weight = 0.;
                let mut amplitude = 1.;
                let mut f = frequency;
                for octave in 0..octaves {
                    sum += amplitude
                        * value_noise((f64::from(px) + x) * f, (f64::from(py) + y) * f, z + f64::from(octave) * 0.37, seed.wrapping_add(channel * 0x10001));
                    weight += amplitude;
                    amplitude *= decay;
                    f *= multiplier;
                }
                if let Some(value) = output.get_mut(channel as usize) {
                    *value = if weight == 0. { 0.5 } else { (sum / weight).clamp(0., 1.) as f32 };
                }
            }
            if !colored {
                output[1] = output[0];
                output[2] = output[0];
            }
            Ok(output)
        },
        "Original smooth lattice noise and normalized octave superposition becomes ordinary native Pattern pixels through target ICC. Source generator intent, seed and bounded geometry are translated; source algorithms are approximated by native cubic value noise. Perlin alpha becomes native inverse octave-amplitude ratio, scale an octave-frequency ratio, zoff a third-coordinate phase; Simplex scale controls approximate pixel frequency and iterations octave count; Plasma turbulence controls native high-frequency detail with independent RGB channels. Source spectral statistics, interpolation, normalization and random sequences differ; fixed Perlin seed 0 is a native policy, not a decoded field",
    )?;
    if let Some(NativeStructure::Replace { layer, .. }) = &mut converted.structure {
        layer.vector_mask = Some(extent_mask(x, y, width, height));
    }
    Ok(converted)
}
