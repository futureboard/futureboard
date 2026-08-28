//! Scalar `f32` kernels.
//!
//! Correctness first: this is a plain, obvious, portable reference. It is the
//! implementation the Python↔Rust golden-vector test is run against, and it
//! stays the reference even after a vectorised backend exists.
//!
//! # The backend seam
//!
//! Every kernel below is a free function taking plain slices, so an AVX2 /
//! AVX-512 / NEON backend is a sibling module with the same signatures plus a
//! runtime feature check at load time — no change to [`crate::lstm`], which
//! only ever calls through this module. That work is deliberately deferred:
//! measuring a scalar baseline is what makes a later claim of speed-up mean
//! anything.
//!
//! No kernel here allocates, locks, or branches on data.

/// `1 / (1 + e^-x)`.
///
/// Written the branch-free way rather than as a lookup table: an LSTM gate is
/// evaluated 4·H times per sample, and a table's interpolation error would show
/// up directly in the golden-vector comparison against PyTorch.
///
/// This is also where the remaining Python↔Rust disagreement lives. PyTorch's
/// vectorised `sigmoid` is up to 2 ULP from a correctly-rounded result, two
/// libm implementations differ from each other by up to 4 ULP, and there are
/// 4·H of these per sample. On a low-gain model that stays at half an ULP of
/// the output; on a model whose weights have to produce +20 dB of gain it
/// compounds to ~13 ULP over a few thousand recurrent steps. Nothing in the
/// arithmetic around it closes that gap — it was measured, not assumed.
#[inline(always)]
pub fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

#[inline(always)]
pub fn tanh(x: f32) -> f32 {
    x.tanh()
}

/// `out[m] += sum_n w[m * n_cols + n] * x[n]` for a row-major `[n_rows, n_cols]`.
///
/// This is the shape PyTorch stores `weight_ih` / `weight_hh` in, so no
/// transpose happens anywhere between the exporter and here.
///
/// Accumulation is plain sequential `f32`. Widening the accumulator to `f64`
/// was measured against the golden vectors and moved the parity error by less
/// than 1 %: the residual disagreement with PyTorch is not in the sums, it is
/// in `exp` and `tanh` (see the note on [`sigmoid`]).
#[inline]
pub fn matvec_accum(out: &mut [f32], w: &[f32], x: &[f32]) {
    let n_cols = x.len();
    debug_assert_eq!(w.len(), out.len() * n_cols);
    for (row, acc) in out.iter_mut().enumerate() {
        let base = row * n_cols;
        let mut sum = 0.0f32;
        for n in 0..n_cols {
            sum += w[base + n] * x[n];
        }
        *acc += sum;
    }
}

#[inline]
pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len());
    let mut sum = 0.0f32;
    for i in 0..a.len() {
        sum += a[i] * b[i];
    }
    sum
}

/// Copy a bias vector into an accumulator.
#[inline]
pub fn load_bias(out: &mut [f32], bias: &[f32]) {
    debug_assert_eq!(out.len(), bias.len());
    out.copy_from_slice(bias);
}

/// One LSTM step, in PyTorch's gate order: input, forget, cell, output.
///
/// `gates` is `[4H]` and already contains `b + W_ih·x + W_hh·h`. `h` and `c`
/// are updated in place. This is the entirety of the recurrence:
///
/// ```text
/// i = σ(g_i)   f = σ(g_f)   g = tanh(g_g)   o = σ(g_o)
/// c' = f ⊙ c + i ⊙ g
/// h' = o ⊙ tanh(c')
/// ```
#[inline]
pub fn lstm_step(gates: &[f32], h: &mut [f32], c: &mut [f32]) {
    let hidden = h.len();
    debug_assert_eq!(gates.len(), 4 * hidden);
    debug_assert_eq!(c.len(), hidden);
    for k in 0..hidden {
        let i = sigmoid(gates[k]);
        let f = sigmoid(gates[hidden + k]);
        let g = tanh(gates[2 * hidden + k]);
        let o = sigmoid(gates[3 * hidden + k]);
        let c_new = f * c[k] + i * g;
        c[k] = c_new;
        h[k] = o * tanh(c_new);
    }
}

/// Copy one row out of a `[rows, dim]` embedding table.
///
/// Bounds are validated when the model is loaded, so an out-of-range index
/// here is a bug in this crate rather than in the file; it clamps instead of
/// panicking because the alternative in an audio callback is silence and a
/// crash report.
#[inline]
pub fn embedding_row<'a>(table: &'a [f32], dim: usize, rows: usize, index: usize) -> &'a [f32] {
    let row = index.min(rows.saturating_sub(1));
    let start = row * dim;
    &table[start..start + dim]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sigmoid_and_tanh_are_sane() {
        assert!((sigmoid(0.0) - 0.5).abs() < 1e-7);
        assert!(sigmoid(-40.0) >= 0.0 && sigmoid(40.0) <= 1.0);
        assert!((tanh(0.0)).abs() < 1e-9);
        assert!(sigmoid(f32::NEG_INFINITY).is_finite());
    }

    #[test]
    fn matvec_matches_a_hand_computed_case() {
        // [2, 3] row-major
        let w = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
        let x = [1.0, 0.5, -1.0];
        let mut out = [10.0, 20.0];
        matvec_accum(&mut out, &w, &x);
        assert_eq!(out, [10.0 + (1.0 + 1.0 - 3.0), 20.0 + (4.0 + 2.5 - 6.0)]);
    }

    #[test]
    fn lstm_step_with_zero_gates() {
        // gates all zero: i = f = o = 0.5, g = 0 -> c stays half, h = 0.5*tanh(c)
        let gates = [0.0f32; 8];
        let mut h = [0.0f32; 2];
        let mut c = [1.0f32; 2];
        lstm_step(&gates, &mut h, &mut c);
        assert!((c[0] - 0.5).abs() < 1e-6);
        assert!((h[0] - 0.5 * 0.5f32.tanh()).abs() < 1e-6);
    }

    #[test]
    fn embedding_row_selects_the_right_slice() {
        let table = [0.0, 1.0, 2.0, 3.0, 4.0, 5.0]; // [3, 2]
        assert_eq!(embedding_row(&table, 2, 3, 1), &[2.0, 3.0]);
        // out of range clamps to the last row rather than panicking
        assert_eq!(embedding_row(&table, 2, 3, 9), &[4.0, 5.0]);
    }
}
