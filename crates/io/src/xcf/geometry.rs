//! Smart-source geometry needs only canvas framing and image properties, never drawables.

use std::io::Read;

use super::{Interrupt, IoError, Result, invalid, is_xcf, metadata, read_header, read_version, transport, wire};

type Geometry = ((f64, f64), f64);

enum Scan {
    Framed(Geometry, Option<bool>),
    Competing(bool),
}

/// Unclaimed input stays available to the ordinary non-XCF geometry reader.
/// A normal scan ends at image properties. Ambiguous compressed framing can also read
/// the pointer tables, but it never needs the inflated drawables or the envelope tail.
pub(super) fn source_geometry(name: &str, bytes: &[u8], ctl: &Interrupt<'_>) -> Result<Option<Geometry>> {
    match transport::with_xcf_prefix(name, bytes, ctl, |reader| scan(reader, ctl, None))? {
        None => Ok(None),
        Some(Scan::Framed(geometry, _)) => Ok(Some(geometry)),
        Some(Scan::Competing(_)) if is_xcf(bytes) => scan_authoritative(bytes, ctl).map(Some),
        Some(Scan::Competing(preferred)) => compare_prefixes(name, bytes, ctl, preferred).map(Some),
    }
}

fn compare_prefixes(name: &str, bytes: &[u8], ctl: &Interrupt<'_>, preferred: bool) -> Result<Geometry> {
    // Replay each interpretation independently. Large property payloads can be discarded
    // a chunk at a time, without buffering everything between the competing positions.
    let scan_with = |repair| match transport::with_xcf_prefix(name, bytes, ctl, |reader| scan(reader, ctl, Some(repair)))? {
        Some(Scan::Framed(geometry, evidence)) => Ok((geometry, evidence)),
        _ => Err(invalid("could not read the XCF palette boundary")),
    };
    let first = scan_with(preferred);
    if let Ok((geometry, Some(true))) = first {
        return Ok(geometry);
    }
    if matches!(first, Err(IoError::Cancelled)) {
        return Err(IoError::Cancelled);
    }
    let second = scan_with(!preferred);
    match (first, second) {
        (_, Err(IoError::Cancelled)) => Err(IoError::Cancelled),
        (Ok((geometry, a)), Ok((alternate, b))) => Ok(if b > a { alternate } else { geometry }),
        (Ok((geometry, _)), Err(_)) | (Err(_), Ok((geometry, _))) => Ok(geometry),
        (Err(error), Err(_)) => Err(error),
    }
}

/// Resolve competing property boundaries using the same image framing as full import.
fn scan_authoritative(bytes: &[u8], ctl: &Interrupt<'_>) -> Result<Geometry> {
    let (mut cursor, header) = read_header(bytes)?;
    let properties = wire::props_image_with_interrupt(&mut cursor, header.version, header.base_type == 2, ctl)?;
    Ok(((f64::from(header.width), f64::from(header.height)), f64::from(metadata::image_resolution(&properties, 72.))))
}

fn scan(reader: &mut dyn Read, ctl: &Interrupt<'_>, repair: Option<bool>) -> Result<Scan> {
    // Use the shared version/header validators, including legacy 26-byte headers.
    let mut bytes = [0; 30];
    read(reader, bytes.get_mut(..14).ok_or_else(|| invalid("invalid geometry header range"))?)?;
    let mut cursor = wire::Cursor { b: &bytes, pos: super::MAGIC.len(), wide: false };
    let version = read_version(&mut cursor)?;
    let length = if version >= 4 { 30 } else { 26 };
    read(reader, bytes.get_mut(14..length).ok_or_else(|| invalid("invalid geometry header range"))?)?;
    let (_, header) = read_header(bytes.get(..length).ok_or_else(|| invalid("invalid geometry header length"))?)?;
    let mut input = Stream::new(reader, length, repair);
    let mut dpi = 72.;
    let mut count = 0;
    loop {
        ctl.check().map_err(|_| IoError::Cancelled)?;
        let Some((kind, declared)) = wire::property_header(&mut input, count)? else {
            let geometry = ((f64::from(header.width), f64::from(header.height)), f64::from(dpi));
            let evidence = if repair.is_some() { wire::drawable_tables(|| input.pointer(version), version, super::MAX_INFLATED, ctl)? } else { None };
            return Ok(Scan::Framed(geometry, evidence));
        };
        count += 1;
        let remaining = wire::property_payload(&mut input, kind, declared, Some(header.version), header.base_type == 2, ctl)?;
        if let Some(preferred) = input.unresolved_palette {
            return Ok(Scan::Competing(preferred));
        }
        if kind == 19 && remaining >= 8 {
            let mut pair = [0; 8];
            input.read_exact(&mut pair)?;
            // Last valid pair wins, and extra bytes do not affect it.
            dpi = metadata::image_resolution(&[wire::Prop { offset: 0, kind, data: &pair }], dpi);
            wire::PropertyInput::discard(&mut input, remaining - pair.len())?;
        } else {
            wire::PropertyInput::discard(&mut input, remaining)?;
        }
    }
}

