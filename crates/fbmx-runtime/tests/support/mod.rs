//! Shared helpers: locate the golden fixtures, and build `.fbmx` files by hand
//! so the parser can be attacked with files no exporter would ever write.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

use fbmx_runtime::sha256;

/// `neural/tests/golden`, relative to this crate.
pub fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("neural")
        .join("tests")
        .join("golden")
}

pub fn golden_model(stem: &str) -> PathBuf {
    golden_dir().join(format!("{stem}.fbmx"))
}

pub fn golden_json(stem: &str) -> PathBuf {
    golden_dir().join(format!("{stem}.json"))
}

/// Assemble a well-framed `.fbmx` from a header and a tensor blob.
///
/// The header is taken verbatim, so a test can put anything in it — including
/// a wrong `data_sha256`, which is the point.
pub fn build_fbmx(header_json: &str, data: &[u8], format_version: u32) -> Vec<u8> {
    let header = header_json.as_bytes();
    let mut out = Vec::new();
    out.extend_from_slice(b"FBMX");
    out.extend_from_slice(&format_version.to_le_bytes());
    out.extend_from_slice(&(header.len() as u64).to_le_bytes());
    out.extend_from_slice(header);
    while out.len() % 16 != 0 {
        out.push(0);
    }
    out.extend_from_slice(data);
    let digest = sha256::digest(&out);
    out.extend_from_slice(&digest);
    out
}

/// A minimal but valid header for a 1-unit unconditioned LSTM, with the tensor
/// table and checksum filled in for `data`.
pub fn minimal_header(data: &[u8], tensors: &str) -> String {
    minimal_header_with(data, tensors, DEFAULT_HPARAMS)
}

/// The hparams block of a supported one-unit LSTM.
pub const DEFAULT_HPARAMS: &str = r#"{"hidden_size":1,"num_layers":1,"sample_rate":48000,
    "conditioning":"none","cond_proj_dim":null,"head_hidden":0,"residual":true,"dropout":0.0}"#;

/// Same, with the architecture hyper-parameters chosen by the caller, so a
/// test can ask for a configuration the runtime is expected to refuse.
pub fn minimal_header_with(data: &[u8], tensors: &str, hparams: &str) -> String {
    format!(
        r#"{{"format":"fbmx","format_version":1,"model_uuid":"test-uuid",
        "created_utc":"2026-01-01T00:00:00+00:00",
        "model":{{"type":"lstm","architecture":"lstm","sample_rate":48000,"channels":1,
                  "causal":true,"recurrent":true,"receptive_field":1,"parameter_count":0,
                  "hidden_size":1,
                  "hparams":{hparams}}},
        "input_spec":{{}},"state_spec":{{}},
        "conditioning":{{"continuous":[],"categorical":[]}},
        "normalization":{{"scheme":"none","input_gain":1.0,"input_offset":0.0,
                          "output_gain":1.0,"output_offset":0.0}},
        "tensors":[{tensors}],
        "metadata":{{"name":"test","license":"CC0-1.0","model_source_type":"synthetic",
                     "validated":false}},
        "checksums":{{"algorithm":"sha256","data_sha256":"{}","data_nbytes":{}}}}}"#,
        sha256::hex_digest(data),
        data.len()
    )
}

/// Little-endian f32 blob.
pub fn f32_blob(values: &[f32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

/// A complete, loadable one-unit LSTM: 4 gates × 1 input, 4 × 1 recurrent,
/// two bias vectors, a 1×1 head. Weights are arbitrary but finite.
pub fn tiny_lstm() -> Vec<u8> {
    let w_ih = [0.1f32, -0.2, 0.3, 0.4];
    let w_hh = [0.05f32, 0.06, -0.07, 0.08];
    let b_ih = [0.0f32, 0.1, 0.0, -0.1];
    let b_hh = [0.01f32, 0.0, 0.02, 0.0];
    let head_w = [0.5f32];
    let head_b = [0.01f32];

    let mut data = Vec::new();
    let mut entries = Vec::new();
    let mut push = |name: &str, shape: &str, values: &[f32], data: &mut Vec<u8>| {
        let bytes = f32_blob(values);
        entries.push(format!(
            r#"{{"name":"{name}","dtype":"f32","shape":{shape},"offset":{},"nbytes":{}}}"#,
            data.len(),
            bytes.len()
        ));
        data.extend_from_slice(&bytes);
    };
    push("rnn.weight_ih_l0", "[4,1]", &w_ih, &mut data);
    push("rnn.weight_hh_l0", "[4,1]", &w_hh, &mut data);
    push("rnn.bias_ih_l0", "[4]", &b_ih, &mut data);
    push("rnn.bias_hh_l0", "[4]", &b_hh, &mut data);
    push("head.weight", "[1,1]", &head_w, &mut data);
    push("head.bias", "[1]", &head_b, &mut data);

    let header = minimal_header(&data, &entries.join(","));
    build_fbmx(&header, &data, 1)
}
