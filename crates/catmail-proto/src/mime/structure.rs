use std::collections::HashMap;

use mail_parser::MessageParser;

use super::content::{Content, extract};

/// A message's MIME structure as IMAP BODYSTRUCTURE describes it. Strings are bytes as
/// the server sent them; parameter values may still be RFC 2047 or RFC 2231 encoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MimeNode {
    Multipart {
        subtype: Vec<u8>,
        children: Vec<MimeNode>,
    },
    /// A single part. An attached message (`message/rfc822`) is one leaf; its own parts
    /// are not described.
    Leaf(Leaf),
}

/// MIME parameters (`charset`, `name`, …) as name/value pairs.
pub type Params = Vec<(Vec<u8>, Vec<u8>)>;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Leaf {
    pub media_type: Vec<u8>,
    pub subtype: Vec<u8>,
    pub params: Params,
    pub content_id: Option<Vec<u8>>,
    pub encoding: Vec<u8>,
    /// Encoded size in octets.
    pub size: u32,
    /// Disposition type and its parameters.
    pub disposition: Option<(Vec<u8>, Params)>,
}

impl Leaf {
    fn is(&self, media_type: &str, subtype: &str) -> bool {
        self.media_type.eq_ignore_ascii_case(media_type.as_bytes())
            && self.subtype.eq_ignore_ascii_case(subtype.as_bytes())
    }

    fn is_attachment(&self) -> bool {
        self.disposition
            .as_ref()
            .is_some_and(|(kind, _)| kind.eq_ignore_ascii_case(b"attachment"))
    }

    /// Decoded size, roughly: base64 carries 57 bytes per 78-octet line.
    fn estimated_size(&self) -> u32 {
        if self.encoding.eq_ignore_ascii_case(b"base64") {
            (u64::from(self.size) * 57 / 78) as u32
        } else {
            self.size
        }
    }
}

impl MimeNode {
    /// Sections of the parts that may be shown as the body: text/plain and text/html
    /// parts that aren't attachments, up to `max_size` octets each.
    pub fn text_sections(&self, max_size: u32) -> Vec<String> {
        self.leaves()
            .into_iter()
            .filter(|(_, leaf)| {
                (leaf.is("text", "plain") || leaf.is("text", "html"))
                    && !leaf.is_attachment()
                    && leaf.size <= max_size
            })
            .map(|(section, _)| section)
            .collect()
    }

    /// The content of the message, from its structure and the parts in `fetched` (by
    /// section, still transfer-encoded). Parts that weren't fetched have no data.
    ///
    /// Builds a stand-in message with the same structure, the fetched parts in place and
    /// the others empty, and parses that the same way as a whole raw message, so both
    /// ways of downloading choose the same body parts and decode them the same way.
    pub fn content(&self, fetched: &HashMap<String, Vec<u8>>) -> Content {
        let mut raw = Vec::new();
        let boundary = boundary(fetched);
        let mut counter = 0;
        match self {
            MimeNode::Multipart { .. } => {
                raw.extend_from_slice(b"MIME-Version: 1.0\r\n");
                self.write(&mut raw, "", fetched, &boundary, &mut counter);
            }
            MimeNode::Leaf(_) => self.write(&mut raw, "1", fetched, &boundary, &mut counter),
        }
        let Some(message) = MessageParser::new().parse(&raw) else {
            return Content::default();
        };
        let mut content = extract(&message, &raw);
        let leaves: HashMap<String, &Leaf> = self.leaves().into_iter().collect();
        for part in &mut content.parts {
            let Some(leaf) = leaves.get(&part.section) else {
                // The stand-in didn't parse like the structure; whatever this is, it
                // wasn't downloaded.
                part.data = None;
                continue;
            };
            part.transfer_encoding =
                Some(lossy(&leaf.encoding).to_ascii_lowercase()).filter(|e| !e.is_empty());
            if !fetched.contains_key(&part.section) {
                part.data = None;
                part.size = Some(leaf.estimated_size());
            }
        }
        content
    }