struct Stream<'a> {
    reader: &'a mut dyn Read,
    buffered: Vec<u8>,
    consumed: usize,
    position: usize,
    replay: bool,
    choice: Option<bool>,
    unresolved_palette: Option<bool>,
}

impl<'a> Stream<'a> {
    fn new(reader: &'a mut dyn Read, position: usize, choice: Option<bool>) -> Self {
        Self { reader, buffered: Vec::new(), consumed: 0, position, replay: choice.is_some(), choice, unresolved_palette: None }
    }

    fn pointer(&mut self, version: u32) -> Result<(u64, usize)> {
        let mut bytes = [0; 8];
        let start = if version < 11 { 4 } else { 0 };
        self.read_exact(&mut bytes[start..])?;
        Ok((u64::from_be_bytes(bytes), self.position))
    }

    fn read_exact(&mut self, output: &mut [u8]) -> Result<()> {
        let pending = &self.buffered[self.consumed..];
        let count = pending.len().min(output.len());
        let (prefix, tail) = output.split_at_mut(count);
        prefix.copy_from_slice(&pending[..count]);
        self.consumed += count;
        if self.consumed == self.buffered.len() {
            self.buffered.clear();
            self.consumed = 0;
        }
        read(self.reader, tail)?;
        self.position += output.len();
        Ok(())
    }
}

impl wire::PropertyInput for Stream<'_> {
    fn unresolved_palette_boundary(&mut self, preferred: bool) -> bool {
        if self.replay {
            return self.choice.take().unwrap_or(preferred);
        }
        self.unresolved_palette = Some(preferred);
        preferred
    }

    fn word(&mut self) -> Result<u32> {
        let mut bytes = [0; 4];
        self.read_exact(&mut bytes)?;
        Ok(u32::from_be_bytes(bytes))
    }

    fn discard(&mut self, mut length: usize) -> Result<()> {
        let mut scratch = [0; 32 * 1024];
        while length != 0 {
            let count = length.min(scratch.len());
            self.read_exact(&mut scratch[..count])?;
            length -= count;
        }
        Ok(())
    }

    fn peek(&mut self, offset: usize, length: usize) -> Result<Option<&[u8]>> {
        let end = offset.checked_add(length).filter(|end| *end <= super::MAX_STRING + 16).ok_or_else(|| invalid("property lookahead exceeds its bound"))?;
        if self.consumed != 0 {
            self.buffered.drain(..self.consumed);
            self.consumed = 0;
        }
        self.buffered.try_reserve(end.saturating_sub(self.buffered.len())).map_err(|_| invalid("not enough memory for property lookahead"))?;
        let mut scratch = [0; 4096];
        while self.buffered.len() < end {
            let count = (end - self.buffered.len()).min(scratch.len());
            match self.reader.read(&mut scratch[..count]) {
                Ok(0) => return Ok(None),
                Ok(count) => self.buffered.extend_from_slice(&scratch[..count]),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(invalid(format!("XCF geometry prefix: {error}"))),
            }
        }
        Ok(self.buffered.get(offset..end))
    }
}

fn read(reader: &mut dyn Read, output: &mut [u8]) -> Result<()> {
    reader.read_exact(output).map_err(|error| invalid(format!("XCF geometry prefix: {error}")))
}

#[cfg(test)]
mod tests;
