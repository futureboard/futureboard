//! How the jam client proves who it is.
//!
//! A Futureboard account is the only identity a jam recognises, and this crate
//! never learns a password, never runs a sign-in flow and never persists a
//! credential. It asks the host application — Studio, which already holds a
//! signed-in account session — for a bearer token at the moment it needs one.
//!
//! The token is fetched per use rather than cached here so that signing out in
//! Studio takes effect on the next jam call instead of on the next restart.

use std::sync::Arc;

use crate::error::{JamError, Result};

/// Supplies the Futureboard account token for jam calls.
///
/// Blocking on purpose: every caller in this crate runs on the jam worker
/// thread or on a caller thread that is already doing network I/O, so an async
/// signature would buy nothing and would force a runtime on Studio.
pub trait JamCredentialProvider: Send + Sync {
    /// A bearer token for the signed-in account, or an error explaining why
    /// there is none.
    fn access_token(&self) -> Result<String>;

    /// The account id, when the host knows it. Only used to label diagnostics
    /// before `auth.ready` arrives; the server's own view of the account is
    /// always authoritative.
    fn account_hint(&self) -> Option<String> {
        None
    }
}

/// Shared handle to a provider.
pub type SharedCredentials = Arc<dyn JamCredentialProvider>;

/// A provider that always reports "signed out".
///
/// It is what a build with no account layer gets, and it makes the failure a
/// clear message at the first call rather than an unauthenticated socket that
/// opens and closes.
pub struct NoCredentials;

impl JamCredentialProvider for NoCredentials {
    fn access_token(&self) -> Result<String> {
        Err(JamError::Auth(
            "no Futureboard account is signed in".to_string(),
        ))
    }
}

/// A provider backed by a closure, for hosts that already have a session store.
pub struct CredentialFn<F>(pub F)
where
    F: Fn() -> Result<String> + Send + Sync;

impl<F> JamCredentialProvider for CredentialFn<F>
where
    F: Fn() -> Result<String> + Send + Sync,
{
    fn access_token(&self) -> Result<String> {
        (self.0)()
    }
}

/// A fixed token. Development only — it is what lets a local `jamd` running
/// `JAM_AUTH_MODE=dev` be reached without a real account service.
pub struct StaticToken(String);

impl StaticToken {
    pub fn new(token: impl Into<String>) -> Self {
        Self(token.into())
    }
}

impl JamCredentialProvider for StaticToken {
    fn access_token(&self) -> Result<String> {
        if self.0.trim().is_empty() {
            return Err(JamError::Auth(
                "the configured jam token is empty".to_string(),
            ));
        }
        Ok(self.0.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_empty_provider_reports_a_sign_in_problem_not_a_network_one() {
        let error = NoCredentials.access_token().expect_err("signed out");
        assert!(matches!(error, JamError::Auth(_)));
        assert!(!error.recoverable());
    }

    #[test]
    fn a_closure_provider_is_consulted_on_every_call() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        let provider = CredentialFn(move || {
            counter.fetch_add(1, Ordering::Relaxed);
            Ok("token".to_string())
        });
        let _ = provider.access_token();
        let _ = provider.access_token();
        assert_eq!(calls.load(Ordering::Relaxed), 2);
    }
}
