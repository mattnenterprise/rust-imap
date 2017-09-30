#![crate_name = "imap"]
#![crate_type = "lib"]

//! imap is a IMAP client for Rust.

extern crate bufstream;
extern crate native_tls;
extern crate regex;

pub mod authenticator;
pub mod client;
pub mod error;
pub mod mailbox;

mod parse;

#[cfg(test)]
mod mock_stream;
