//! BODYSTRUCTURE in library-neutral terms.

use imap_proto::{
    BodyContentCommon, BodyContentSinglePart, BodyParams, BodyStructure, ContentEncoding,
};

use crate::mime::{Leaf, MimeNode, Params};

pub(super) fn mime_node(structure: &BodyStructure<'_>) -> MimeNode {
    match structure {
        BodyStructure::Multipart { common, bodies, .. } => MimeNode::Multipart {
            subtype: bytes(&common.ty.subtype),
            children: bodies.iter().map(mime_node).collect(),
        },
        BodyStructure::Basic { common, other, .. }
        | BodyStructure::Text { common, other, .. }
        | BodyStructure::Message { common, other, .. } => MimeNode::Leaf(leaf(common, other)),
    }
}

fn leaf(common: &BodyContentCommon<'_>, other: &BodyContentSinglePart<'_>) -> Leaf {
    Leaf {
        media_type: bytes(&common.ty.ty),
        subtype: bytes(&common.ty.subtype),
        params: pairs(&common.ty.params),
        content_id: other.id.as_deref().map(bytes),
        encoding: encoding(&other.transfer_encoding).as_bytes().to_vec(),
        size: other.octets,
        disposition: common
            .disposition
            .as_ref()
            .map(|d| (bytes(&d.ty), pairs(&d.params))),
    }
}

fn encoding<'a>(encoding: &'a ContentEncoding<'_>) -> &'a str {
    match encoding {
        ContentEncoding::SevenBit => "7BIT",
        ContentEncoding::EightBit => "8BIT",
        ContentEncoding::Binary => "BINARY",
        ContentEncoding::Base64 => "BASE64",
        ContentEncoding::QuotedPrintable => "QUOTED-PRINTABLE",
        ContentEncoding::Other(other) => other,
    }
}

fn bytes(s: &str) -> Vec<u8> {
    s.as_bytes().to_vec()
}

fn pairs(params: &BodyParams<'_>) -> Params {
    params
        .iter()
        .flatten()
        .map(|(k, v)| (bytes(k), bytes(v)))
        .collect()
}
