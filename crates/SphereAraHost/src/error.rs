//! Error type shared by every ARA entry point, on every platform.

use std::fmt;

/// Why an ARA operation could not be completed.
///
/// Kept free of `ara2_bridge` types so the public surface is identical on
/// platforms that build the stub.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AraHostError {
    /// ARA is not available in this build (unsupported platform, or the plug-in
    /// exposes no ARA factory).
    Unsupported(String),
    /// The host asked for something the ARA model forbids.
    Invalid(String),
    /// The plug-in rejected an operation or returned an unusable result.
    Plugin(String),
    /// The host could not supply data the plug-in asked for.
    Host(String),
    /// The document session was quarantined and must be torn down.
    Poisoned,
}

impl AraHostError {
    /// Convenience constructor for the unsupported case.
    pub fn unsupported(what: impl Into<String>) -> Self {
        Self::Unsupported(what.into())
    }

    /// Convenience constructor for a host-side failure.
    pub fn host(what: impl Into<String>) -> Self {
        Self::Host(what.into())
    }

    /// Convenience constructor for an invalid-argument failure.
    pub fn invalid(what: impl Into<String>) -> Self {
        Self::Invalid(what.into())
    }

    /// Whether this error means "ARA simply is not there", as opposed to a real
    /// failure. Call sites use it to stay quiet instead of surfacing an error.
    pub fn is_unsupported(&self) -> bool {
        matches!(self, Self::Unsupported(_))
    }
}

impl fmt::Display for AraHostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported(what) => write!(f, "ARA unavailable: {what}"),
            Self::Invalid(what) => write!(f, "invalid ARA request: {what}"),
            Self::Plugin(what) => write!(f, "ARA plug-in error: {what}"),
            Self::Host(what) => write!(f, "ARA host error: {what}"),
            Self::Poisoned => write!(f, "ARA document session is poisoned"),
        }
    }
}

impl std::error::Error for AraHostError {}

/// Result alias used throughout the crate.
pub type AraResult<T> = Result<T, AraHostError>;
