//! Byte-level `.fbmx` parsing.
//!
//! ```text
//! offset  size  contents
//! 0       4     magic b"FBMX"
//! 4       4     u32 LE format version
//! 8       8     u64 LE header length
//! 16      H     UTF-8 JSON header
//! ...     pad   zero padding to a 16-byte boundary
//! D       ...   tensor data, little-endian f32, C-contiguous, in header order
//! EOF-32  32    SHA-256 of every preceding byte
//! ```
//!
//! Order of operations matters: nothing is interpreted before it has been
//! bounds-checked, and no weight is read before both checksums verify. The
//! file is assumed hostile until then.

use std::path::Path;

use crate::error::{FbmxError, Result};
use crate::header::{
    Architecture, FbmxHeader, FbmxModelInfo, ModelType, RnnHparams, SourceType, TensorEntry,
};
use crate::lstm::LstmRuntime;
use crate::sha256;
use crate::{MAGIC, SUPPORTED_FORMAT_VERSION};

/// Largest header this runtime will buffer. A `.fbmx` header is a few
/// kilobytes; the cap exists so a corrupt length field cannot ask for a
/// gigabyte.
pub const MAX_HEADER_BYTES: u64 = 16 * 1024 * 1024;

/// Largest tensor region. V0 models are tens of kilobytes; this is a sanity
/// bound, not a target.
pub const MAX_TENSOR_BYTES: usize = 512 * 1024 * 1024;

const TRAILER_BYTES: usize = 32;
const PREFIX_BYTES: usize = 16;
const ALIGN: usize = 16;

/// One tensor, decoded to `f32` and owned.
#[derive(Debug, Clone)]
pub struct Tensor {
    pub name: String,
    pub shape: Vec<usize>,
    pub data: Vec<f32>,
}

impl Tensor {
    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Check the shape and return the data, or say precisely what was wrong.
    pub fn expect_shape(&self, want: &[usize]) -> Result<&[f32]> {
        if self.shape != want {
            return Err(FbmxError::TensorShape {
                name: self.name.clone(),
                want: want.to_vec(),
                got: self.shape.clone(),
            });
        }
        Ok(&self.data)
    }
}

/// A parsed, verified `.fbmx` file.
///
/// Holding one costs the weights plus the header; it is not itself a running
/// model. Call [`FbmxModel::instantiate`] to get something with state.
#[derive(Debug, Clone)]
pub struct FbmxModel {
    header: FbmxHeader,
    info: FbmxModelInfo,
    tensors: Vec<Tensor>,
}

impl FbmxModel {
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self> {
        let bytes = std::fs::read(path)?;
        Self::from_bytes(&bytes)
    }

    pub fn from_bytes(raw: &[u8]) -> Result<Self> {
        let minimum = PREFIX_BYTES + TRAILER_BYTES;
        if raw.len() < minimum {
            return Err(FbmxError::TooShort {
                len: raw.len(),
                need: minimum,
            });
        }

        // -- framing --------------------------------------------------------
        let magic: [u8; 4] = [raw[0], raw[1], raw[2], raw[3]];
        if magic != MAGIC {
            return Err(FbmxError::BadMagic(magic));
        }
        let format_version = u32::from_le_bytes([raw[4], raw[5], raw[6], raw[7]]);
        if format_version != SUPPORTED_FORMAT_VERSION {
            return Err(FbmxError::UnsupportedFormatVersion {
                found: format_version,
                supported: SUPPORTED_FORMAT_VERSION,
            });
        }
        let header_len = u64::from_le_bytes([
            raw[8], raw[9], raw[10], raw[11], raw[12], raw[13], raw[14], raw[15],
        ]);
        if header_len > MAX_HEADER_BYTES {
            return Err(FbmxError::HeaderTooLarge {
                header_len,
                cap: MAX_HEADER_BYTES,
            });
        }
        let body_end = raw.len() - TRAILER_BYTES;
        let header_end =
            PREFIX_BYTES
                .checked_add(header_len as usize)
                .ok_or(FbmxError::HeaderOutOfBounds {
                    header_len,
                    file_len: raw.len(),
                })?;
        if header_end > body_end {
            return Err(FbmxError::HeaderOutOfBounds {
                header_len,
                file_len: raw.len(),
            });
        }

        // -- integrity, before anything is interpreted -----------------------
        let computed_file = sha256::digest(&raw[..body_end]);
        if computed_file != raw[body_end..] {
            return Err(FbmxError::ChecksumMismatch {
                what: "file",
                expected: sha256::hex(&raw[body_end..]),
                actual: sha256::hex(&computed_file),
            });
        }

        let header: FbmxHeader = serde_json::from_slice(&raw[PREFIX_BYTES..header_end])?;

        let data_offset = header_end + ((ALIGN - header_end % ALIGN) % ALIGN);
        if data_offset > body_end {
            return Err(FbmxError::HeaderOutOfBounds {
                header_len,
                file_len: raw.len(),
            });
        }
        let data = &raw[data_offset..body_end];
        if data.len() > MAX_TENSOR_BYTES {
            return Err(FbmxError::TensorRegionTooLarge {
                bytes: data.len(),
                cap: MAX_TENSOR_BYTES,
            });
        }
        if header.checksums.data_sha256.is_empty() {
            return Err(FbmxError::MissingChecksum);
        }
        let computed_data = sha256::hex_digest(data);
        if computed_data != header.checksums.data_sha256 {
            return Err(FbmxError::ChecksumMismatch {
                what: "tensor data",
                expected: header.checksums.data_sha256.clone(),
                actual: computed_data,
            });
        }

        // -- tensors ---------------------------------------------------------
        let tensors = header
            .tensors
            .iter()
            .map(|entry| decode_tensor(entry, data))
            .collect::<Result<Vec<_>>>()?;

        let info = build_info(&header);
        Ok(Self {
            header,
            info,
            tensors,
        })
    }

