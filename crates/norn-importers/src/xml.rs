//! A small, bounded XML tree for declarative source files.
//!
//! Elements keep their qualified name, their attributes in document order and
//! their text. DTDs are refused, so no entity can expand; namespaces are not
//! resolved, because the formats read here use none beyond the schema-location
//! attribute on the root.

use quick_xml::{Reader, events::Event};

/// Largest file read.
pub(crate) const MAX_FILE_BYTES: usize = 16 * 1024 * 1024;
/// Deepest nesting accepted.
pub(crate) const MAX_DEPTH: usize = 64;
/// Most elements accepted in one file.
pub(crate) const MAX_ELEMENTS: usize = 500_000;
/// Most attributes accepted on one element.
pub(crate) const MAX_ATTRIBUTES: usize = 256;

/// One element.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Element {
    /// The qualified name as written.
    pub(crate) name: String,
    /// Attributes in document order, values decoded and normalized.
    pub(crate) attributes: Vec<(String, String)>,
    /// Child elements in document order.
    pub(crate) children: Vec<Element>,
    /// Text content, whitespace runs collapsed to one space.
    pub(crate) text: String,
}

impl Element {
    /// The value of attribute `name`.
    #[must_use]
    pub(crate) fn get(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    /// Children named `name`.
    pub(crate) fn named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Self> {
        self.children.iter().filter(move |child| child.name == name)
    }

    /// The collapsed text of the first child named `name`, when not blank.
    #[must_use]
    pub(crate) fn child_text(&self, name: &str) -> Option<String> {
        self.named(name)
            .next()
            .map(|child| child.text.clone())
            .filter(|text| !text.is_empty())
    }
}

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Parse `bytes` into its root element.
///
/// # Errors
/// Oversized, non-UTF-8, malformed, DTD-bearing or too deep or large input.
pub(crate) fn parse(bytes: &[u8]) -> Result<Element, String> {
    if bytes.len() > MAX_FILE_BYTES {
        return Err("file exceeds 16 MiB".into());
    }
    std::str::from_utf8(bytes).map_err(|error| error.to_string())?;
    let mut reader = Reader::from_reader(bytes);
    let mut stack: Vec<Element> = Vec::new();
    let mut root: Option<Element> = None;
    let mut count = 0_usize;
    loop {
        let event = reader.read_event().map_err(|error| error.to_string())?;
        let empty = matches!(&event, Event::Empty(_));
        match event {
            Event::Start(start) | Event::Empty(start) => {
                if root.is_some() {
                    return Err("content after the root element".into());
                }
                count = count.saturating_add(1);
                if count > MAX_ELEMENTS {
                    return Err("too many elements".into());
                }
                if stack.len() >= MAX_DEPTH {
                    return Err("nesting too deep".into());
                }
                let name = std::str::from_utf8(start.name().as_ref())
                    .map_err(|error| error.to_string())?
                    .to_owned();
                let attributes = attributes(&start, reader.decoder())?;
                let element = Element {
                    name,
                    attributes,
                    children: Vec::new(),
                    text: String::new(),
                };
                if empty {
                    close(element, &mut stack, &mut root);
                } else {
                    stack.push(element);
                }
            }
            Event::End(_) => {
                let element = stack.pop().ok_or("unexpected closing tag")?;
                close(element, &mut stack, &mut root);
            }
            Event::Text(text) => {
                let value = text
                    .xml_content(quick_xml::XmlVersion::Implicit1_0)
                    .map_err(|error| error.to_string())?;
                append(&mut stack, &value)?;
            }
            Event::CData(text) => {
                let value = text.decode().map_err(|error| error.to_string())?;
                append(&mut stack, &value)?;
            }
            Event::GeneralRef(reference) => {
                let name =
                    std::str::from_utf8(reference.as_ref()).map_err(|error| error.to_string())?;
                let encoded = format!("&{name};");
                let value =
                    quick_xml::escape::unescape(&encoded).map_err(|error| error.to_string())?;
                append(&mut stack, &value)?;
            }
            Event::Decl(declaration) => {
                if declaration
                    .version()
                    .map_err(|error| error.to_string())?
                    .as_ref()
                    != b"1.0"
                {
                    return Err("only XML 1.0 is supported".into());
                }
            }
            Event::DocType(_) => return Err("a DTD is not accepted".into()),
            Event::Eof => break,
            Event::Comment(_) | Event::PI(_) => {}
        }
    }
    if !stack.is_empty() {
        return Err("unclosed element".into());
    }
    let mut root = root.ok_or("no root element")?;
    finish(&mut root);
    Ok(root)
}

/// The attributes of `start`, decoded and normalized: at most
/// [`MAX_ATTRIBUTES`], each name once.
fn attributes(
    start: &quick_xml::events::BytesStart<'_>,
    decoder: quick_xml::encoding::Decoder,
) -> Result<Vec<(String, String)>, String> {
    let mut attributes = Vec::new();
    let mut keys = std::collections::BTreeSet::new();
    for attribute in start.attributes() {
        let attribute = attribute.map_err(|error| error.to_string())?;
        if attributes.len() >= MAX_ATTRIBUTES {
            return Err("too many attributes on one element".into());
        }
        let key = std::str::from_utf8(attribute.key.as_ref())
            .map_err(|error| error.to_string())?
            .to_owned();
        if !keys.insert(key.clone()) {
            return Err(format!("duplicate attribute {key}"));
        }
        let value = attribute
            .decoded_and_normalized_value(quick_xml::XmlVersion::Implicit1_0, decoder)
            .map_err(|error| error.to_string())?
            .into_owned();
        attributes.push((key, value));
    }
    Ok(attributes)
}

fn close(element: Element, stack: &mut [Element], root: &mut Option<Element>) {
    match stack.last_mut() {
        Some(parent) => parent.children.push(element),
        None => *root = Some(element),
    }
}

fn append(stack: &mut [Element], text: &str) -> Result<(), String> {
    match stack.last_mut() {
        Some(element) => {
            element.text.push_str(text);
            Ok(())
        }
        None if text.trim().is_empty() => Ok(()),
        None => Err("text outside the root element".into()),
    }
}

/// Collapse whitespace in every element's text, once, after parsing.
fn finish(element: &mut Element) {
    element.text = collapse(&element.text);
    for child in &mut element.children {
        finish(child);
    }
}
