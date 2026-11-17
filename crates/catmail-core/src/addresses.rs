//! The addresses of a draft's To / Cc / Bcc fields: read from what the user types, shown
//! so that they read back, and checked to be usable for sending.

use catmail_proto::smtp;
use catmail_types::{Address, DraftContent};

use crate::{Error, Result};

/// Parses what a user types into a To / Cc / Bcc field: addresses separated by commas
/// or semicolons, each either bare or with a display name (`张三 <zhang@example.com>`,
/// `"Doe, Jane" <jane@example.com>`). Every entry must be exactly that: anything else is
/// an error, never a recipient silently left out. Every address must be usable for
/// sending.
pub fn parse_addresses(input: &str) -> Result<Vec<Address>> {
    if input.contains(['\r', '\n']) {
        return Err(Error::InvalidInput("addresses must be on one line".into()));
    }
    split_entries(input)
        .into_iter()
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .map(|entry| {
            let address = parse_entry(entry)
                .ok_or_else(|| Error::InvalidInput(format!("`{entry}` is not an email address")))?;
            check_address(&address.email)?;
            Ok(address)
        })
        .collect()
}

/// The addresses as a To / Cc / Bcc field shows them, so that [`parse_addresses`] reads
/// them back: a name with a separator, quote, bracket or backslash in it is quoted
/// (`"Doe, Jane" <jane@example.com>`).
pub fn format_addresses(list: &[Address]) -> String {
    list.iter()
        .map(
            |a| match a.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
                None => a.email.clone(),
                Some(name) if name.contains([',', ';', '"', '<', '>', '\\']) => {
                    let escaped = name.replace('\\', "\\\\").replace('"', "\\\"");
                    format!("\"{escaped}\" <{}>", a.email)
                }
                Some(name) => format!("{name} <{}>", a.email),
            },
        )
        .collect::<Vec<_>>()
        .join(", ")
}

/// `input` split at the commas and semicolons that separate addresses, not those inside
/// a quoted name (`"Doe, Jane"`) or angle brackets.
fn split_entries(input: &str) -> Vec<&str> {
    let (mut quoted, mut escaped, mut bracketed, mut start) = (false, false, false, 0);
    let mut entries = Vec::new();
    for (i, c) in input.char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' if quoted => escaped = true,
            '"' if !bracketed => quoted = !quoted,
            '<' if !quoted => bracketed = true,
            '>' if !quoted => bracketed = false,
            ',' | ';' if !quoted && !bracketed => {
                entries.push(&input[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    entries.push(&input[start..]);
    entries
}

/// One trimmed entry: `addr`, `<addr>`, `Name <addr>` or `"Name" <addr>`. `None` for
/// anything else, including text after the address and a second address.
fn parse_entry(entry: &str) -> Option<Address> {
    let Some(rest) = entry.strip_suffix('>') else {
        // Bare; `check_address` refuses spaces, brackets and quotes.
        return Some(Address {
            name: None,
            email: entry.to_owned(),
        });
    };
    let (name, email) = rest.rsplit_once('<')?;
    if email.is_empty() {
        return None;
    }
    let name = name.trim();
    let name = match name.strip_prefix('"') {
        Some(quoted) => {
            let mut unquoted = String::new();
            let mut chars = quoted.strip_suffix('"')?.chars();
            while let Some(c) = chars.next() {
                match c {
                    '\\' => unquoted.push(chars.next()?),
                    '"' => return None,
                    c => unquoted.push(c),
                }
            }
            unquoted
        }
        None if name.contains(['"', '<', '>']) => return None,
        None => name.to_owned(),
    };
    Some(Address {
        name: (!name.is_empty()).then_some(name),
        email: email.to_owned(),
    })
}

/// Addresses must be usable as they are in the SMTP envelope. Checked when saving, so a
/// mistake shows up while the user is still writing.
pub(crate) fn validate(content: &DraftContent) -> Result<()> {
    for address in content.recipients() {
        check_address(&address.email)?;
    }
    Ok(())
}

pub(crate) fn check_address(email: &str) -> Result<()> {
    if !email.is_ascii() {
        return Err(Error::InvalidInput(format!(
            "`{email}`: addresses with non-ASCII characters are not supported yet"
        )));
    }
    // Beyond the envelope's rules, nothing that would split the address in a header.
    if !smtp::is_envelope_address(email) || email.contains(['(', ')', ',', ';']) {
        return Err(Error::InvalidInput(format!(
            "`{email}` is not an email address"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(name: Option<&str>, email: &str) -> Address {
        Address {
            name: name.map(String::from),
            email: email.into(),
        }
    }

    #[test]
    fn parses_address_fields() {
        let parsed = parse_addresses(
            r#"张三 <zhang@example.com>, "Doe, Jane" <jane@example.com>,b@example.com"#,
        )
        .unwrap();
        assert_eq!(
            parsed,
            [
                addr(Some("张三"), "zhang@example.com"),
                addr(Some("Doe, Jane"), "jane@example.com"),
                addr(None, "b@example.com"),
            ]
        );
        assert_eq!(parse_addresses("  ").unwrap(), []);
        assert_eq!(
            parse_addresses(r#"a@example.com; <b@example.com>, "O\"Brien" <c@example.com>,"#)
                .unwrap(),
            [
                addr(None, "a@example.com"),
                addr(None, "b@example.com"),
                addr(Some("O\"Brien"), "c@example.com"),
            ]
        );
        for bad in [
            "bob@example.com, Carol",
            "bob@example.com; Carol",
            "bob@example.com,,, nobody",
            "Bob <bob@example.com> Carol",
            "Bob <bob@example.com> <carol@example.com>",
            "Team: bob@example.com",
            "bob@example.com (work)",
            "Carol <>",
            "nobody",
            "a@example.com\r\nBcc: x@example.com",
            "Bob <b c@example.com>",
        ] {
            assert!(parse_addresses(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn formatted_addresses_parse_back() {
        let list = [
            addr(Some("Doe, Jane"), "jane@example.com"),
            addr(Some("O\"Brien"), "o@example.com"),
            addr(Some(r"back\slash"), "b@example.com"),
            addr(Some("semi;colon <x>"), "s@example.com"),
            addr(Some("张三"), "zhang@example.com"),
            addr(None, "plain@example.com"),
        ];
        let field = format_addresses(&list);
        assert!(
            field.starts_with(r#""Doe, Jane" <jane@example.com>, "O\"Brien""#),
            "{field}"
        );
        assert_eq!(parse_addresses(&field).unwrap(), list);
    }

    #[test]
    fn addresses_must_fit_the_envelope() {
        assert!(check_address("a@example.com").is_ok());
        for bad in [
            "",
            "a",
            "a@",
            "a b@example.com",
            "<a@example.com>",
            "张三@example.com",
        ] {
            assert!(check_address(bad).is_err(), "{bad:?}");
        }
    }
}
