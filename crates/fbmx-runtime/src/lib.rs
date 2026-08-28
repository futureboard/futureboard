//! `fbmx-runtime` — load and execute `.fbmx` neural audio models in pure Rust.
//!
//! No Python, no PyTorch, no ONNX Runtime, no TensorFlow, no GPU, and — after
//! [`FbmxModel::load`] has returned — no allocation, no file access, no locks,
//! no logging and no JSON. The realtime half of this crate is a few hundred
//! lines of `f32` arithmetic over buffers that were sized while the model was
//! being parsed.
//!
//! ```no_run
//! use fbmx_runtime::{AudioModel, FbmxModel};
//!
//! // Load time: allocates, verifies checksums, sizes every buffer.
//! let model = FbmxModel::load("models/fa76-revd.fbmx")?;
//! let mut engine = model.instantiate()?;
//! engine.set_parameter("Input", 7.0)?;
//! engine.set_category("Ratio", "All Buttons")?;
//!
//! // Realtime: allocation-free, state carried across every call.
//! let mut buffer = [0.0f32; 128];
//! engine.process_block_in_place(&mut buffer);
//! # Ok::<(), fbmx_runtime::FbmxError>(())
//! ```
//!
//! # What is implemented
//!
//! Exactly what the FBMX LSTM-32 V0 baseline needs, and nothing else:
//! dense layers, embedding lookup, the LSTM recurrence, `tanh`/`sigmoid`, a
//! residual output and conditioning normalisation. This is deliberately not a
//! general graph executor — an architecture the training side has not committed
//! to is rejected with a named error rather than half-executed.
//!
//! # Trust model
//!
//! A `.fbmx` file may come from anywhere, so the loader treats it as untrusted
//! input: the crate is `#![forbid(unsafe_code)]`, every offset is bounds
//! checked before use, header and tensor-region sizes are capped, and both
//! SHA-256 checksums must verify before a single weight is read.

#![forbid(unsafe_code)]

pub mod container;
pub mod error;
pub mod header;
pub mod lstm;
pub mod ops;
pub mod params;
pub mod sha256;

pub use container::{FbmxModel, Tensor};
pub use error::{FbmxError, Result};
pub use header::{
    Architecture, CategoricalParam, Checksums, ConditioningSchema, ContinuousParam, FbmxHeader,
    FbmxModelInfo, Metadata, ModelType, Normalization, SourceType, TensorEntry,
};
pub use lstm::{LstmRuntime, LstmState};
pub use params::ParameterSet;

/// The container format version this build understands.
pub const SUPPORTED_FORMAT_VERSION: u32 = 1;

/// File magic: the first four bytes of every `.fbmx`.
pub const MAGIC: [u8; 4] = *b"FBMX";

/// Anything that can be driven sample-by-sample or block-by-block in an audio
/// callback.
///
/// Implementors must guarantee that processing `n` samples as one call and as
/// any sequence of smaller calls produce the same output — i.e. all temporal
/// state is carried explicitly and none of it resets at a block boundary.
pub trait AudioModel {
    /// Return to the "silence forever before now" state.
    fn reset(&mut self);

    /// Process one sample.
    fn process_sample(&mut self, x: f32) -> f32;

    /// Process a block. `input` and `output` must be the same length.
    fn process_block(&mut self, input: &[f32], output: &mut [f32]);

    /// Process a block in place.
    fn process_block_in_place(&mut self, buffer: &mut [f32]) {
        for i in 0..buffer.len() {
            buffer[i] = self.process_sample(buffer[i]);
        }
    }

    /// Rate the model was trained at. Running it at another rate changes its
    /// time constants; the caller decides whether that is acceptable.
    fn sample_rate(&self) -> u32;

    /// Additional latency introduced by the model. Always 0 here: every
    /// architecture this runtime accepts is causal with no lookahead.
    fn latency_samples(&self) -> usize {
        0
    }
}
