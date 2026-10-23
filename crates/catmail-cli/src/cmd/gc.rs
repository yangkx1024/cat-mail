//! `catmail gc`: deleting stored files that nothing refers to.

use anyhow::Result;
use catmail_core::{MailClient, display};

use crate::json::{self, JsonGc};

pub(crate) async fn gc(client: &MailClient, json: bool) -> Result<()> {
    let report = client.collect_garbage().await?;
    if json {
        json::print(&JsonGc::from(&report))?;
    } else {
        println!(
            "Removed {} unreferenced files ({}).",
            report.removed,
            display::size(report.bytes)
        );
    }
    Ok(())
}
