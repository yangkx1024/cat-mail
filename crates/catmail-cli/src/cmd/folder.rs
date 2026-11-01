//! `catmail folder`: create, rename and delete an account's folders (on Gmail, its
//! labels). Unlike changes to messages, these are made on the server at once, and fail
//! without a connection.

use anyhow::Result;
use catmail_core::{Folder, MailClient};
use clap::Subcommand;

use crate::cmd::{find_folder, one_account};
use crate::json::{self, JsonFolder};
use crate::output::printable;

#[derive(Subcommand)]
pub(crate) enum FolderCommand {
    /// Create a folder (on Gmail, a label)
    Add {
        /// Its name: one level, e.g. `2026`
        name: String,
        /// The folder to create it in [default: at the top]
        #[arg(long)]
        parent: Option<String>,
        /// Account id or email address [default: the only account]
        #[arg(short, long)]
        account: Option<String>,
    },
    /// Rename a folder, staying where it is; the folders in it go along
    Rename {
        /// The folder, by name (e.g. `Archive/2026`)
        folder: String,
        /// Its new name: one level
        name: String,
        /// Account id or email address [default: the only account]
        #[arg(short, long)]
        account: Option<String>,
    },
    /// Delete a folder, the folders in it, and their messages, on the server too (on
    /// Gmail, take a label off its messages); this can't be undone
    Rm {
        /// The folder, by name
        folder: String,
        /// Account id or email address [default: the only account]
        #[arg(short, long)]
        account: Option<String>,
    },
}

pub(crate) async fn folder(client: &MailClient, cmd: FolderCommand, json: bool) -> Result<()> {
    match cmd {
        FolderCommand::Add {
            name,
            parent,
            account,
        } => {
            let account = one_account(client, account.as_deref()).await?;
            let parent = match parent {
                Some(name) => Some(find_folder(client, account.id, &name).await?.id),
                None => None,
            };
            let folder = client.create_folder(account.id, parent, &name).await?;
            let line = format!("Created {}", printable(&folder.name));
            print(&folder, &line, json)
        }
        FolderCommand::Rename {
            folder,
            name,
            account,
        } => {
            let account = one_account(client, account.as_deref()).await?;
            let old = find_folder(client, account.id, &folder).await?;
            let folder = client.rename_folder(old.id, &name).await?;
            let line = format!(
                "Renamed {} to {}",
                printable(&old.name),
                printable(&folder.name)
            );
            print(&folder, &line, json)
        }
        FolderCommand::Rm { folder, account } => {
            let account = one_account(client, account.as_deref()).await?;
            // As it was: it is gone afterwards.
            let folder = find_folder(client, account.id, &folder).await?;
            client.delete_folder(folder.id).await?;
            let line = format!("Deleted {}", printable(&folder.name));
            print(&folder, &line, json)
        }
    }
}

/// The folder for scripts, `line` for people.
fn print(folder: &Folder, line: &str, json: bool) -> Result<()> {
    if json {
        json::print_keyed("folder", &JsonFolder::from(folder))
    } else {
        println!("{line}");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use catmail_testkit::{FakeImap, ServerState};

    use super::*;
    use crate::test_support::synced;

    #[tokio::test(flavor = "multi_thread")]
    async fn folders_are_found_by_name() {
        let fake = FakeImap::start(ServerState::new("alice", "pw")).await;
        let dir = tempfile::tempdir().unwrap();
        let (client, account) = synced(&fake, dir.path()).await;
        let run = async |cmd| folder(&client, cmd, true).await;
        run(FolderCommand::Add {
            name: "Clients".into(),
            parent: None,
            account: None,
        })
        .await
        .unwrap();
        run(FolderCommand::Add {
            name: "Acme".into(),
            parent: Some("Clients".into()),
            account: None,
        })
        .await
        .unwrap();
        run(FolderCommand::Rename {
            folder: "Clients/Acme".into(),
            name: "Initech".into(),
            account: None,
        })
        .await
        .unwrap();
        assert_eq!(
            fake.state().mailbox_names(),
            ["INBOX", "Clients", "Clients/Initech"]
        );

        run(FolderCommand::Rm {
            folder: "Clients".into(),
            account: None,
        })
        .await
        .unwrap();
        assert_eq!(fake.state().mailbox_names(), ["INBOX"]);
        let err = run(FolderCommand::Rm {
            folder: "Clients".into(),
            account: None,
        })
        .await
        .unwrap_err();
        assert!(err.to_string().contains("no folder `Clients`"), "{err}");
        assert_eq!(client.list_folders(account).await.unwrap().len(), 1);
    }
}