    pub fn info(&self) -> &FbmxModelInfo {
        &self.info
    }

    pub fn header(&self) -> &FbmxHeader {
        &self.header
    }

    pub fn tensors(&self) -> &[Tensor] {
        &self.tensors
    }

    pub fn tensor(&self, name: &str) -> Result<&Tensor> {
        self.tensors
            .iter()
            .find(|t| t.name == name)
            .ok_or_else(|| FbmxError::MissingTensor(name.to_string()))
    }

    /// Total weight bytes held.
    pub fn weight_bytes(&self) -> usize {
        self.tensors.iter().map(|t| t.data.len() * 4).sum()
    }

    /// Build a running engine. Allocates; call it before the audio thread
    /// starts, never from inside a callback.
    pub fn instantiate(&self) -> Result<LstmRuntime> {
        if !self.header.model.causal {
            return Err(FbmxError::NotCausal);
        }
        match self.info.model_type {
            ModelType::Lstm => LstmRuntime::build(self),
            ref other => Err(FbmxError::UnsupportedModelType(other.as_str().to_string())),
        }
    }

    /// The typed hyper-parameters of a recurrent model.
    pub(crate) fn rnn_hparams(&self) -> Result<RnnHparams> {
        let hp: RnnHparams = serde_json::from_value(self.header.model.hparams.clone())?;
        hp.check_supported()?;
        Ok(hp)
    }
}

fn decode_tensor(entry: &TensorEntry, data: &[u8]) -> Result<Tensor> {
    // f32 only. f16/i8 storage is a format-version-2 conversation, and reading
    // one as f32 would produce noise rather than an error.
    if entry.dtype != "f32" {
        return Err(FbmxError::UnsupportedDtype {
            name: entry.name.clone(),
            dtype: entry.dtype.clone(),
        });
    }
    let elements: usize = entry.shape.iter().copied().try_fold(1usize, |acc, d| {
        acc.checked_mul(d).ok_or(FbmxError::TensorSizeMismatch {
            name: entry.name.clone(),
            declared: entry.nbytes,
            from_shape: usize::MAX,
        })
    })?;
    let from_shape = elements
        .checked_mul(4)
        .ok_or(FbmxError::TensorSizeMismatch {
            name: entry.name.clone(),
            declared: entry.nbytes,
            from_shape: usize::MAX,
        })?;
    if from_shape != entry.nbytes {
        return Err(FbmxError::TensorSizeMismatch {
            name: entry.name.clone(),
            declared: entry.nbytes,
            from_shape,
        });
    }
    let end = entry
        .offset
        .checked_add(entry.nbytes)
        .ok_or(FbmxError::TensorOutOfBounds {
            name: entry.name.clone(),
            offset: entry.offset,
            nbytes: entry.nbytes,
            data_len: data.len(),
        })?;
    if end > data.len() {
        return Err(FbmxError::TensorOutOfBounds {
            name: entry.name.clone(),
            offset: entry.offset,
            nbytes: entry.nbytes,
            data_len: data.len(),
        });
    }

    // from_le_bytes rather than a cast: the data region is 16-byte aligned in
    // the file but a &[u8] from anywhere is not required to be, and this crate
    // has no unsafe.
    let mut values = Vec::with_capacity(elements);
    for chunk in data[entry.offset..end].chunks_exact(4) {
        values.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
    if values.iter().any(|v| !v.is_finite()) {
        return Err(FbmxError::NonFiniteWeights(entry.name.clone()));
    }

    Ok(Tensor {
        name: entry.name.clone(),
        shape: entry.shape.clone(),
        data: values,
    })
}

fn build_info(header: &FbmxHeader) -> FbmxModelInfo {
    let m = &header.model;
    FbmxModelInfo {
        format_version: header.format_version,
        model_uuid: header.model_uuid.clone(),
        model_type: ModelType::from(m.model_type.as_str()),
        architecture: Architecture {
            name: if m.architecture.is_empty() {
                m.model_type.clone()
            } else {
                m.architecture.clone()
            },
            hidden_size: m.hidden_size,
            num_layers: m
                .hparams
                .get("num_layers")
                .and_then(|v| v.as_u64())
                .map(|v| v as usize),
            channels: m.channels,
            causal: m.causal,
            recurrent: m.recurrent,
            receptive_field: m.receptive_field,
            parameter_count: m.parameter_count,
        },
        sample_rate: m.sample_rate,
        conditioning: header.conditioning.clone(),
        source_type: SourceType::from(header.metadata.model_source_type.as_str()),
        validated: header.metadata.validated,
        name: header.metadata.name.clone(),
        license: header.metadata.license.clone(),
    }
}
