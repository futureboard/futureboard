//! Runtime parameter state.
//!
//! Parameters arrive from a host in user units — Input 7.0, Attack 4.0, Ratio
//! "All Buttons" — and the network wants normalised values and embedding
//! indices. That conversion lives here, and it must match
//! `fbmx.conditioning` in the Python package exactly; a mismatch is a model
//! that responds to the wrong part of its own control surface.
//!
//! Setting a parameter is allocation-free and lock-free, so it is safe from the
//! audio thread. Resolve names to indices once during preparation
//! ([`ParameterSet::continuous_index`]) if you care about the string compare.

use crate::error::{FbmxError, Result};
use crate::header::ConditioningSchema;

#[derive(Debug, Clone)]
pub struct ParameterSet {
    schema: ConditioningSchema,
    /// User units, kept so a host can read back what it set.
    continuous_raw: Vec<f32>,
    /// The `[-1, 1]` values the network sees.
    continuous_norm: Vec<f32>,
    categorical: Vec<usize>,
    dirty: bool,
}

impl ParameterSet {
    /// Every parameter at its declared default.
    pub fn defaults(schema: &ConditioningSchema) -> Self {
        let continuous_raw: Vec<f32> = schema.continuous.iter().map(|p| p.default).collect();
        let continuous_norm: Vec<f32> = schema
            .continuous
            .iter()
            .map(|p| p.normalize(p.default))
            .collect();
        let categorical: Vec<usize> = schema
            .categorical
            .iter()
            .map(|p| p.default_index())
            .collect();
        Self {
            schema: schema.clone(),
            continuous_raw,
            continuous_norm,
            categorical,
            dirty: true,
        }
    }

    pub fn schema(&self) -> &ConditioningSchema {
        &self.schema
    }

    // -- lookup ----------------------------------------------------------
    pub fn continuous_index(&self, name: &str) -> Result<usize> {
        self.schema
            .continuous_index(name)
            .ok_or_else(|| FbmxError::UnknownParameter(name.to_string()))
    }

    pub fn categorical_index(&self, name: &str) -> Result<usize> {
        self.schema
            .categorical_index(name)
            .ok_or_else(|| FbmxError::UnknownParameter(name.to_string()))
    }

    // -- setting ---------------------------------------------------------
    /// Set a continuous parameter in user units. Out-of-range values clamp to
    /// the declared range: a host sending 11.0 to a 0..10 dial should get the
    /// top of the dial, not extrapolation into territory the model never saw.
    pub fn set_continuous(&mut self, index: usize, value: f32) {
        if index >= self.continuous_raw.len() {
            return;
        }
        let param = &self.schema.continuous[index];
        let value = if value.is_finite() {
            value
        } else {
            param.default
        };
        let normalized = param.normalize(value);
        if normalized != self.continuous_norm[index] {
            self.dirty = true;
        }
        self.continuous_raw[index] = value.clamp(param.minimum, param.maximum);
        self.continuous_norm[index] = normalized;
    }

    pub fn set_by_name(&mut self, name: &str, value: f32) -> Result<()> {
        let index = self.continuous_index(name)?;
        self.set_continuous(index, value);
        Ok(())
    }

    pub fn set_category_index(&mut self, index: usize, category: usize) {
        if index >= self.categorical.len() {
            return;
        }
        let clamped = category.min(self.schema.categorical[index].categories.len() - 1);
        if clamped != self.categorical[index] {
            self.dirty = true;
        }
        self.categorical[index] = clamped;
    }

    pub fn set_category(&mut self, name: &str, category: &str) -> Result<()> {
        let index = self.categorical_index(name)?;
        let value = self.schema.categorical[index].index_of(category)?;
        self.set_category_index(index, value);
        Ok(())
    }

    // -- reading ---------------------------------------------------------
    pub fn get(&self, name: &str) -> Option<f32> {
        self.schema
            .continuous_index(name)
            .map(|i| self.continuous_raw[i])
    }

    pub fn category(&self, name: &str) -> Option<&str> {
        let i = self.schema.categorical_index(name)?;
        Some(self.schema.categorical[i].categories[self.categorical[i]].as_str())
    }

    pub fn normalized(&self) -> &[f32] {
        &self.continuous_norm
    }

    pub fn categories(&self) -> &[usize] {
        &self.categorical
    }

    /// True if a value changed since the last call; clears the flag.
    pub fn take_dirty(&mut self) -> bool {
        let was = self.dirty;
        self.dirty = false;
        was
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::header::{CategoricalParam, ContinuousParam};

    fn schema() -> ConditioningSchema {
        ConditioningSchema {
            continuous: vec![ContinuousParam {
                name: "Input".into(),
                minimum: 0.0,
                maximum: 10.0,
                default: 5.0,
                unit: "dial".into(),
                description: String::new(),
            }],
            categorical: vec![CategoricalParam {
                name: "Ratio".into(),
                categories: vec!["4:1".into(), "20:1".into(), "All Buttons".into()],
                default: "4:1".into(),
                embedding_dim: 4,
                description: String::new(),
            }],
        }
    }

    #[test]
    fn defaults_are_normalised() {
        let p = ParameterSet::defaults(&schema());
        assert_eq!(p.normalized(), &[0.0]); // 5 of 0..10 -> 0
        assert_eq!(p.categories(), &[0]);
    }

    #[test]
    fn out_of_range_clamps() {
        let mut p = ParameterSet::defaults(&schema());
        p.set_by_name("Input", 99.0).unwrap();
        assert_eq!(p.normalized(), &[1.0]);
        assert_eq!(p.get("Input"), Some(10.0));
        p.set_by_name("Input", -5.0).unwrap();
        assert_eq!(p.normalized(), &[-1.0]);
    }

    #[test]
    fn non_finite_falls_back_to_default() {
        let mut p = ParameterSet::defaults(&schema());
        p.set_by_name("Input", f32::NAN).unwrap();
        assert_eq!(p.get("Input"), Some(5.0));
    }

    #[test]
    fn categories_are_by_name() {
        let mut p = ParameterSet::defaults(&schema());
        p.set_category("Ratio", "All Buttons").unwrap();
        assert_eq!(p.categories(), &[2]);
        assert_eq!(p.category("Ratio"), Some("All Buttons"));
        assert!(p.set_category("Ratio", "7:1").is_err());
        assert!(p.set_category("Nope", "4:1").is_err());
        assert!(p.set_by_name("Ratio", 1.0).is_err()); // categorical, not a dial
    }

    #[test]
    fn dirty_tracks_changes_only() {
        let mut p = ParameterSet::defaults(&schema());
        assert!(p.take_dirty());
        assert!(!p.take_dirty());
        p.set_by_name("Input", 5.0).unwrap(); // same value
        assert!(!p.take_dirty());
        p.set_by_name("Input", 7.0).unwrap();
        assert!(p.take_dirty());
    }
}
