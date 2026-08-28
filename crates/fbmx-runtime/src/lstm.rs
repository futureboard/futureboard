//! Stateful LSTM inference.
//!
//! One layer, `H` hidden units, conditioning concatenated to the per-sample
//! input, one linear output head, residual audio path:
//!
//! ```text
//! input[0]    = x                      (post input normalisation)
//! input[1..]  = [normalised dials ..., embedding rows ...]
//!
//! gates = b + W_ih · input + W_hh · h
//! i,f,g,o = σ,σ,tanh,σ of the four gate blocks
//! c = f ⊙ c + i ⊙ g
//! h = o ⊙ tanh(c)
//! y = head_b + head_w · h + x          (residual)
//! ```
//!
//! # Realtime contract
//!
//! Everything is sized in [`LstmRuntime::build`]. After that,
//! [`LstmRuntime::process_block`] performs no allocation, no I/O, no locking,
//! no logging and no JSON parsing — the only writes are into buffers this
//! struct already owns. `tests/no_alloc.rs` asserts that with a counting
//! allocator rather than trusting the claim.
//!
//! # State
//!
//! `h` and `c` are the entire memory of the model, and they are only ever
//! touched by [`LstmRuntime::process_sample`] and [`LstmRuntime::reset`].
//! Nothing resets at a block boundary, which is why processing 1024 samples in
//! one call and in 64 calls of 16 gives the same result.

use crate::AudioModel;
use crate::container::FbmxModel;
use crate::error::{FbmxError, Result};
use crate::header::{FbmxModelInfo, Normalization};
use crate::ops;
use crate::params::ParameterSet;

/// Hidden and cell state. Cloneable so a host can snapshot and restore it
/// (offline rendering, undo, A/B).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LstmState {
    pub h: Vec<f32>,
    pub c: Vec<f32>,
}

impl LstmState {
    pub fn zeros(hidden: usize) -> Self {
        Self {
            h: vec![0.0; hidden],
            c: vec![0.0; hidden],
        }
    }

    pub fn clear(&mut self) {
        self.h.iter_mut().for_each(|v| *v = 0.0);
        self.c.iter_mut().for_each(|v| *v = 0.0);
    }
}

#[derive(Debug, Clone)]
struct EmbeddingTable {
    rows: usize,
    dim: usize,
    data: Vec<f32>,
}

#[derive(Debug, Clone)]
pub struct LstmRuntime {
    // -- weights, immutable after build --------------------------------
    w_ih: Vec<f32>,   // [4H, input_size]
    w_hh: Vec<f32>,   // [4H, H]
    bias: Vec<f32>,   // b_ih + b_hh, [4H]
    head_w: Vec<f32>, // [H]
    head_b: f32,
    embeddings: Vec<EmbeddingTable>,

    // -- shape ---------------------------------------------------------
    hidden: usize,
    input_size: usize,
    cond_dim: usize,
    residual: bool,
    normalization: Normalization,
    info: FbmxModelInfo,

    // -- preallocated working set --------------------------------------
    state: LstmState,
    gates: Vec<f32>,
    input: Vec<f32>,
    params: ParameterSet,
    idle: Option<IdleTwin>,
}

/// A second copy of the model, fed nothing but silence.
///
/// A model distilled from a system that is silent at rest is not silent at
/// rest. Its recurrent state settles onto a fixed point determined by the
/// conditioning, the readout makes some number of that fixed point, and that
/// number comes out of the model forever after. Measured on the FA76 models it
/// runs to **-28 dBFS at INPUT 2** — four percent of full scale, against a
/// circuit that puts out exactly zero — and it moves with the controls, so it
/// is a family of constants rather than one.
///
/// It is not a training problem that went unnoticed for want of trying. A
/// silence-gated DC term in the loss moved it a few dB; extending the corpus
/// to cover the whole Input dial moved it a few more; neither came close,
/// because one linear readout cannot map a whole manifold of conditioning-
/// dependent fixed points to exactly zero and still model the audio.
///
/// So subtract it instead, and get it exactly rather than approximately: run
/// the same weights on a stream of zeros with the same conditioning, and take
/// its output away from the real one. At rest the two states are identical and
/// the result is exactly zero. There is no filter and no corner frequency, so
/// nothing in the frequency response moves — the twin sees only zeros, so its
/// output is a constant, and subtracting a constant is not an equaliser.
///
/// The twin costs a second model's arithmetic only while it is still moving.
/// Once its state stops changing — which is what "at rest" means — the cached
/// output stands until the conditioning changes and disturbs it again, so the
/// steady-state cost is one comparison per sample.
#[derive(Debug, Clone)]
struct IdleTwin {
    state: LstmState,
    /// The state before the current step, so "has it stopped moving?" is a
    /// question about the state rather than about its magnitude.
    previous_h: Vec<f32>,
    gates: Vec<f32>,
    input: Vec<f32>,
    output: f32,
    settled: bool,
}

