//! `catmail account`: adding, listing and removing accounts.

use std::io::{self, BufRead, IsTerminal};

use anyhow::{Context, Result, bail};
use catmail_core::{ExposeSecret, MailClient, NewAccount, SecretString, Security, ServerConfig};
use clap::{Args, Subcommand};

use crate::cmd::resolve_account;
use crate::json::{self, JsonAccount};
use crate::output::print_accounts;

#[derive(Subcommand)]
pub(crate) enum AccountCommand {
    /// Add an IMAP/SMTP account, once the IMAP server accepts the login
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

    #[arg(long)]
    smtp_host: String,
    /// [default: 465 for tls, 587 for starttls, 25 for plain]
    #[arg(long)]
    smtp_port: Option<u16>,
    /// tls | starttls | plain
    #[arg(long, default_value = "tls")]
    smtp_security: Security,

    /// Read the password from the first line of stdin instead of prompting
    #[arg(long)]
    password_stdin: bool,
    /// Add it without logging in first, e.g. while offline
    #[arg(long)]
    no_verify: bool,
}

pub(crate) async fn account(client: &MailClient, cmd: AccountCommand, json: bool) -> Result<()> {
    match cmd {
        AccountCommand::Add(args) => {
            let password = read_password(&args.email, args.password_stdin)?;
            add(client, args, password, json).await?;
        }
        AccountCommand::List => {
            let accounts = client.list_accounts().await?;
            if json {
                json::print_list::<JsonAccount, _>("accounts", &accounts)?;
            } else {
                print_accounts(&accounts);
            }
        }
        AccountCommand::Remove { account } => {
            let account = resolve_account(client, &account).await?;
            client.remove_account(account.id).await?;
            if json {
                json::print_keyed("account", &JsonAccount::from(&account))?;
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
        smtp: ServerConfig {
            host: args.smtp_host,
            port: args
                .smtp_port
                .unwrap_or_else(|| args.smtp_security.default_smtp_port()),
            security: args.smtp_security,
        },
    };
    if !args.no_verify {
        client
            .check_login(&new, &password)
            .await
            .context("couldn't log in, so the account wasn't added (--no-verify adds it anyway)")?;
    }
    let account = client.add_account(new, password).await?;
    if json {
        json::print_keyed("account", &JsonAccount::from(&account))?;
    } else {
        println!("Added account {} ({})", account.id, account.email);
    }
    Ok(())
}

fn read_password(email: &str, from_stdin: bool) -> Result<SecretString> {
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
    use catmail_testkit::{FakeImap, ServerState};

    use super::*;
    use crate::test_support::client;

    fn args(port: u16, no_verify: bool) -> AddAccount {
        AddAccount {
            email: "alice@example.com".into(),
            name: None,
            username: Some("alice".into()),
            imap_host: "127.0.0.1".into(),
            imap_port: Some(port),
            imap_security: Security::Plain,
            smtp_host: "127.0.0.1".into(),
            smtp_port: Some(port),
            smtp_security: Security::Plain,
            password_stdin: true,
            no_verify,
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn wrong_password_adds_nothing() {
        let fake = FakeImap::start(ServerState::new("alice", "pw")).await;
        let dir = tempfile::tempdir().unwrap();
        let (client, secrets) = client(dir.path());

        let err = add(&client, args(fake.port(), false), "typo".into(), false)
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("--no-verify"), "{err:#}");
        assert!(client.list_accounts().await.unwrap().is_empty());
        assert!(secrets.is_empty());

        add(&client, args(fake.port(), false), "pw".into(), false)
            .await
            .unwrap();
        assert_eq!(client.list_accounts().await.unwrap().len(), 1);

        // Without asking the server, as before.
        client
            .remove_account(client.list_accounts().await.unwrap()[0].id)
            .await
            .unwrap();
        drop(fake);
        add(&client, args(1, true), "typo".into(), false)
            .await
            .unwrap();
        assert_eq!(client.list_accounts().await.unwrap().len(), 1);
    }
}
