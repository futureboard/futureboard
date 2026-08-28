//! Errors.
//!
//! Every rejection path names what was wrong and what was expected. A model
//! loader that fails with "invalid file" is useless in the field, and one that
//! silently accepts a truncated tensor is worse than useless.

use std::fmt;

pub type Result<T> = std::result::Result<T, FbmxError>;

#[derive(Debug)]
pub enum FbmxError {
    Io(std::io::Error),
    Json(serde_json::Error),

    // -- container framing --------------------------------------------------
    /// Smaller than magic + version + header length + trailer.
    TooShort {
        len: usize,
        need: usize,
    },
    BadMagic([u8; 4]),
    UnsupportedFormatVersion {
        found: u32,
        supported: u32,
    },
    /// Header length is larger than the cap, i.e. the file is claiming a header
    /// we refuse to buffer.
    HeaderTooLarge {
        header_len: u64,
        cap: u64,
    },
    /// Header length runs past the end of the file.
    HeaderOutOfBounds {
        header_len: u64,
        file_len: usize,
    },
    TensorRegionTooLarge {
        bytes: usize,
        cap: usize,
    },

    // -- integrity ----------------------------------------------------------
    ChecksumMismatch {
        what: &'static str,
        expected: String,
        actual: String,
    },
    MissingChecksum,

    // -- tensor table -------------------------------------------------------
    UnsupportedDtype {
        name: String,
        dtype: String,
    },
    TensorOutOfBounds {
        name: String,
        offset: usize,
        nbytes: usize,
        data_len: usize,
    },
    /// `nbytes` disagrees with `shape` × element size.
    TensorSizeMismatch {
        name: String,
        declared: usize,
        from_shape: usize,
    },
    MissingTensor(String),
    TensorShape {
        name: String,
        want: Vec<usize>,
        got: Vec<usize>,
    },
    NonFiniteWeights(String),

    // -- architecture -------------------------------------------------------
    UnsupportedModelType(String),
    UnsupportedArchitecture(String),
    NotCausal,

    // -- parameters ---------------------------------------------------------
    UnknownParameter(String),
    UnknownCategory {
        parameter: String,
        value: String,
    },
}

impl fmt::Display for FbmxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FbmxError::Io(e) => write!(f, "i/o error: {e}"),
            FbmxError::Json(e) => write!(f, "malformed .fbmx header: {e}"),
            FbmxError::TooShort { len, need } => {
                write!(
                    f,
                    "file is {len} bytes, too short to be a .fbmx (need >= {need})"
                )
            }
            FbmxError::BadMagic(m) => write!(
                f,
                "bad magic {m:?}, expected {:?} — this is not a .fbmx file",
                crate::MAGIC
            ),
            FbmxError::UnsupportedFormatVersion { found, supported } => write!(
                f,
                "format version {found} is not supported by this build (supports {supported})"
            ),
            FbmxError::HeaderTooLarge { header_len, cap } => {
                write!(
                    f,
                    "header claims {header_len} bytes, above the {cap}-byte cap"
                )
            }
            FbmxError::HeaderOutOfBounds {
                header_len,
                file_len,
            } => write!(
                f,
                "header length {header_len} runs past the end of a {file_len}-byte file"
            ),
            FbmxError::TensorRegionTooLarge { bytes, cap } => {
                write!(
                    f,
                    "tensor region is {bytes} bytes, above the {cap}-byte cap"
                )
            }
            FbmxError::ChecksumMismatch {
                what,
                expected,
                actual,
            } => write!(
                f,
                "{what} checksum mismatch: expected {expected}, computed {actual} \
                 — the file is truncated or modified"
            ),
            FbmxError::MissingChecksum => {
                write!(
                    f,
                    "header carries no tensor-data checksum; refusing to load"
                )
            }
            FbmxError::UnsupportedDtype { name, dtype } => {
                write!(
                    f,
                    "tensor {name:?} has dtype {dtype:?}, which this runtime cannot read"
                )
            }
            FbmxError::TensorOutOfBounds {
                name,
                offset,
                nbytes,
                data_len,
            } => write!(
                f,
                "tensor {name:?} spans {offset}..{} of a {data_len}-byte data region",
                offset.saturating_add(*nbytes)
            ),
            FbmxError::TensorSizeMismatch {
                name,
                declared,
                from_shape,
            } => write!(
                f,
                "tensor {name:?} declares {declared} bytes but its shape implies {from_shape}"
            ),
            FbmxError::MissingTensor(name) => write!(f, "model is missing tensor {name:?}"),
            FbmxError::TensorShape { name, want, got } => {
                write!(f, "tensor {name:?} has shape {got:?}, expected {want:?}")
            }
            FbmxError::NonFiniteWeights(name) => {
                write!(f, "tensor {name:?} contains NaN or Inf")
            }
            FbmxError::UnsupportedModelType(t) => write!(
                f,
                "model type {t:?} is not implemented by this runtime (V0 implements: lstm)"
            ),
            FbmxError::UnsupportedArchitecture(why) => {
                write!(f, "unsupported architecture: {why}")
            }
            FbmxError::NotCausal => write!(
                f,
                "model header does not declare itself causal; it cannot run in an audio callback"
            ),
            FbmxError::UnknownParameter(p) => write!(f, "no parameter named {p:?}"),
            FbmxError::UnknownCategory { parameter, value } => {
                write!(f, "parameter {parameter:?} has no category {value:?}")
            }
        }
    }
}

impl std::error::Error for FbmxError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            FbmxError::Io(e) => Some(e),
            FbmxError::Json(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for FbmxError {
    fn from(e: std::io::Error) -> Self {
        FbmxError::Io(e)
    }
}

impl From<serde_json::Error> for FbmxError {
    fn from(e: serde_json::Error) -> Self {
        FbmxError::Json(e)
    }
}
