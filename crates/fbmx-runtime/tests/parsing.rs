//! Container parsing: what must load, and what must be refused.
//!
//! A model loader is an attack surface — a `.fbmx` can arrive from a forum
//! post. Every case here is a file that is *almost* right.

mod support;

use fbmx_runtime::{FbmxError, FbmxModel, ModelType, SourceType};
use support::*;

#[test]
fn loads_the_exported_smoke_model() {
    let model = FbmxModel::load(golden_model("smoke_lstm32")).expect("smoke model must load");
    let info = model.info();
    assert_eq!(info.format_version, 1);
    assert_eq!(info.model_type, ModelType::Lstm);
    assert_eq!(info.sample_rate, 48_000);
    assert_eq!(info.architecture.channels, 1);
    assert!(info.architecture.causal);
    assert!(info.architecture.recurrent);
    assert_eq!(info.architecture.receptive_field, 1);
    assert_eq!(info.architecture.hidden_size, Some(32));
    assert_eq!(info.source_type, SourceType::Synthetic);
    assert!(!info.validated, "a smoke model must never claim validation");
    assert!(!info.model_uuid.is_empty());
}

#[test]
fn conditioning_schema_survives_the_crossing() {
    let model = FbmxModel::load(golden_model("smoke_lstm32")).unwrap();
    let schema = &model.info().conditioning;
    assert_eq!(schema.names(), vec!["drive", "mix", "mode"]);
    assert_eq!(schema.continuous.len(), 2);
    assert_eq!(schema.categorical.len(), 1);
    let mode = &schema.categorical[0];
    assert_eq!(mode.categories, vec!["soft", "hard"]);
    assert_eq!(mode.embedding_dim, 4);
    // two dials + one 4-wide embedding
    assert_eq!(schema.cond_dim(), 6);
}

#[test]
fn continuous_normalisation_matches_python() {
    let model = FbmxModel::load(golden_model("smoke_lstm32")).unwrap();
    let drive = &model.info().conditioning.continuous[0];
    assert_eq!(drive.normalize(0.0), -1.0);
    assert_eq!(drive.normalize(1.0), 1.0);
    assert_eq!(drive.normalize(0.5), 0.0);
    assert_eq!(drive.normalize(99.0), 1.0, "must clamp, not extrapolate");
}

#[test]
fn parameter_count_in_the_header_matches_the_weights_loaded() {
    // True for a model with no auxiliary heads: everything the header counts
    // is something the runtime executes. A model with an auxiliary gain head
    // declares more than the runtime loads, by design.
    let model = FbmxModel::load(golden_model("smoke_lstm32")).unwrap();
    let engine = model.instantiate().unwrap();
    assert_eq!(
        engine.parameter_count() as u64,
        model.info().architecture.parameter_count
    );
}

// ---------------------------------------------------------------------------
// rejection
// ---------------------------------------------------------------------------
#[test]
fn rejects_a_short_file() {
    assert!(matches!(
        FbmxModel::from_bytes(b"FBMX"),
        Err(FbmxError::TooShort { .. })
    ));
}

#[test]
fn rejects_bad_magic() {
    let mut raw = tiny_lstm();
    raw[0] = b'X';
    assert!(matches!(
        FbmxModel::from_bytes(&raw),
        Err(FbmxError::BadMagic(_))
    ));
}

#[test]
fn rejects_an_unsupported_format_version() {
    let data = f32_blob(&[0.0]);
    let header = minimal_header(&data, "");
    let raw = build_fbmx(&header, &data, 2);
    assert!(matches!(
        FbmxModel::from_bytes(&raw),
        Err(FbmxError::UnsupportedFormatVersion { found: 2, .. })
    ));
}

#[test]
fn rejects_a_flipped_bit_in_the_weights() {
    let mut raw = tiny_lstm();
    let n = raw.len();
    raw[n - 40] ^= 0xff;
    match FbmxModel::from_bytes(&raw) {
        Err(FbmxError::ChecksumMismatch { what, .. }) => assert_eq!(what, "file"),
        other => panic!("expected a checksum rejection, got {other:?}"),
    }
}

#[test]
fn rejects_a_truncated_file() {
    let raw = tiny_lstm();
    let cut = &raw[..raw.len() - 8];
    assert!(FbmxModel::from_bytes(cut).is_err());
}

#[test]
fn rejects_a_wrong_data_checksum_even_when_the_file_hash_agrees() {
    // The trailer is recomputed over the tampered body, so only the header's
    // own tensor-data hash catches this one. Both checks have to exist.
    let data = f32_blob(&[1.0, 2.0]);
    let header = minimal_header(&data, "")
        .replace(&fbmx_runtime::sha256::hex_digest(&data), &"0".repeat(64));
    let raw = build_fbmx(&header, &data, 1);
    match FbmxModel::from_bytes(&raw) {
        Err(FbmxError::ChecksumMismatch { what, .. }) => assert_eq!(what, "tensor data"),
        other => panic!("expected a data checksum rejection, got {other:?}"),
    }
}

#[test]
fn rejects_a_header_longer_than_the_file() {
    let mut raw = tiny_lstm();
    raw[8..16].copy_from_slice(&(u64::MAX / 2).to_le_bytes());
    // The trailer no longer matches either, but the length must be rejected
    // before anything is sliced with it.
    assert!(matches!(
        FbmxModel::from_bytes(&raw),
        Err(FbmxError::HeaderTooLarge { .. }) | Err(FbmxError::HeaderOutOfBounds { .. })
    ));
}

