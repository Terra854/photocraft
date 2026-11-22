//! Shared XML parsing for XCF markup and metadata with format-specific limits.

use craft_xml::{Canonical, El, Event, Limits, Names, Options, XML_NS, is_name, is_xml_char};
use photocraft_raster::Interrupt;

use super::{Result, invalid, work_budget::WorkBudget};
use crate::IoError;

const MAX_ATTRIBUTES: usize = 256;
const PARSE_WORK: usize = 1 << 20;
pub(super) fn admit(xml: &str, label: &str, max_input: usize, max_depth: usize, ctl: &Interrupt<'_>) -> Result<()> {
    ctl.check().map_err(|_| IoError::Cancelled)?;
    if xml.len() > max_input {
        return Err(IoError::Unsupported(format!("{label} exceeds the bounded XML byte allowance")));
    }
    if craft_svg::guard::too_wide(xml, MAX_ATTRIBUTES, PARSE_WORK) {
        return Err(IoError::Unsupported(format!("{label} exceeds the bounded XML attribute-parse allowance")));
    }
    ctl.check().map_err(|_| IoError::Cancelled)?;
    if craft_svg::guard::deeper_than(xml, max_depth) {
        return Err(IoError::Unsupported(format!("{label} exceeds the bounded XML nesting allowance")));
    }
    let limits = craft_xml::Limits { max_input, max_depth, max_attrs: MAX_ATTRIBUTES, ..Default::default() };
    check_namespace_work(xml, label, limits, PARSE_WORK, ctl)
}

pub(super) fn parse_admitted(xml: &str, label: &str, max_input: usize, max_depth: usize, max_nodes: usize) -> Result<El> {
    let limits = Limits { max_input, max_depth, max_nodes, max_attrs: MAX_ATTRIBUTES, max_attrs_total: PARSE_WORK, ..Limits::DEFAULT };
    // Canonical names distinguish unqualified elements and attributes from
    // namespaced ones without retaining declarations in the text style tree.
    let namespaces = Canonical::new(&[]);
    let options = Options { names: Names::Canonical(&namespaces), ns_decls: false, ..options(limits) };
    options.parse(xml.as_bytes()).map_err(|error| match error {
        craft_xml::Error::TooManyNodes(_) => IoError::Unsupported(format!("{label} exceeds the bounded XML node allowance")),
        _ => invalid(format!("invalid {label}: {error}")),
    })
}

fn options(limits: Limits) -> Options<'static> {
    Options {
        names: craft_xml::Names::AsWritten,
        ns_decls: true,
        strict: true,
        doctype: craft_xml::Doctype::Reject,
        unknown_entity: craft_xml::UnknownEntity::Error,
        limits,
        comments: true,
        pis: true,
        ..Default::default()
    }
}

fn is_namespace_declaration(name: &str) -> bool {
    name == "xmlns" || name.starts_with("xmlns:")
}

fn is_ncname(name: &str) -> bool {
    !name.contains(':') && is_name(name)
}

fn valid_qualified_name(name: &str, bindings: &[String]) -> bool {
    match name.split_once(':') {
        Some((prefix, local)) => is_ncname(local) && (prefix == "xml" || bindings.iter().rev().any(|bound| bound == prefix)),
        None => is_ncname(name),
    }
}

/// Bound namespace lookup work and preserve strict names before the shared
/// parser builds a tree. Its fragment reader accepts undeclared prefixes.
pub(super) fn check_namespace_work(xml: &str, label: &str, limits: Limits, mut work_left: usize, ctl: &Interrupt<'_>) -> Result<()> {
    const XMLNS_NS: &str = "http://www.w3.org/2000/xmlns/";
    let malformed = |detail| invalid(format!("invalid {label}: {detail}"));
    let mut events = options(limits).events(xml.as_bytes());
    let mut scopes = Vec::new();
    let mut bindings = Vec::new();
    let mut declarations_seen = 1usize; // Includes the predefined XML binding.
    loop {
        ctl.check().map_err(|_| IoError::Cancelled)?;
        let Some(event) = events.next().map_err(|error| invalid(format!("invalid {label}: {error}")))? else { break };
        match event {
            Event::Start { name, attrs } => {
                let declarations = attrs.iter().filter(|(name, _)| is_namespace_declaration(name)).count();
                // Charge all declarations in scope, including shadowed prefixes.
                let scope = bindings.len().saturating_add(declarations);
                let copying = if declarations == 0 { 0 } else { scope.saturating_mul(scope) };
                let inserted = declarations_seen.saturating_add(declarations);
                let insertion = declarations.saturating_mul(inserted);
                let cost = copying.saturating_add(scope.saturating_mul(attrs.len().saturating_add(1))).saturating_add(insertion);
                if !WorkBudget::new(&mut work_left).keep_remaining(cost) {
                    return Err(IoError::Unsupported(format!("{label} exceeds the bounded XML namespace-parse allowance")));
                }
                declarations_seen = inserted;
                scopes.push(bindings.len());
                if xml.as_bytes().get(events.span()).and_then(|tag| tag.get(1..)).is_some_and(|tag| tag.contains(&b'<')) {
                    return Err(malformed("a start tag contains a literal '<'"));
                }
                for (key, value) in &attrs {
                    if !value.chars().all(is_xml_char) {
                        return Err(malformed("an attribute contains an invalid XML character"));
                    }
                    if let Some(prefix) = key.strip_prefix("xmlns:") {
                        if !is_ncname(prefix) || prefix == "xmlns" || value.is_empty() || value == XMLNS_NS || (prefix == "xml") != (value == XML_NS) {
                            return Err(malformed("invalid namespace declaration"));
                        }
                        bindings.push(prefix.to_owned());
                    } else if key == "xmlns" {
                        if value == XML_NS || value == XMLNS_NS {
                            return Err(malformed("invalid default namespace"));
                        }
                        bindings.push(String::new());
                    }
                }
                if !valid_qualified_name(&name, &bindings)
                    || attrs.iter().any(|(name, _)| !is_namespace_declaration(name) && !valid_qualified_name(name, &bindings))
                {
                    return Err(malformed("invalid or undeclared qualified name"));
                }
            }
            Event::End { .. } => {
                if let Some(start) = scopes.pop() {
                    bindings.truncate(start);
                }
            }
            Event::Text(text) => {
                if !text.chars().all(is_xml_char) || xml.get(events.span()).is_some_and(|source| source.contains("]]>")) {
                    return Err(malformed("invalid XML character data"));
                }
            }
            Event::CData(text) if !text.chars().all(is_xml_char) => return Err(malformed("invalid CDATA characters")),
            Event::Comment(text) if text.contains("--") || text.ends_with('-') || !text.chars().all(is_xml_char) => {
                return Err(malformed("invalid XML comment"));
            }
            Event::Pi { target, data } if !is_name(&target) || target.eq_ignore_ascii_case("xml") || !data.chars().all(is_xml_char) => {
                return Err(malformed("invalid XML processing instruction"));
            }
            _ => {}
        }
    }
    Ok(())
}
