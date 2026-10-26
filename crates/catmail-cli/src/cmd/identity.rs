//! `catmail identity`: other addresses an account sends as, each through an SMTP server
//! of its own, and `--from`, which picks one for a message.

use anyhow::{Context, Result};
use catmail_core::{Account, AccountId, IdentityId, MailClient, NewIdentity, SecretString};
use clap::{Args, Subcommand, ValueEnum};

use crate::cmd::account::{Login, SmtpServer, read_password};
use crate::cmd::one_account;
use crate::json::{self, JsonIdentity};
use crate::output::print_identities;

#[derive(Subcommand)]
pub(crate) enum IdentityCommand {
    /// Add an address to send as through another SMTP server (a sending service, say),
    /// once the server accepts the login
    Add(AddIdentity),
    /// List an account's identities
    List {
        /// Account id or email address [default: the only account]
        #[arg(short, long)]
        account: Option<String>,
    },
    /// Make an identity the default: new messages are from it
    Default {
        /// Identity id, as shown by `catmail identity list`
        id: i64,
    },
    /// Remove an identity other than the default; its drafts are sent as the default
    Rm {
        /// Identity id, as shown by `catmail identity list`
        id: i64,
    },
    /// Say whether an identity's SMTP server saves what it sends to Sent itself (Gmail's
    /// does), so that no second copy is saved
    SavesSent {
        /// Identity id, as shown by `catmail identity list`
        id: i64,
        /// `auto` guesses: yes for Gmail's own SMTP server with the account's login
        saves: SavesSent,
    },
}

#[derive(Clone, Copy, ValueEnum)]
pub(crate) enum SavesSent {
    Yes,
    No,
    Auto,
}

impl SavesSent {
    fn setting(self) -> Option<bool> {
        match self {
            Self::Yes => Some(true),
            Self::No => Some(false),
            Self::Auto => None,
        }
    }
}

#[derive(Args)]
pub(crate) struct AddIdentity {
    /// Account id or email address [default: the only account]
    #[arg(short, long)]
    account: Option<String>,
    /// The address messages are from
    #[arg(long)]
    email: String,
    /// Display name used in the From header
    #[arg(long)]
    name: Option<String>,
    /// Login name for the SMTP server [default: the email address]
    #[arg(long)]
    username: Option<String>,

    #[command(flatten)]
    smtp: SmtpServer,
    #[command(flatten)]
    login: Login,
}

pub(crate) async fn identity(client: &MailClient, cmd: IdentityCommand, json: bool) -> Result<()> {
    match cmd {
        IdentityCommand::Add(args) => {
            let account = one_account(client, args.account.as_deref()).await?;
            let password = read_password(&args.email, args.login.password_stdin)?;
            add(client, &account, args, password, json).await?;
        }
        IdentityCommand::List { account } => {
            let account = one_account(client, account.as_deref()).await?;
            let identities = client.list_identities(account.id).await?;
            if json {
                let list: Vec<_> = identities
                    .iter()
                    .map(|i| JsonIdentity::new(i, &account))
                    .collect();
                json::print_keyed("identities", &list)?;
            } else {
                print_identities(&account, &identities);
            }
        }
        IdentityCommand::Default { id } => {
            client.set_default_identity(IdentityId(id)).await?;
            if !json {
                println!("Identity {id} is the default now.");
            }
        }
        IdentityCommand::Rm { id } => {
            client.remove_identity(IdentityId(id)).await?;
            if !json {
                println!("Removed identity {id}.");
            }
        }
        IdentityCommand::SavesSent { id, saves } => {
            client
                .set_identity_saves_sent(IdentityId(id), saves.setting())
                .await?;
            if !json {
                println!("Updated identity {id}.");
            }
        }
    }
    Ok(())
}