impl IdleTwin {
    fn new(hidden: usize, input_size: usize) -> Self {
        Self {
            state: LstmState::zeros(hidden),
            previous_h: vec![0.0; hidden],
            gates: vec![0.0; 4 * hidden],
            input: vec![0.0; input_size],
            output: 0.0,
            settled: false,
        }
    }
}

impl LstmRuntime {
    /// Build from a parsed model. Allocates; not for the audio thread.
    pub(crate) fn build(model: &FbmxModel) -> Result<Self> {
        let hp = model.rnn_hparams()?;
        let hidden = hp.hidden_size;
        if hidden == 0 || hidden > 4096 {
            return Err(FbmxError::UnsupportedArchitecture(format!(
                "hidden_size = {hidden} is outside the supported range 1..=4096"
            )));
        }
        let info = model.info().clone();
        let schema = &info.conditioning;
        let conditioned = hp.conditioning == "concat";
        let cond_dim = if conditioned { schema.cond_dim() } else { 0 };
        let input_size = 1 + cond_dim;

        let w_ih = model
            .tensor("rnn.weight_ih_l0")?
            .expect_shape(&[4 * hidden, input_size])?
            .to_vec();
        let w_hh = model
            .tensor("rnn.weight_hh_l0")?
            .expect_shape(&[4 * hidden, hidden])?
            .to_vec();
        let b_ih = model
            .tensor("rnn.bias_ih_l0")?
            .expect_shape(&[4 * hidden])?
            .to_vec();
        let b_hh = model
            .tensor("rnn.bias_hh_l0")?
            .expect_shape(&[4 * hidden])?
            .to_vec();
        // PyTorch keeps the two bias vectors separate for historical CuDNN
        // reasons; their sum is all the arithmetic ever needs. Splitting them
        // to mirror PyTorch's own grouping was tried and changed the parity
        // error by less than 1 %, so the simpler form stays.
        let bias: Vec<f32> = b_ih.iter().zip(&b_hh).map(|(a, b)| a + b).collect();

        let head_w = model
            .tensor("head.weight")?
            .expect_shape(&[1, hidden])?
            .to_vec();
        let head_b = model.tensor("head.bias")?.expect_shape(&[1])?[0];

        // Embedding tables, in schema order. Extra tensors in the file —
        // auxiliary gain-reduction heads, for instance — are ignored: they are
        // training-time outputs and cost the audio path nothing.
        let mut embeddings = Vec::with_capacity(schema.categorical.len());
        if conditioned {
            for (i, param) in schema.categorical.iter().enumerate() {
                let name = format!("cond_encoder.embeddings.{i}.weight");
                let rows = param.categories.len();
                let dim = param.embedding_dim;
                let data = model.tensor(&name)?.expect_shape(&[rows, dim])?.to_vec();
                embeddings.push(EmbeddingTable { rows, dim, data });
            }
        }

        let params = ParameterSet::defaults(schema);
        let mut runtime = Self {
            w_ih,
            w_hh,
            bias,
            head_w,
            head_b,
            embeddings,
            hidden,
            input_size,
            cond_dim,
            residual: hp.residual,
            normalization: model.header().normalization.clone(),
            info,
            state: LstmState::zeros(hidden),
            gates: vec![0.0; 4 * hidden],
            input: vec![0.0; input_size],
            params,
            idle: None,
        };
        runtime.refresh_conditioning();
        Ok(runtime)
    }

