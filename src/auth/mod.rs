//! Platform identity & credentials for the Actual AI account.
//!
//! This is a **separate** surface from the runner auth check in
//! [`crate::runner::auth`], which only inspects the underlying coding-agent
//! (e.g. `claude auth status`). Here we model the user's *Actual AI platform*
//! login: OAuth-issued tokens, the selected organization, and secure local
//! persistence.
//!
//! The browser OAuth + PKCE login flow and the `login` / `logout` / `whoami`
//! commands build on top of the credential store provided here.

pub mod loopback;
pub mod oauth;
pub mod pat;
pub mod pkce;
pub mod store;
pub mod token_store;

pub use store::StoredCredentials;
