//! `catmail account`: adding, listing and removing accounts.

use std::io::{self, BufRead, IsTerminal};

use anyhow::{Context, Result, bail};
use catmail_core::{
    Account, ExposeSecret, Identity, MailClient, NewAccount, SecretString, Security, ServerConfig,
};
use clap::{Args, Subcommand};

use crate::cmd::resolve_account;
use crate::json::{self, JsonAccount};
use crate::output::print_accounts;

#[derive(Subcommand)]
pub(crate) enum AccountCommand {
    /// Add an IMAP/SMTP account, once both servers accept the login
    Add(AddAccount),
    /// List accounts
    List,
    /// Remove an account and all local data for it
    Remove {
        /// Account id or email address
        account: String,
    },
}

#[derive(Args)]
pub(crate) struct AddAccount {
    #[arg(long)]
    email: String,
    /// Display name used in the From header
    #[arg(long)]
    name: Option<String>,
    /// Login name [default: the email address]
    #[arg(long)]
    username: Option<String>,

    #[arg(long)]
    imap_host: String,
    /// [default: 993 for tls, 143 otherwise]
    #[arg(long)]
    imap_port: Option<u16>,
    /// tls | starttls | plain
    #[arg(long, default_value = "tls")]
    imap_security: Security,

    #[command(flatten)]
    smtp: SmtpServer,
    #[command(flatten)]
    login: Login,
}

/// The SMTP server of `account add` and `identity add`.
#[derive(Args)]
pub(crate) struct SmtpServer {
    #[arg(long)]
    pub(crate) smtp_host: String,
    /// [default: 465 for tls, 587 for starttls, 25 for plain]
    #[arg(long)]
    pub(crate) smtp_port: Option<u16>,
    /// tls | starttls | plain
    #[arg(long, default_value = "tls")]
    pub(crate) smtp_security: Security,
}

impl SmtpServer {
    pub(crate) fn config(self) -> ServerConfig {
        ServerConfig {
            host: self.smtp_host,
            port: self
                .smtp_port
                .unwrap_or_else(|| self.smtp_security.default_smtp_port()),
            security: self.smtp_security,
        }
    }
}

/// How `account add` and `identity add` get the password, and whether they log in first.
#[derive(Args)]
pub(crate) struct Login {
    /// Read the password from the first line of stdin instead of prompting
    #[arg(long)]
    pub(crate) password_stdin: bool,
    /// Add it without logging in first, e.g. while offline
    #[arg(long)]
    pub(crate) no_verify: bool,
}

pub(crate) async fn account(client: &MailClient, cmd: AccountCommand, json: bool) -> Result<()> {
    match cmd {
        AccountCommand::Add(args) => {
            let password = read_password(&args.email, args.login.password_stdin)?;
            add(client, args, password, json).await?;
        }
        AccountCommand::List => {
            let mut accounts = Vec::new();
            for account in client.list_accounts().await? {
                accounts.push(with_identities(client, account).await?);
            }
            if json {
                let list: Vec<_> = accounts
                    .iter()
                    .map(|(a, identities)| JsonAccount::new(a, identities))
                    .collect();
                json::print_keyed("accounts", &list)?;
            } else {
                print_accounts(&accounts);
            }
        }
        AccountCommand::Remove { account } => {
            let account = resolve_account(client, &account).await?;
            // As it was: it is gone afterwards.
            let (account, identities) = with_identities(client, account).await?;
            client.remove_account(account.id).await?;
            if json {
                json::print_keyed("account", &JsonAccount::new(&account, &identities))?;
            } else {
                println!("Removed account {} ({})", account.id, account.email);
            }
        }
    }
    Ok(())
}

