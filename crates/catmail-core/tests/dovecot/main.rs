//! Against the real servers `testenv/dovecot/run.sh` starts ([`testenv`]): Dovecot over
//! IMAP ([`imap`]), and its SMTP submission service relaying to a mailpit sink
//! ([`submission`]). Without them the tests skip.

mod imap;
mod submission;
mod testenv;