    // -- description -----------------------------------------------------
    pub fn info(&self) -> &FbmxModelInfo {
        &self.info
    }

    pub fn hidden_size(&self) -> usize {
        self.hidden
    }

    pub fn input_size(&self) -> usize {
        self.input_size
    }

    pub fn conditioning_dim(&self) -> usize {
        self.cond_dim
    }

    /// Number of learned parameters this engine actually executes.
    ///
    /// Not necessarily the header's `parameter_count`: a model exported with
    /// auxiliary training heads declares those too, and the runtime does not
    /// load them. The two agree exactly when the model has no extra heads.
    pub fn parameter_count(&self) -> usize {
        self.w_ih.len()
            + self.w_hh.len()
            + self.bias.len() * 2
            + self.head_w.len()
            + 1
            + self.embeddings.iter().map(|e| e.data.len()).sum::<usize>()
    }

    // -- state -----------------------------------------------------------
    pub fn state(&self) -> &LstmState {
        &self.state
    }

    pub fn set_state(&mut self, state: LstmState) -> Result<()> {
        if state.h.len() != self.hidden || state.c.len() != self.hidden {
            return Err(FbmxError::UnsupportedArchitecture(format!(
                "state length {} does not match hidden size {}",
                state.h.len(),
                self.hidden
            )));
        }
        self.state = state;
        Ok(())
    }

    // -- parameters ------------------------------------------------------
    pub fn parameters(&self) -> &ParameterSet {
        &self.params
    }

    pub fn set_parameter(&mut self, name: &str, value: f32) -> Result<()> {
        self.params.set_by_name(name, value)
    }

    pub fn set_category(&mut self, name: &str, category: &str) -> Result<()> {
        self.params.set_category(name, category)
    }

    /// Index-based setters, for hosts that resolved names during preparation.
    pub fn set_parameter_at(&mut self, index: usize, value: f32) {
        self.params.set_continuous(index, value);
    }

    pub fn set_category_at(&mut self, index: usize, category: usize) {
        self.params.set_category_index(index, category);
    }

    /// Recompute the conditioning half of the input vector.
    ///
    /// Called automatically at the start of every block; exposed because a
    /// caller doing sample-at-a-time processing has to trigger it itself after
    /// changing a parameter.
    pub fn refresh_conditioning(&mut self) {
        self.params.take_dirty();
        if self.cond_dim == 0 {
            return;
        }
        let normalized = self.params.normalized();
        let mut at = 1;
        for value in normalized {
            self.input[at] = *value;
            at += 1;
        }
        for (table, &index) in self.embeddings.iter().zip(self.params.categories()) {
            let row = ops::embedding_row(&table.data, table.dim, table.rows, index);
            self.input[at..at + table.dim].copy_from_slice(row);
            at += table.dim;
        }
        debug_assert_eq!(at, self.input_size);

        // The twin runs the same conditioning against a silent input, and its
        // fixed point has just moved, so it has to run again to find it.
        if let Some(twin) = self.idle.as_mut() {
            twin.input.copy_from_slice(&self.input);
            twin.input[0] = self.normalization.input_offset;
            twin.settled = false;
        }
    }

    /// Subtract the model's own idle output, so silence in gives silence out.
    ///
    /// See [`IdleTwin`] for what this corrects and why it is not a filter.
    /// Off by default: it is a correction for a specific defect, it doubles the
    /// arithmetic while the controls are moving, and a model that does not have
    /// the defect should not pay for it. Allocates, so call it before the audio
    /// thread has the runtime.
    pub fn set_idle_compensation(&mut self, enabled: bool) {
        self.idle = if enabled {
            let mut twin = IdleTwin::new(self.hidden, self.input_size);
            twin.input.copy_from_slice(&self.input);
            twin.input[0] = self.normalization.input_offset;
            Some(twin)
        } else {
            None
        };
    }

    pub fn idle_compensation(&self) -> bool {
        self.idle.is_some()
    }