    /// Leaves with their sections; a lone leaf at the top is section `1`.
    fn leaves(&self) -> Vec<(String, &Leaf)> {
        fn walk<'a>(node: &'a MimeNode, section: String, out: &mut Vec<(String, &'a Leaf)>) {
            match node {
                MimeNode::Leaf(leaf) => out.push((section, leaf)),
                MimeNode::Multipart { children, .. } => {
                    for (i, child) in children.iter().enumerate() {
                        let n = i + 1;
                        let child_section = if section.is_empty() {
                            n.to_string()
                        } else {
                            format!("{section}.{n}")
                        };
                        walk(child, child_section, out);
                    }
                }
            }
        }
        let mut out = Vec::new();
        match self {
            MimeNode::Leaf(leaf) => out.push(("1".to_owned(), leaf)),
            MimeNode::Multipart { .. } => walk(self, String::new(), &mut out),
        }
        out
    }

    fn write(
        &self,
        out: &mut Vec<u8>,
        section: &str,
        fetched: &HashMap<String, Vec<u8>>,
        boundary: &str,
        counter: &mut usize,
    ) {
        match self {
            MimeNode::Multipart { subtype, children } => {
                *counter += 1;
                let boundary_here = format!("{boundary}-{counter}");
                out.extend_from_slice(b"Content-Type: multipart/");
                out.extend_from_slice(&token(subtype, b"mixed"));
                out.extend_from_slice(format!("; boundary=\"{boundary_here}\"\r\n\r\n").as_bytes());
                for (i, child) in children.iter().enumerate() {
                    let n = i + 1;
                    let child_section = if section.is_empty() {
                        n.to_string()
                    } else {
                        format!("{section}.{n}")
                    };
                    out.extend_from_slice(format!("--{boundary_here}\r\n").as_bytes());
                    child.write(out, &child_section, fetched, boundary, counter);
                    out.extend_from_slice(b"\r\n");
                }
                out.extend_from_slice(format!("--{boundary_here}--\r\n").as_bytes());
            }
            MimeNode::Leaf(leaf) => {
                // A leaf must stay a leaf: written as multipart (with a boundary the server
                // chose) it would swallow the parts after it and shift their sections.
                let mut media_type = token(&leaf.media_type, b"application");
                if media_type.eq_ignore_ascii_case(b"multipart") {
                    media_type = b"application".to_vec();
                }
                out.extend_from_slice(b"Content-Type: ");
                out.extend_from_slice(&media_type);
                out.push(b'/');
                out.extend_from_slice(&token(&leaf.subtype, b"octet-stream"));
                let params: Params = leaf
                    .params
                    .iter()
                    .filter(|(name, _)| !name.eq_ignore_ascii_case(b"boundary"))
                    .cloned()
                    .collect();
                write_params(out, &params);
                out.extend_from_slice(b"\r\n");
                let body = fetched.get(section);
                // A part that wasn't fetched is an empty base64 body, which mail-parser
                // never tries to read as an attached message.
                out.extend_from_slice(b"Content-Transfer-Encoding: ");
                match body {
                    Some(_) => out.extend_from_slice(&token(&leaf.encoding, b"7bit")),
                    None => out.extend_from_slice(b"base64"),
                }
                out.extend_from_slice(b"\r\n");
                if let Some(id) = &leaf.content_id {
                    out.extend_from_slice(b"Content-ID: ");
                    out.extend_from_slice(&single_line(id));
                    out.extend_from_slice(b"\r\n");
                }
                if let Some((kind, params)) = &leaf.disposition {
                    out.extend_from_slice(b"Content-Disposition: ");
                    out.extend_from_slice(&token(kind, b"attachment"));
                    write_params(out, params);
                    out.extend_from_slice(b"\r\n");
                }
                out.extend_from_slice(b"\r\n");
                if let Some(body) = body {
                    out.extend_from_slice(body);
                }
            }
        }
    }
}

/// A boundary that occurs in none of the fetched parts.
fn boundary(fetched: &HashMap<String, Vec<u8>>) -> String {
    let mut boundary = "=_catmail_skeleton".to_owned();
    while fetched.values().any(|body| {
        body.windows(boundary.len())
            .any(|w| w == boundary.as_bytes())
    }) {
        boundary.push('_');
    }
    boundary
}

/// `; name="value"` for each parameter. RFC 2231 extended values (`name*=utf-8''…`)
/// are written bare, as they must be.
fn write_params(out: &mut Vec<u8>, params: &[(Vec<u8>, Vec<u8>)]) {
    for (name, value) in params {
        let name = token(name, b"");
        if name.is_empty() {
            continue;
        }
        out.extend_from_slice(b"; ");
        out.extend_from_slice(&name);
        out.push(b'=');
        let value = single_line(value);
        if name.ends_with(b"*") {
            out.extend(value.iter().filter(|b| !b" ;\"".contains(b)));
        } else {
            out.push(b'"');
            for b in value {
                if b == b'"' || b == b'\\' {
                    out.push(b'\\');
                }
                out.push(b);
            }
            out.push(b'"');
        }
    }
}

