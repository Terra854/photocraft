//! Shared native-conversion helpers; mapping registration follows separately.

#[allow(unused_imports)]
use photocraft_color::{BlendMode, ColorMode};

#[allow(unused_imports)]
use photocraft_doc::{SmartFilter, import_conversion::ConversionReason};

#[allow(unused_imports)]
use serde_json::{Value, json};

#[allow(unused_imports)]
use super::{ArgumentValue, MappingContext, MappingFailure, MappingResult, NativeMapping, StoredEffect, native_filter};

type Result<T> = std::result::Result<T, MappingFailure>;

/// Classify malformed stored controls as invalid optional data rather than an intrinsic native capability
/// limit.
fn invalid(message: impl Into<String>) -> MappingFailure {
    MappingFailure { reason: ConversionReason::InvalidOptionalData, message: message.into() }
}

/// Conservative native box-Gaussian workspace: clipped output, repeated-edge 1-D
/// lines/strips and concurrent branch caches, never a two-dimensional halo.
pub(super) fn gaussian_workspace(c: &MappingContext, sigma: f64, caches: usize) -> Result<()> {
    let b = c.native_filter_bounds;
    if b.is_empty() || !sigma.is_finite() || !(0.0..=1000.).contains(&sigma) {
        return Err(invalid("native Gaussian workspace requires nonempty bounds and radius 0..1000"));
    }
    let w = b.width() as usize;
    let h = b.height() as usize;
    let reach = (sigma * 3. + 6.).ceil() as usize;
    let samples = w
        .checked_mul(h)
        .and_then(|area| w.checked_add(2 * reach).and_then(|line| line.checked_mul(h)).and_then(|lines| area.checked_add(lines)))
        .and_then(|area| h.checked_add(2 * reach).and_then(|line| line.checked_mul(w)).and_then(|lines| area.checked_add(lines)))
        .and_then(|n| n.checked_mul(3))
        .and_then(|n| n.checked_mul(c.mode.color_channels() + 1))
        .and_then(|n| n.checked_mul(4))
        .ok_or_else(|| MappingFailure::resource("native Gaussian scratch size overflows"))?;
    let tile = photocraft_geom::TILE_SIZE as usize;
    let axis = |a: i32, z: i32| -> Result<usize> {
        usize::try_from((i64::from(z) - 1).div_euclid(tile as i64) - i64::from(a).div_euclid(tile as i64) + 1)
            .map_err(|_| MappingFailure::resource("native Gaussian tile extent is invalid"))
    };
    let cache = axis(b.x0, b.x1)?
        .checked_mul(axis(b.y0, b.y1)?)
        .and_then(|n| n.checked_mul(tile * tile))
        .and_then(|n| n.checked_mul((c.mode.color_channels() + 1) * 4))
        .and_then(|n| n.checked_mul(caches))
        .and_then(|n| n.checked_add(samples))
        .ok_or_else(|| MappingFailure::resource("native Gaussian cache size overflows"))?;
    super::residual_spatial::generation_bytes(c, cache)
}