    /// What the model puts out with nothing going in, at the current settings.
    ///
    /// Zero when idle compensation is off — not because the model is silent,
    /// but because nothing is measuring it.
    pub fn idle_output(&self) -> f32 {
        self.idle.as_ref().map_or(0.0, |twin| twin.output)
    }

    /// Advance the twin one sample and return what to subtract.
    ///
    /// Skipped entirely once the twin's state stops moving, which is the
    /// steady state and therefore almost always.
    #[inline]
    fn idle_offset(&mut self) -> f32 {
        if self.idle.as_ref().is_none_or(|twin| twin.settled) {
            return self.idle.as_ref().map_or(0.0, |twin| twin.output);
        }
        // Taken out and put back so the twin can be written while the weights
        // are read; both live in `self` and the borrow checker cannot see that
        // they are different fields once one is behind an `Option`. Moving an
        // `IdleTwin` moves three vector headers and allocates nothing.
        let mut twin = self.idle.take().expect("checked just above");

        ops::load_bias(&mut twin.gates, &self.bias);
        ops::matvec_accum(&mut twin.gates, &self.w_ih, &twin.input);
        ops::matvec_accum(&mut twin.gates, &self.w_hh, &twin.state.h);
        twin.previous_h.copy_from_slice(&twin.state.h);
        ops::lstm_step(&twin.gates, &mut twin.state.h, &mut twin.state.c);

        let moved = twin
            .state
            .h
            .iter()
            .zip(&twin.previous_h)
            .fold(0.0f32, |m, (&now, &before)| m.max((now - before).abs()));

        let mut y = self.head_b + ops::dot(&self.head_w, &twin.state.h);
        if self.residual {
            y += twin.input[0];
        }
        y = self.normalization.output_gain * y + self.normalization.output_offset;
        let y = if y.is_finite() { y } else { 0.0 };

        // Settled when neither the state nor the output is still moving, by a
        // margin far below anything audible. The output test is the one that
        // matters — it is the quantity being subtracted — and the state test
        // stops a drift that has not yet reached the output being called done.
        const SETTLED: f32 = 1e-9;
        twin.settled = moved < SETTLED && (y - twin.output).abs() < SETTLED;
        twin.output = y;
        self.idle = Some(twin);
        y
    }

    #[inline]
    fn refresh_if_dirty(&mut self) {
        if self.params.take_dirty() {
            self.refresh_conditioning();
        }
    }
}

impl AudioModel for LstmRuntime {
    fn reset(&mut self) {
        self.state.clear();
        if let Some(twin) = self.idle.as_mut() {
            twin.state.clear();
            twin.output = 0.0;
            twin.settled = false;
        }
    }

    #[inline]
    fn process_sample(&mut self, x: f32) -> f32 {
        // A non-finite input would poison h and c permanently; substituting
        // silence for that one sample keeps the model recoverable.
        let x = if x.is_finite() { x } else { 0.0 };
        let x_in = self.normalization.input_gain * x + self.normalization.input_offset;
        self.input[0] = x_in;

        ops::load_bias(&mut self.gates, &self.bias);
        ops::matvec_accum(&mut self.gates, &self.w_ih, &self.input);
        ops::matvec_accum(&mut self.gates, &self.w_hh, &self.state.h);
        ops::lstm_step(&self.gates, &mut self.state.h, &mut self.state.c);

        let mut y = self.head_b + ops::dot(&self.head_w, &self.state.h);
        if self.residual {
            y += x_in;
        }
        y = self.normalization.output_gain * y + self.normalization.output_offset;
        y -= self.idle_offset();
        if y.is_finite() { y } else { 0.0 }
    }

    fn process_block(&mut self, input: &[f32], output: &mut [f32]) {
        self.refresh_if_dirty();
        let n = input.len().min(output.len());
        for i in 0..n {
            output[i] = self.process_sample(input[i]);
        }
    }

    fn process_block_in_place(&mut self, buffer: &mut [f32]) {
        self.refresh_if_dirty();
        for sample in buffer.iter_mut() {
            *sample = self.process_sample(*sample);
        }
    }

    fn sample_rate(&self) -> u32 {
        self.info.sample_rate
    }
}
