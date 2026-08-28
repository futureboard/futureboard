//! The JSON header, as explicit Rust types.
//!
//! Only the fields the runtime actually uses are typed; the rest of the header
//! is kept as raw JSON so that a file written by a newer trainer still loads.
//! Anything that changes how audio is computed — architecture, hyper-
//! parameters, conditioning, normalisation — is typed and validated, because
//! guessing there means producing wrong audio silently.

use serde::Deserialize;

use crate::error::{FbmxError, Result};

/// Model families this runtime can *describe*. Only some of them can be
/// *executed*; see [`crate::container::FbmxModel::instantiate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelType {
    Lstm,
    Gru,
    Tcn,
    Other(String),
}

impl ModelType {
    pub fn as_str(&self) -> &str {
        match self {
            ModelType::Lstm => "lstm",
            ModelType::Gru => "gru",
            ModelType::Tcn => "tcn",
            ModelType::Other(s) => s,
        }
    }
}

impl From<&str> for ModelType {
    fn from(s: &str) -> Self {
        match s {
            "lstm" => ModelType::Lstm,
            "gru" => ModelType::Gru,
            "tcn" => ModelType::Tcn,
            other => ModelType::Other(other.to_string()),
        }
    }
}

/// How the data this model was fitted to came into existence.
///
/// Carried through from the dataset, and never inferred. A model distilled
/// from a circuit simulation and one fitted to a hardware capture are not
/// interchangeable claims, and a host that displays "modelled from hardware"
/// needs this to be true.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceType {
    Synthetic,
    CircuitModel,
    HardwareCapture,
    Hybrid,
    /// A value written by a newer trainer. Not treated as any of the above.
    Other(String),
}

impl SourceType {
    pub fn as_str(&self) -> &str {
        match self {
            SourceType::Synthetic => "synthetic",
            SourceType::CircuitModel => "circuit_model",
            SourceType::HardwareCapture => "hardware_capture",
            SourceType::Hybrid => "hybrid",
            SourceType::Other(s) => s,
        }
    }

    /// True when the model's behaviour originates in measured hardware.
    pub fn is_hardware_derived(&self) -> bool {
        matches!(self, SourceType::HardwareCapture | SourceType::Hybrid)
    }
}

impl From<&str> for SourceType {
    fn from(s: &str) -> Self {
        match s {
            "synthetic" => SourceType::Synthetic,
            "circuit_model" => SourceType::CircuitModel,
            "hardware_capture" => SourceType::HardwareCapture,
            "hybrid" => SourceType::Hybrid,
            other => SourceType::Other(other.to_string()),
        }
    }
}

/// The structural facts about a model, independent of its weights.
#[derive(Debug, Clone)]
pub struct Architecture {
    pub name: String,
    pub hidden_size: Option<usize>,
    pub num_layers: Option<usize>,
    pub channels: u32,
    pub causal: bool,
    pub recurrent: bool,
    pub receptive_field: usize,
    pub parameter_count: u64,
}

/// Everything a host needs to know about a loaded model.
#[derive(Debug, Clone)]
pub struct FbmxModelInfo {
    pub format_version: u32,
    pub model_uuid: String,
    pub model_type: ModelType,
    pub architecture: Architecture,
    pub sample_rate: u32,
    pub conditioning: ConditioningSchema,
    pub source_type: SourceType,
    /// True only if a human asserted this model was measured against its
    /// reference. Written by the exporter; never inferred here.
    pub validated: bool,
    pub name: String,
    pub license: String,
}