/// Logs in first, unless told not to, as `catmail account add` does.
async fn add(
    client: &MailClient,
    account: &Account,
    args: AddIdentity,
    password: SecretString,
    json: bool,
) -> Result<()> {
    let new = NewIdentity {
        username: args.username.unwrap_or_else(|| args.email.clone()),
        email: args.email,
        display_name: args.name,
        smtp: args.smtp.config(),
    };
    let added = client
        .add_identity(account.id, new, password, args.login.verify())
        .await;
    let identity = args.login.added("identity", added)?;
    if json {
        json::print_keyed("identity", &JsonIdentity::new(&identity, account))?;
    } else {
        println!(
            "Added identity {} ({}) to {}",
            identity.id, identity.email, account.email
        );
    }
    Ok(())
}

/// The identity of the account `--from` names, by id or address (for an address two of
/// them send as, see [`MailClient::identity_by_address`]).
pub(crate) async fn resolve_from(
    client: &MailClient,
    account: AccountId,
    from: &str,
) -> Result<IdentityId> {
    let from = from.trim();
    let found = match from.parse::<i64>() {
        Ok(id) => client
            .list_identities(account)
            .await?
            .into_iter()
            .find(|i| i.id.0 == id)
            .map(|i| i.id),
        Err(_) => client.identity_by_address(account, from).await?,
    };
    found.with_context(|| {
        format!("account {account} has no identity `{from}`; see `catmail identity list`")
    })
}

#[cfg(test)]
mod tests {
    use catmail_core::Security;
    use catmail_testkit::{FakeImap, FakeSmtp, ServerState, SmtpState};

    use super::*;
    use crate::test_support::synced_with_smtp;

    fn args(port: u16, no_verify: bool) -> AddIdentity {
        AddIdentity {
            account: None,
            email: "news@example.org".into(),
            name: None,
            username: Some("resend".into()),
            smtp: SmtpServer {
                smtp_host: "127.0.0.1".into(),
                smtp_port: Some(port),
                smtp_security: Security::Plain,
            },
            login: Login {
                password_stdin: true,
                no_verify,
            },
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn wrong_password_adds_nothing_and_from_finds_what_was_added() {
        let fake = FakeImap::start(ServerState::new("alice", "pw")).await;
        let smtp = FakeSmtp::start(SmtpState::new("resend", "key")).await;
        let dir = tempfile::tempdir().unwrap();
        let (client, account) = synced_with_smtp(&fake, smtp.port(), dir.path()).await;
        let account = client.get_account(account).await.unwrap().unwrap();

        let err = add(
            &client,
            &account,
            args(smtp.port(), false),
            "typo".into(),
            false,
        )
        .await
        .unwrap_err();
        assert!(format!("{err:#}").contains("--no-verify"), "{err:#}");
        let only_own = client.list_identities(account.id).await.unwrap();
        assert_eq!(only_own.len(), 1, "only the one made with the account");

        add(
            &client,
            &account,
            args(smtp.port(), false),
            "key".into(),
            false,
        )
        .await
        .unwrap();
        let identities = client.list_identities(account.id).await.unwrap();
        let (own, identity) = (&identities[0], &identities[1]);
        for from in ["News@example.org", &identity.id.to_string()] {
            assert_eq!(
                resolve_from(&client, account.id, from).await.unwrap(),
                identity.id
            );
        }
        assert_eq!(
            resolve_from(&client, account.id, "ALICE@example.com")
                .await
                .unwrap(),
            own.id
        );
        assert!(
            resolve_from(&client, account.id, "other@example.org")
                .await
                .is_err()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn saves_sent_is_set_and_put_back_to_a_guess() {
        let fake = FakeImap::start(ServerState::new("alice", "pw")).await;
        let smtp = FakeSmtp::start(SmtpState::new("alice", "pw")).await;
        let dir = tempfile::tempdir().unwrap();
        let (client, account) = synced_with_smtp(&fake, smtp.port(), dir.path()).await;
        let own = client.list_identities(account).await.unwrap()[0].id;
        for (saves, expected) in [
            (SavesSent::Yes, Some(true)),
            (SavesSent::No, Some(false)),
            (SavesSent::Auto, None),
        ] {
            let cmd = IdentityCommand::SavesSent { id: own.0, saves };
            identity(&client, cmd, true).await.unwrap();
            let identities = client.list_identities(account).await.unwrap();
            assert_eq!(identities[0].saves_sent, expected);
        }
    }
}