#[test]
fn rejects_a_tensor_that_runs_past_the_data_region() {
    let data = f32_blob(&[1.0, 2.0, 3.0, 4.0]);
    let tensors =
        r#"{"name":"rnn.weight_ih_l0","dtype":"f32","shape":[4,1],"offset":8,"nbytes":16}"#;
    let raw = build_fbmx(&minimal_header(&data, tensors), &data, 1);
    match FbmxModel::from_bytes(&raw) {
        Err(FbmxError::TensorOutOfBounds { name, .. }) => assert_eq!(name, "rnn.weight_ih_l0"),
        other => panic!("expected a bounds rejection, got {other:?}"),
    }
}

#[test]
fn rejects_a_shape_that_disagrees_with_nbytes() {
    let data = f32_blob(&[1.0, 2.0, 3.0, 4.0]);
    let tensors = r#"{"name":"w","dtype":"f32","shape":[2,2],"offset":0,"nbytes":8}"#;
    let raw = build_fbmx(&minimal_header(&data, tensors), &data, 1);
    assert!(matches!(
        FbmxModel::from_bytes(&raw),
        Err(FbmxError::TensorSizeMismatch { .. })
    ));
}

#[test]
fn rejects_an_unsupported_dtype() {
    let data = f32_blob(&[1.0, 2.0]);
    let tensors = r#"{"name":"w","dtype":"f16","shape":[4],"offset":0,"nbytes":8}"#;
    let raw = build_fbmx(&minimal_header(&data, tensors), &data, 1);
    assert!(matches!(
        FbmxModel::from_bytes(&raw),
        Err(FbmxError::UnsupportedDtype { .. })
    ));
}

#[test]
fn rejects_non_finite_weights() {
    let data = f32_blob(&[f32::NAN, 0.0, 0.0, 0.0]);
    let tensors = r#"{"name":"w","dtype":"f32","shape":[4],"offset":0,"nbytes":16}"#;
    let raw = build_fbmx(&minimal_header(&data, tensors), &data, 1);
    assert!(matches!(
        FbmxModel::from_bytes(&raw),
        Err(FbmxError::NonFiniteWeights(_))
    ));
}

#[test]
fn rejects_malformed_json() {
    let data = f32_blob(&[0.0]);
    let raw = build_fbmx("{not json", &data, 1);
    assert!(matches!(
        FbmxModel::from_bytes(&raw),
        Err(FbmxError::Json(_))
    ));
}

#[test]
fn reports_a_missing_tensor_by_name() {
    let data = f32_blob(&[1.0, 2.0, 3.0, 4.0]);
    let tensors =
        r#"{"name":"rnn.weight_ih_l0","dtype":"f32","shape":[4,1],"offset":0,"nbytes":16}"#;
    let raw = build_fbmx(&minimal_header(&data, tensors), &data, 1);
    let model = FbmxModel::from_bytes(&raw).expect("the container itself is valid");
    match model.instantiate() {
        Err(FbmxError::MissingTensor(name)) => assert_eq!(name, "rnn.weight_hh_l0"),
        other => panic!("expected a missing-tensor error, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// architecture gating
// ---------------------------------------------------------------------------
#[test]
fn rejects_architectures_it_does_not_implement() {
    let raw = tiny_lstm();
    let text = String::from_utf8_lossy(&raw[16..1200]).to_string();
    assert!(text.contains("\"type\":\"lstm\""));

    for (patch, needle) in [(r#""type":"tcn""#, "tcn"), (r#""type":"gru""#, "gru")] {
        let data = f32_blob(&[0.0; 16]);
        let header = minimal_header(&data, "").replace(r#""type":"lstm""#, patch);
        let raw = build_fbmx(&header, &data, 1);
        let model = FbmxModel::from_bytes(&raw).unwrap();
        match model.instantiate() {
            Err(FbmxError::UnsupportedModelType(t)) => assert_eq!(t, needle),
            other => panic!("expected {needle} to be refused, got {other:?}"),
        }
    }
}

#[test]
fn rejects_hyperparameters_it_cannot_execute_exactly() {
    // Every one of these is an option the training side supports and this
    // runtime does not. Approximating any of them would produce a model that
    // sounds subtly wrong with no error anywhere, so each must be a refusal.
    let cases = [
        (
            r#"{"hidden_size":1,"num_layers":2,"conditioning":"none","head_hidden":0,"residual":true}"#,
            "two layers",
        ),
        (
            r#"{"hidden_size":1,"num_layers":1,"conditioning":"film","head_hidden":0,"residual":true}"#,
            "FiLM conditioning",
        ),
        (
            r#"{"hidden_size":1,"num_layers":1,"conditioning":"both","head_hidden":0,"residual":true}"#,
            "concat+FiLM",
        ),
        (
            r#"{"hidden_size":1,"num_layers":1,"conditioning":"concat","cond_proj_dim":16,"head_hidden":0,"residual":true}"#,
            "projected conditioning",
        ),
        (
            r#"{"hidden_size":1,"num_layers":1,"conditioning":"none","head_hidden":8,"residual":true}"#,
            "MLP output head",
        ),
    ];
    for (hparams, what) in cases {
        let data = f32_blob(&[0.0; 16]);
        let header = minimal_header_with(&data, "", hparams);
        let raw = build_fbmx(&header, &data, 1);
        let model = FbmxModel::from_bytes(&raw).unwrap();
        assert!(
            matches!(
                model.instantiate(),
                Err(FbmxError::UnsupportedArchitecture(_))
            ),
            "{what} must be refused, not approximated"
        );
    }
}

#[test]
fn rejects_a_model_that_does_not_declare_itself_causal() {
    let data = f32_blob(&[0.0; 16]);
    let header = minimal_header(&data, "").replace(r#""causal":true"#, r#""causal":false"#);
    let raw = build_fbmx(&header, &data, 1);
    let model = FbmxModel::from_bytes(&raw).unwrap();
    assert!(matches!(model.instantiate(), Err(FbmxError::NotCausal)));
}
