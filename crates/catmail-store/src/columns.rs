//! Conversions for column conventions shared by the repositories.

use std::str::FromStr;

use catmail_types::BlobHash;
use rusqlite::Row;
use rusqlite::types::Type;
use serde::Serialize;
use serde::de::DeserializeOwned;

/// Reads a TEXT column holding an enum's string form.
pub(crate) fn parse_text<T: FromStr<Err = String>>(
    row: &Row<'_>,
    col: &str,
) -> rusqlite::Result<T> {
    parse_value(row, col, row.get(col)?)
}

pub(crate) fn parse_opt_text<T: FromStr<Err = String>>(
    row: &Row<'_>,
    col: &str,
) -> rusqlite::Result<Option<T>> {
    let value: Option<String> = row.get(col)?;
    value.map(|v| parse_value(row, col, v)).transpose()
}

/// Reads a nullable blob hash column. A malformed hash is an error rather than `None`,
/// which would make a referenced blob look unreferenced.
pub(crate) fn opt_blob_hash(row: &Row<'_>, col: &str) -> rusqlite::Result<Option<BlobHash>> {
    let value: Option<String> = row.get(col)?;
    value
        .map(|v| BlobHash::try_from(v).map_err(|e| conversion_error(row, col, e.into())))
        .transpose()
}

pub(crate) fn blob_hash_at(row: &Row<'_>, idx: usize) -> rusqlite::Result<BlobHash> {
    let value: String = row.get(idx)?;
    BlobHash::try_from(value)
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(idx, Type::Text, e.into()))
}

/// Reads a TEXT column holding JSON (address lists, keyword lists).
pub(crate) fn json<T: DeserializeOwned>(row: &Row<'_>, col: &str) -> rusqlite::Result<T> {
    let value: String = row.get(col)?;
    serde_json::from_str(&value).map_err(|e| conversion_error(row, col, e.into()))
}

pub(crate) fn to_json<T: Serialize + ?Sized>(value: &T) -> String {
    serde_json::to_string(value).expect("plain data serializes")
}

fn parse_value<T: FromStr<Err = String>>(
    row: &Row<'_>,
    col: &str,
    value: String,
) -> rusqlite::Result<T> {
    value
        .parse()
        .map_err(|e: String| conversion_error(row, col, e.into()))
}

/// All conventions here are TEXT columns.
fn conversion_error(
    row: &Row<'_>,
    col: &str,
    err: Box<dyn std::error::Error + Send + Sync>,
) -> rusqlite::Error {
    let idx = row.as_ref().column_index(col).unwrap_or_default();
    rusqlite::Error::FromSqlConversionFailure(idx, Type::Text, err)
}
