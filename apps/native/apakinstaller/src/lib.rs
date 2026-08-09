pub mod cli;
pub mod gui;
pub mod platform;

pub(crate) fn bundled_verifying_key() -> apak::Result<apak::ApakVerifyingKey> {
    let value = option_env!("APAK_VERIFYING_KEY").ok_or_else(|| {
        apak::ApakError::Signature(
            "this APAK build does not contain a package verification key".to_string(),
        )
    })?;
    apak::parse_verifying_key(value)
}