/// `value` reduced to header token characters, or `default` if nothing is left.
fn token(value: &[u8], default: &[u8]) -> Vec<u8> {
    let token: Vec<u8> = value
        .iter()
        .copied()
        .filter(|b| b.is_ascii_graphic() && !b"()<>@,;:\\\"/[]?=".contains(b))
        .collect();
    if token.is_empty() {
        default.to_vec()
    } else {
        token
    }
}

/// Line breaks would end the header (or forge new ones).
fn single_line(value: &[u8]) -> Vec<u8> {
    value
        .iter()
        .map(|&b| if b == b'\r' || b == b'\n' { b' ' } else { b })
        .collect()
}

fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mime::parse_message;

    fn leaf(media_type: &str, subtype: &str, encoding: &str, size: u32) -> Leaf {
        Leaf {
            media_type: media_type.into(),
            subtype: subtype.into(),
            encoding: encoding.into(),
            size,
            ..Default::default()
        }
    }

    fn params(pairs: &[(&str, &[u8])]) -> Params {
        pairs
            .iter()
            .map(|(k, v)| (k.as_bytes().to_vec(), v.to_vec()))
            .collect()
    }

    /// multipart/mixed (alternative (plain, html), pdf, related-image, attached message)
    fn tree() -> MimeNode {
        let mut plain = leaf("TEXT", "PLAIN", "BASE64", 12);
        plain.params = params(&[("CHARSET", b"gbk")]);
        let mut html = leaf("TEXT", "HTML", "QUOTED-PRINTABLE", 40);
        html.params = params(&[("CHARSET", b"utf-8")]);
        let mut pdf = leaf("APPLICATION", "PDF", "BASE64", 7_800_000);
        pdf.params = params(&[("NAME", b"=?UTF-8?B?5oql5ZGKLnBkZg==?=")]);
        pdf.disposition = Some((b"ATTACHMENT".to_vec(), Vec::new()));
        let mut png = leaf("IMAGE", "PNG", "BASE64", 780);
        png.content_id = Some(b"<logo@x>".to_vec());
        png.disposition = Some((b"INLINE".to_vec(), Vec::new()));
        let mut note = leaf("TEXT", "PLAIN", "7BIT", 4);
        note.disposition = Some((
            b"attachment".to_vec(),
            params(&[("filename*", b"utf-8''%E7%AC%94%E8%AE%B0.txt")]),
        ));
        MimeNode::Multipart {
            subtype: b"MIXED".to_vec(),
            children: vec![
                MimeNode::Multipart {
                    subtype: b"ALTERNATIVE".to_vec(),
                    children: vec![MimeNode::Leaf(plain), MimeNode::Leaf(html)],
                },
                MimeNode::Leaf(pdf),
                MimeNode::Leaf(png),
                MimeNode::Leaf(leaf("MESSAGE", "RFC822", "7BIT", 500)),
                MimeNode::Leaf(note),
            ],
        }
    }

    #[test]
    fn picks_text_sections() {
        assert_eq!(tree().text_sections(1_000), ["1.1", "1.2"]);
        assert_eq!(tree().text_sections(20), ["1.1"]);
        let single = MimeNode::Leaf(leaf("text", "html", "7bit", 10));
        assert_eq!(single.text_sections(100), ["1"]);
    }

    #[test]
    fn builds_content_from_structure_and_fetched_parts() {
        let fetched = HashMap::from([
            ("1.1".to_owned(), b"x+uy6crV\r\n".to_vec()),
            (
                "1.2".to_owned(),
                b"<p>=E8=AF=B7=E6=9F=A5=E6=94=B6</p><img src=3D\"cid:logo@x\">".to_vec(),
            ),
        ]);
        let content = tree().content(&fetched);
        assert_eq!(content.text.as_deref(), Some("请查收"));
        assert!(content.html.unwrap().contains("<p>请查收</p>"));

        let parts: Vec<_> = content
            .parts
            .iter()
            .map(|p| {
                (
                    p.section.as_str(),
                    p.mime_type.as_str(),
                    p.filename.as_deref(),
                    p.inline,
                    p.transfer_encoding.as_deref(),
                    p.size,
                    p.data.is_some(),
                )
            })
            .collect();
        assert_eq!(
            parts,
            [
                (
                    "2",
                    "application/pdf",
                    Some("报告.pdf"),
                    false,
                    Some("base64"),
                    Some(5_700_000),
                    false
                ),
                (
                    "3",
                    "image/png",
                    None,
                    true,
                    Some("base64"),
                    Some(570),
                    false
                ),
                (
                    "4",
                    "message/rfc822",
                    None,
                    false,
                    Some("7bit"),
                    Some(500),
                    false
                ),
                (
                    "5",
                    "text/plain",
                    Some("笔记.txt"),
                    false,
                    Some("7bit"),
                    Some(4),
                    false
                ),
            ]
        );
        assert_eq!(content.parts[1].content_id.as_deref(), Some("logo@x"));
    }

    #[test]
    fn matches_parsing_the_whole_message() {
        // The same message, once whole and once rebuilt from its structure.
        let raw = b"Content-Type: multipart/mixed; boundary=m\r\n\r\n\
            --m\r\n\
            Content-Type: text/plain; charset=utf-8\r\n\r\n\
            Hello there\r\n\
            --m\r\n\
            Content-Type: application/zip; name=\"a \\\"b\\\".zip\"\r\n\
            Content-Transfer-Encoding: base64\r\n\r\n\
            UEsDBA==\r\n\
            --m--\r\n";
        let mut zip = leaf("application", "zip", "base64", 8);
        zip.params = params(&[("name", b"a \"b\".zip")]);
        let tree = MimeNode::Multipart {
            subtype: b"mixed".to_vec(),
            children: vec![
                MimeNode::Leaf(leaf("text", "plain", "7bit", 13)),
                MimeNode::Leaf(zip),
            ],
        };
        let fetched = HashMap::from([
            ("1".to_owned(), b"Hello there".to_vec()),
            ("2".to_owned(), b"UEsDBA==".to_vec()),
        ]);
        let whole = parse_message(raw);
        let rebuilt = tree.content(&fetched);
        assert_eq!(rebuilt, whole);
        assert_eq!(whole.parts[0].filename.as_deref(), Some("a \"b\".zip"));
    }

    #[test]
    fn hostile_structure_values_stay_inside_their_header() {
        let mut evil = leaf("text", "plain", "7bit", 5);
        evil.params = params(&[("charset", b"utf-8\"\r\n\r\nInjected body")]);
        evil.content_id = Some(b"<x>\r\nContent-Type: text/html".to_vec());
        let fetched = HashMap::from([("1".to_owned(), b"body!".to_vec())]);
        let content = MimeNode::Leaf(evil).content(&fetched);
        assert_eq!(content.text.as_deref(), Some("body!"));
        assert_eq!(content.html, None);

        // Garbage where a token belongs makes an unknown type, nothing more.
        let garbage = MimeNode::Leaf(leaf("text\r\nX-Evil: 1", "plain", "7bit", 5));
        let content = garbage.content(&fetched);
        assert_eq!(content.text, None);
        assert_eq!(content.parts[0].mime_type, "textx-evil1/plain");
    }

    #[test]
    fn a_leaf_claiming_to_be_multipart_stays_one_part() {
        // A hostile structure: a leaf of type MULTIPART whose boundary is the
        // stand-in's own.
        let mut fake = leaf("MULTIPART", "MIXED", "7BIT", 10);
        fake.params = params(&[("BOUNDARY", b"=_catmail_skeleton-1")]);
        let mut pdf = leaf("application", "pdf", "base64", 100);
        pdf.params = params(&[("name", b"a.pdf")]);
        let tree = MimeNode::Multipart {
            subtype: b"mixed".to_vec(),
            children: vec![
                MimeNode::Leaf(leaf("text", "plain", "7bit", 2)),
                MimeNode::Leaf(fake),
                MimeNode::Leaf(pdf),
            ],
        };
        let fetched = HashMap::from([("1".to_owned(), b"hi".to_vec())]);
        let content = tree.content(&fetched);
        let parts: Vec<_> = content
            .parts
            .iter()
            .map(|p| (p.section.as_str(), p.data.is_some()))
            .collect();
        assert_eq!(parts, [("2", false), ("3", false)]);
        assert_eq!(content.parts[1].filename.as_deref(), Some("a.pdf"));
    }
}
