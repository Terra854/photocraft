//! Editable source text is retained as inert configuration until conversion is available.
use super::{IoError, Result};
use photocraft_doc::TextLayer;

pub(super) struct DecodedText {
    pub layer: TextLayer,
    pub source_only: Vec<String>,
    pub native_policy: Vec<String>,
}

pub(super) fn decode(_data: &[u8], _dpi: f32, _offset: (i32, i32)) -> Result<DecodedText> {
    Err(IoError::Unsupported("Editable XCF text conversion is unsupported; saved pixels are preserved".into()))
}

pub(super) fn decode_gdyntext(_data: &[u8], _dpi: f32, _offset: (i32, i32)) -> Result<DecodedText> {
    Err(IoError::Unsupported("Editable historical XCF text conversion is unsupported; saved pixels are preserved".into()))
}