// ---------------------------------------------------------------------------
// raw header
// ---------------------------------------------------------------------------
#[derive(Debug, Clone, Deserialize)]
pub struct FbmxHeader {
    pub format: String,
    pub format_version: u32,
    pub model_uuid: String,
    #[serde(default)]
    pub created_utc: String,
    #[serde(default)]
    pub producer: serde_json::Value,
    pub model: ModelBlock,
    #[serde(default)]
    pub input_spec: serde_json::Value,
    #[serde(default)]
    pub state_spec: serde_json::Value,
    #[serde(default)]
    pub conditioning: ConditioningSchema,
    #[serde(default)]
    pub normalization: Normalization,
    pub tensors: Vec<TensorEntry>,
    #[serde(default)]
    pub metadata: Metadata,
    pub checksums: Checksums,
    #[serde(default)]
    pub extra: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ModelBlock {
    #[serde(rename = "type")]
    pub model_type: String,
    #[serde(default)]
    pub architecture: String,
    pub sample_rate: u32,
    #[serde(default = "one")]
    pub channels: u32,
    #[serde(default)]
    pub causal: bool,
    #[serde(default)]
    pub recurrent: bool,
    #[serde(default = "one_usize")]
    pub receptive_field: usize,
    #[serde(default)]
    pub parameter_count: u64,
    #[serde(default)]
    pub hidden_size: Option<usize>,
    /// Architecture-specific; deserialised by whichever runtime claims it.
    #[serde(default)]
    pub hparams: serde_json::Value,
}

fn one() -> u32 {
    1
}
fn one_usize() -> usize {
    1
}

#[derive(Debug, Clone, Deserialize)]
pub struct TensorEntry {
    pub name: String,
    pub dtype: String,
    pub shape: Vec<usize>,
    pub offset: usize,
    pub nbytes: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Checksums {
    #[serde(default)]
    pub algorithm: String,
    #[serde(default)]
    pub data_sha256: String,
    #[serde(default)]
    pub data_nbytes: usize,
}

/// Fixed affine scaling around the network:
/// `y = output_gain * f(input_gain * x + input_offset) + output_offset`.
#[derive(Debug, Clone, Deserialize)]
pub struct Normalization {
    #[serde(default = "none_scheme")]
    pub scheme: String,
    #[serde(default = "unit")]
    pub input_gain: f32,
    #[serde(default)]
    pub input_offset: f32,
    #[serde(default = "unit")]
    pub output_gain: f32,
    #[serde(default)]
    pub output_offset: f32,
}

fn none_scheme() -> String {
    "none".to_string()
}
fn unit() -> f32 {
    1.0
}

impl Default for Normalization {
    fn default() -> Self {
        Self {
            scheme: none_scheme(),
            input_gain: 1.0,
            input_offset: 0.0,
            output_gain: 1.0,
            output_offset: 0.0,
        }
    }
}

impl Normalization {
    /// True when the transform is the identity and can be skipped entirely.
    pub fn is_identity(&self) -> bool {
        self.input_gain == 1.0
            && self.input_offset == 0.0
            && self.output_gain == 1.0
            && self.output_offset == 0.0
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Metadata {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub license: String,
    #[serde(default)]
    pub license_url: String,
    #[serde(default)]
    pub attribution: String,
    #[serde(default = "synthetic_source")]
    pub model_source_type: String,
    #[serde(default)]
    pub dataset: serde_json::Value,
    #[serde(default)]
    pub training: serde_json::Value,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub validated: bool,
}

fn synthetic_source() -> String {
    "synthetic".to_string()
}

// ---------------------------------------------------------------------------
// conditioning
// ---------------------------------------------------------------------------
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct ConditioningSchema {
    #[serde(default)]
    pub continuous: Vec<ContinuousParam>,
    #[serde(default)]
    pub categorical: Vec<CategoricalParam>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ContinuousParam {
    pub name: String,
    #[serde(default)]
    pub minimum: f32,
    #[serde(default = "unit")]
    pub maximum: f32,
    #[serde(default)]
    pub default: f32,
    #[serde(default)]
    pub unit: String,
    #[serde(default)]
    pub description: String,
}

impl ContinuousParam {
    /// Map a real value onto the `[-1, 1]` the network was trained on.
    /// Out-of-range values clamp; this must match `fbmx.conditioning` exactly.
    pub fn normalize(&self, value: f32) -> f32 {
        let span = self.maximum - self.minimum;
        if span <= 0.0 {
            return 0.0;
        }
        let clamped = value.clamp(self.minimum, self.maximum);
        2.0 * (clamped - self.minimum) / span - 1.0
    }

    pub fn denormalize(&self, value: f32) -> f32 {
        let span = self.maximum - self.minimum;
        self.minimum + (value.clamp(-1.0, 1.0) + 1.0) * 0.5 * span
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct CategoricalParam {
    pub name: String,
    pub categories: Vec<String>,
    #[serde(default)]
    pub default: String,
    #[serde(default = "four")]
    pub embedding_dim: usize,
    #[serde(default)]
    pub description: String,
}

fn four() -> usize {
    4
}

impl CategoricalParam {
    pub fn index_of(&self, value: &str) -> Result<usize> {
        self.categories
            .iter()
            .position(|c| c == value)
            .ok_or_else(|| FbmxError::UnknownCategory {
                parameter: self.name.clone(),
                value: value.to_string(),
            })
    }

    pub fn default_index(&self) -> usize {
        self.categories
            .iter()
            .position(|c| *c == self.default)
            .unwrap_or(0)
    }
}

impl ConditioningSchema {
    /// Width of the vector the network sees: continuous values followed by one
    /// embedding row per categorical parameter, in declaration order.
    pub fn cond_dim(&self) -> usize {
        self.continuous.len()
            + self
                .categorical
                .iter()
                .map(|c| c.embedding_dim)
                .sum::<usize>()
    }

    pub fn is_empty(&self) -> bool {
        self.continuous.is_empty() && self.categorical.is_empty()
    }

    pub fn continuous_index(&self, name: &str) -> Option<usize> {
        self.continuous.iter().position(|p| p.name == name)
    }

    pub fn categorical_index(&self, name: &str) -> Option<usize> {
        self.categorical.iter().position(|p| p.name == name)
    }

    pub fn names(&self) -> Vec<&str> {
        self.continuous
            .iter()
            .map(|p| p.name.as_str())
            .chain(self.categorical.iter().map(|p| p.name.as_str()))
            .collect()
    }
}

// ---------------------------------------------------------------------------
// LSTM hyper-parameters
// ---------------------------------------------------------------------------
/// The `hparams` block of an `lstm` (or `gru`) model.
#[derive(Debug, Clone, Deserialize)]
pub struct RnnHparams {
    #[serde(default = "thirty_two")]
    pub hidden_size: usize,
    #[serde(default = "one_usize")]
    pub num_layers: usize,
    #[serde(default = "forty_eight_k")]
    pub sample_rate: u32,
    #[serde(default = "concat")]
    pub conditioning: String,
    #[serde(default)]
    pub cond_proj_dim: Option<usize>,
    #[serde(default)]
    pub head_hidden: usize,
    #[serde(default = "yes")]
    pub residual: bool,
    #[serde(default)]
    pub dropout: f64,
}

fn thirty_two() -> usize {
    32
}
fn forty_eight_k() -> u32 {
    48_000
}
fn concat() -> String {
    "concat".to_string()
}
fn yes() -> bool {
    true
}

impl RnnHparams {
    /// Reject anything this runtime does not implement *exactly*.
    ///
    /// The alternative — running a close-enough approximation of an unsupported
    /// option — produces a model that sounds subtly wrong with no error
    /// anywhere. V0 implements one layer, concatenated conditioning and a
    /// single linear head; everything else is a named refusal.
    pub fn check_supported(&self) -> Result<()> {
        if self.num_layers != 1 {
            return Err(FbmxError::UnsupportedArchitecture(format!(
                "num_layers = {} (this runtime implements 1)",
                self.num_layers
            )));
        }
        match self.conditioning.as_str() {
            "concat" | "none" => {}
            other => {
                return Err(FbmxError::UnsupportedArchitecture(format!(
                    "conditioning = {other:?} (this runtime implements \"concat\" and \"none\"; \
                     FiLM-conditioned models are a training-side experiment)"
                )));
            }
        }
        if self.cond_proj_dim.is_some() {
            return Err(FbmxError::UnsupportedArchitecture(
                "cond_proj_dim is set (this runtime feeds raw normalised parameters \
                 and embeddings to the recurrence)"
                    .to_string(),
            ));
        }
        if self.head_hidden != 0 {
            return Err(FbmxError::UnsupportedArchitecture(format!(
                "head_hidden = {} (this runtime implements a single linear output layer)",
                self.head_hidden
            )));
        }
        Ok(())
    }
}