/// Logs in first, unless told not to, so that a mistyped password leaves nothing
/// behind: no account, no keyring entry.
async fn add(
    client: &MailClient,
    args: AddAccount,
    password: SecretString,
    json: bool,
) -> Result<()> {
    let new = NewAccount {
        username: args.username.unwrap_or_else(|| args.email.clone()),
        email: args.email,
        display_name: args.name,
        imap: ServerConfig {
            host: args.imap_host,
            port: args
                .imap_port
                .unwrap_or_else(|| args.imap_security.default_imap_port()),
            security: args.imap_security,
        },
        smtp: args.smtp.config(),
    };
    let account = if args.login.no_verify {
        client.add_account(new, password).await?
    } else {
        client
            .add_account_checked(new, password)
            .await
            .context("the account wasn't added (--no-verify adds it without logging in)")?
    };
    if json {
        let (account, identities) = with_identities(client, account).await?;
        json::print_keyed("account", &JsonAccount::new(&account, &identities))?;
    } else {
        println!("Added account {} ({})", account.id, account.email);
    }
    Ok(())
}

/// The account with the addresses it sends as.
async fn with_identities(
    client: &MailClient,
    account: Account,
) -> Result<(Account, Vec<Identity>)> {
    let identities = client.list_identities(account.id).await?;
    Ok((account, identities))
}

pub(crate) fn read_password(email: &str, from_stdin: bool) -> Result<SecretString> {
    let password = if from_stdin {
        let mut line = String::new();
        io::stdin().lock().read_line(&mut line)?;
        // Trimmed in place, so no copy is left behind unzeroed.
        let len = line.trim_end_matches(['\r', '\n']).len();
        line.truncate(len);
        SecretString::from(line)
    } else {
        if !io::stdin().is_terminal() {
            bail!("stdin is not a terminal; use --password-stdin to pipe the password");
        }
        rpassword::prompt_password(format!("Password for {email}: "))?.into()
    };
    if password.expose_secret().is_empty() {
        bail!("password must not be empty");
    }
    Ok(password)
}

#[cfg(test)]
mod tests {
    use catmail_testkit::{FakeImap, FakeSmtp, ServerState, SmtpState};

    use super::*;
    use crate::test_support::client;

    fn args(port: u16, smtp_port: u16, no_verify: bool) -> AddAccount {
        AddAccount {
            email: "alice@example.com".into(),
            name: None,
            username: Some("alice".into()),
            imap_host: "127.0.0.1".into(),
            imap_port: Some(port),
            imap_security: Security::Plain,
            smtp: SmtpServer {
                smtp_host: "127.0.0.1".into(),
                smtp_port: Some(smtp_port),
                smtp_security: Security::Plain,
            },
            login: Login {
                password_stdin: true,
                no_verify,
            },
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn wrong_password_adds_nothing() {
        let fake = FakeImap::start(ServerState::new("alice", "pw")).await;
        let smtp = FakeSmtp::start(SmtpState::new("alice", "pw")).await;
        let dir = tempfile::tempdir().unwrap();
        let (client, secrets) = client(dir.path());

        let err = add(
            &client,
            args(fake.port(), smtp.port(), false),
            "typo".into(),
            false,
        )
        .await
        .unwrap_err();
        assert!(format!("{err:#}").contains("--no-verify"), "{err:#}");
        // Nor does a server for sending that doesn't take it.
        smtp.state().faults.reject_login = true;
        let err = add(
            &client,
            args(fake.port(), smtp.port(), false),
            "pw".into(),
            false,
        )
        .await
        .unwrap_err();
        assert!(format!("{err:#}").contains("SMTP"), "{err:#}");
        assert!(client.list_accounts().await.unwrap().is_empty());
        assert!(secrets.is_empty());

        smtp.state().faults.reject_login = false;
        add(
            &client,
            args(fake.port(), smtp.port(), false),
            "pw".into(),
            false,
        )
        .await
        .unwrap();
        assert_eq!(client.list_accounts().await.unwrap().len(), 1);

        // Without asking the server, as before.
        client
            .remove_account(client.list_accounts().await.unwrap()[0].id)
            .await
            .unwrap();
        drop(fake);
        add(&client, args(1, 1, true), "typo".into(), false)
            .await
            .unwrap();
        assert_eq!(client.list_accounts().await.unwrap().len(), 1);
    }
}
