//! BODYSTRUCTURE in library-neutral terms.

use imap_next::imap_types::body::{BodyStructure, SpecificFields};
use imap_next::imap_types::core::{IString, NString};

use crate::mime::{Leaf, MimeNode, Params};

pub(super) fn mime_node(structure: &BodyStructure<'_>) -> MimeNode {
    match structure {
        BodyStructure::Multi {
            bodies, subtype, ..
        } => MimeNode::Multipart {
            subtype: bytes(subtype),
            children: bodies.as_ref().iter().map(mime_node).collect(),
        },
        BodyStructure::Single {
            body,
            extension_data,
        } => {
            let (media_type, subtype) = match &body.specific {
                SpecificFields::Basic { r#type, subtype } => (bytes(r#type), bytes(subtype)),
                SpecificFields::Message { .. } => (b"message".to_vec(), b"rfc822".to_vec()),
                SpecificFields::Text { subtype, .. } => (b"text".to_vec(), bytes(subtype)),
            };
            let disposition = extension_data
                .as_ref()
                .and_then(|e| e.tail.as_ref())
                .and_then(|d| d.disposition.as_ref())
                .map(|(kind, params)| (bytes(kind), pairs(params)));
            MimeNode::Leaf(Leaf {
                media_type,
                subtype,
                params: pairs(&body.basic.parameter_list),
                content_id: nstring(&body.basic.id),
                encoding: bytes(&body.basic.content_transfer_encoding),
                size: body.basic.size,
                disposition,
            })
        }
    }
}

fn bytes(s: &IString<'_>) -> Vec<u8> {
    s.as_ref().to_vec()
}

fn nstring(s: &NString<'_>) -> Option<Vec<u8>> {
    s.0.as_ref().map(bytes)
}

fn pairs(list: &[(IString<'_>, IString<'_>)]) -> Params {
    list.iter().map(|(k, v)| (bytes(k), bytes(v))).collect()
}
