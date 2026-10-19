//! What a server derives from a raw message: BODYSTRUCTURE and body sections.

use mail_parser::{Attribute, Message, MessageParser, MessagePart, MimeHeaders, PartType};

use crate::imap::{literal, quote};

/// The BODYSTRUCTURE of `raw`, in wire form (may contain literals).
pub(crate) fn body_structure(raw: &[u8]) -> Vec<u8> {
    match MessageParser::new().parse(raw) {
        Some(message) => node(&message, 0, raw),
        None => b"(\"TEXT\" \"PLAIN\" NIL NIL NIL \"7BIT\" 0 0)".to_vec(),
    }
}

/// The content of section `spec` (`""` for the whole message, else `1.2`), still
/// transfer-encoded, as `BODY[spec]` returns it.
pub(crate) fn section(raw: &[u8], spec: &str) -> Option<Vec<u8>> {
    if spec.is_empty() {
        return Some(raw.to_vec());
    }
    let message = MessageParser::new().parse(raw)?;
    let mut id = 0u32;
    for (depth, n) in spec.split('.').enumerate() {
        let n: usize = n.parse().ok()?;
        match message.parts[id as usize].sub_parts() {
            Some(children) => id = *children.get(n.checked_sub(1)?)?,
            // A message that isn't multipart has just part 1.
            None if depth == 0 && n == 1 => {}
            None => return None,
        }
    }
    let part = &message.parts[id as usize];
    raw.get(part.offset_body as usize..part.offset_end as usize)
        .map(<[u8]>::to_vec)
}

fn node(message: &Message<'_>, id: u32, raw: &[u8]) -> Vec<u8> {
    let part = &message.parts[id as usize];
    let mut out = b"(".to_vec();
    if let Some(children) = part.sub_parts() {
        for &child in children {
            out.extend(node(message, child, raw));
        }
        out.push(b' ');
        out.extend(string(subtype(part).as_deref().unwrap_or("MIXED")));
        out.push(b')');
        return out;
    }

    let (kind, sub) = match part.content_type() {
        Some(ct) => (ct.ctype().to_owned(), subtype(part).unwrap_or_default()),
        None => ("text".to_owned(), "plain".to_owned()),
    };
    out.extend(string(&kind));
    out.push(b' ');
    out.extend(string(&sub));
    out.push(b' ');
    out.extend(list(part.content_type().and_then(|ct| ct.attributes())));
    out.push(b' ');
    out.extend(nstring(
        part.content_id().map(|id| format!("<{id}>")).as_deref(),
    ));
    out.extend(b" NIL ");
    out.extend(string(part.content_transfer_encoding().unwrap_or("7BIT")));
    let body = raw
        .get(part.offset_body as usize..part.offset_end as usize)
        .unwrap_or_default();
    out.extend(format!(" {}", body.len()).as_bytes());
    let lines = body.iter().filter(|&&b| b == b'\n').count();
    if let PartType::Message(nested) = &part.body {
        out.extend(b" (NIL NIL NIL NIL NIL NIL NIL NIL NIL NIL) ");
        out.extend(node(nested, 0, nested.raw_message()));
        out.extend(format!(" {lines}").as_bytes());
    } else if kind.eq_ignore_ascii_case("text") {
        out.extend(format!(" {lines}").as_bytes());
    }
    // Extension data: MD5, then disposition.
    out.extend(b" NIL ");
    match part.content_disposition() {
        Some(d) => {
            out.push(b'(');
            out.extend(string(d.ctype()));
            out.push(b' ');
            out.extend(list(d.attributes()));
            out.push(b')');
        }
        None => out.extend(b"NIL"),
    }
    out.push(b')');
    out
}

fn subtype(part: &MessagePart<'_>) -> Option<String> {
    part.content_type()?.subtype().map(str::to_owned)
}

fn list(attributes: Option<&[Attribute<'_>]>) -> Vec<u8> {
    let Some(attributes) = attributes.filter(|a| !a.is_empty()) else {
        return b"NIL".to_vec();
    };
    let mut out = b"(".to_vec();
    for (i, a) in attributes.iter().enumerate() {
        if i > 0 {
            out.push(b' ');
        }
        out.extend(string(&a.name));
        out.push(b' ');
        out.extend(string(&a.value));
    }
    out.push(b')');
    out
}

fn nstring(s: Option<&str>) -> Vec<u8> {
    s.map_or_else(|| b"NIL".to_vec(), string)
}

/// A quoted string, or a literal if it isn't plain ASCII (as with decoded non-ASCII
/// file names).
fn string(s: &str) -> Vec<u8> {
    if s.bytes().all(|b| b.is_ascii() && b != b'\r' && b != b'\n') {
        quote(s).into_bytes()
    } else {
        literal(s.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RAW: &[u8] = b"Content-Type: multipart/mixed; boundary=m\r\n\r\n\
        --m\r\n\
        Content-Type: text/plain; charset=utf-8\r\n\r\n\
        Hello\r\n\
        --m\r\n\
        Content-Type: application/pdf; name=\"r.pdf\"\r\n\
        Content-Disposition: attachment; filename=\"r.pdf\"\r\n\
        Content-Transfer-Encoding: base64\r\n\r\n\
        JVBERi0xLjQK\r\n\
        --m--\r\n";

    #[test]
    fn describes_and_cuts_sections() {
        let bs = String::from_utf8(body_structure(RAW)).unwrap();
        assert_eq!(
            bs,
            "((\"text\" \"plain\" (\"charset\" \"utf-8\") NIL NIL \"7BIT\" 5 0 NIL NIL)\
             (\"application\" \"pdf\" (\"name\" \"r.pdf\") NIL NIL \"base64\" 12 NIL \
             (\"attachment\" (\"filename\" \"r.pdf\"))) \"mixed\")"
        );
        assert_eq!(section(RAW, "1").unwrap(), b"Hello");
        assert_eq!(section(RAW, "2").unwrap(), b"JVBERi0xLjQK");
        assert_eq!(section(RAW, "3"), None);
        assert_eq!(section(RAW, "").unwrap(), RAW);
        let single = b"Subject: x\r\n\r\nbody\r\n";
        assert_eq!(section(single, "1").unwrap(), b"body\r\n");
        assert_eq!(section(single, "1.1"), None);
    }
}
