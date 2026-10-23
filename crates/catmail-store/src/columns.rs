//! Conversions for column conventions shared by the repositories.

use std::str::FromStr;

use catmail_types::ServerConfig;
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

/// Reads a server's `{prefix}_host`, `{prefix}_port` and `{prefix}_security` columns.
pub(crate) fn server(row: &Row<'_>, prefix: &str) -> rusqlite::Result<ServerConfig> {
    Ok(ServerConfig {
        host: row.get(format!("{prefix}_host").as_str())?,
        port: row.get(format!("{prefix}_port").as_str())?,
        security: parse_text(row, &format!("{prefix}_security"))?,
    })
}

/// Reads a size limit a server announced (SMTP SIZE, IMAP APPENDLIMIT).
pub(crate) fn size_limit<I: rusqlite::RowIndex>(
    row: &Row<'_>,
    col: I,
) -> rusqlite::Result<Option<u64>> {
    Ok(row.get::<_, Option<i64>>(col)?.map(|l| l as u64))
}

/// A size limit as stored: larger than SQLite holds is no limit worth telling apart.
pub(crate) fn size_limit_column(limit: Option<u64>) -> Option<i64> {
    limit.map(|l| i64::try_from(l).unwrap_or(i64::MAX))
}

/// Reads a TEXT column holding JSON (address lists, keyword lists).
pub(crate) fn json<T: DeserializeOwned>(row: &Row<'_>, col: &str) -> rusqlite::Result<T> {
    let value: String = row.get(col)?;
    serde_json::from_str(&value).map_err(|e| conversion_error(row, col, e.into()))
}

pub(crate) fn to_json<T: Serialize + ?Sized>(value: &T) -> String {
    serde_json::to_string(value).expect("plain data serializes")
}

/// Reads a Message-ID list (References), stored space-separated without brackets.
pub(crate) fn id_list(row: &Row<'_>, col: &str) -> rusqlite::Result<Vec<String>> {
    let value: Option<String> = row.get(col)?;
    Ok(value
        .map(|ids| ids.split_whitespace().map(String::from).collect())
        .unwrap_or_default())
}

/// NULL for an empty list.
pub(crate) fn to_id_list(ids: &[String]) -> Option<String> {
    (!ids.is_empty()).then(|| ids.join(" "))
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

/// A TEXT column holding a value that doesn't parse.
pub(crate) fn conversion_error(
    row: &Row<'_>,
    col: &str,
    err: Box<dyn std::error::Error + Send + Sync>,
) -> rusqlite::Error {
    let idx = row.as_ref().column_index(col).unwrap_or_default();
    rusqlite::Error::FromSqlConversionFailure(idx, Type::Text, err)
}
