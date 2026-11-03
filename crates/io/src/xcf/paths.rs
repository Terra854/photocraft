//! Source vector records stay inert until native path conversion is available.
use super::{Cursor, IoError, Result};
use photocraft_doc::vector::{NamedPath, ShapeLayer};

pub(super) struct DecodedPath {
    pub named: NamedPath,
    pub tattoo: Option<u32>,
    pub selected: bool,
    pub source_only: bool,
}
pub(super) struct VectorStyle {
    pub path_tattoo: u32,
    pub modified: bool,
    pub shape: ShapeLayer,
    pub source_only: Vec<String>,
}
pub(super) fn vector_style(_data: &[u8]) -> Result<VectorStyle> {
    Err(IoError::Unsupported("Editable XCF vector styles are unsupported; saved pixels are preserved".into()))
}
pub(super) fn vectors(_data: &[u8]) -> Result<Vec<DecodedPath>> {
    Err(IoError::Unsupported("XCF vector conversion is unsupported; source records are retained".into()))
}
pub(super) fn legacy(_data: &[u8]) -> Result<Vec<DecodedPath>> {
    Err(IoError::Unsupported("Historical XCF path conversion is unsupported; source records are retained".into()))
}
pub(super) fn standalone(_c: &Cursor<'_>, _offset: usize) -> Result<DecodedPath> {
    Err(IoError::Unsupported("XCF path conversion is unsupported; source records are retained".into()))
}
