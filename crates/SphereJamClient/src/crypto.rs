//! The process-level rustls crypto provider.
//!
//! rustls only chooses a provider on its own when exactly one of its
//! `aws-lc-rs` and `ring` backends is compiled in. This workspace links both:
//! rustls' own default features bring in `aws-lc-rs`, while reqwest and
//! hyper-rustls ask for `ring`, and cargo unifies the two into one rustls
//! build. Every `ClientConfig::builder()` in the process then panics with
//! "Could not automatically determine the process-level CryptoProvider" —
//! including the one inside `tungstenite::connect`, which is why an unguarded
//! jam took the `jam-control` thread down with it and left the session
//! reporting "the jam worker has stopped".
//!
//! Selecting the provider by hand is the fix rustls documents, and it is a
//! process-wide setting rather than a jam one: whoever installs first wins, and
//! every later caller — here or anywhere else in Studio — uses that provider.
//! So this is deliberately tolerant of an already-installed provider instead of
//! insisting on its own.

use std::sync::Once;

static INSTALL: Once = Once::new();

/// Make sure this process has a rustls crypto provider before any TLS is
/// opened.
///
/// Idempotent and safe from any thread. Call it on the non-realtime path that
/// is about to build a TLS client — the jam threads do, at every point where a
/// handshake can start.
pub fn ensure_crypto_provider() {
    INSTALL.call_once(|| {
        // An `Err` here means another component installed a provider first.
        // That is a working process, not a failure: the goal is only that one
        // provider exists by the time rustls looks for it.
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Without an installed provider this is the call that panicked inside
    /// `tungstenite::connect`, so building one here is the regression this
    /// module exists to prevent.
    #[test]
    fn a_tls_client_config_can_be_built() {
        ensure_crypto_provider();
        assert!(rustls::crypto::CryptoProvider::get_default().is_some());

        let config = rustls::ClientConfig::builder()
            .with_root_certificates(rustls::RootCertStore::empty())
            .with_no_client_auth();
        assert!(!config.alpn_protocols.iter().any(|p| p.is_empty()));
    }

    #[test]
    fn installing_twice_is_not_an_error() {
        ensure_crypto_provider();
        ensure_crypto_provider();
        assert!(rustls::crypto::CryptoProvider::get_default().is_some());
    }
}
