//! Shared vertical scrub gesture for compact numeric readouts.
//!
//! The anchor lives in the active GPUI drag payload, rather than in a render
//! closure. This is important because changing the scrubbed value rerenders
//! the control while the pointer is still down.

use std::sync::{Arc, Mutex};

use gpui::{Empty, IntoElement, Render, Window};

#[derive(Clone, Debug)]
pub(crate) struct SpinDrag {
    id: String,
    start_value: f64,
    anchor_y: Arc<Mutex<Option<f32>>>,
}

impl SpinDrag {
    pub(crate) fn new(id: impl Into<String>, start_value: f64) -> Self {
        Self {
            id: id.into(),
            start_value,
            anchor_y: Arc::new(Mutex::new(None)),
        }
    }

    pub(crate) fn begin(&self) {
        *self
            .anchor_y
            .lock()
            .expect("spin drag anchor mutex poisoned") = None;
    }

    pub(crate) fn matches(&self, id: &str) -> bool {
        self.id == id
    }

    pub(crate) fn value_at(
        &self,
        current_y: f32,
        units_per_pixel: f64,
        min: f64,
        max: f64,
        quantum: Option<f64>,
    ) -> f64 {
        let mut anchor = self
            .anchor_y
            .lock()
            .expect("spin drag anchor mutex poisoned");
        let start_y = *anchor.get_or_insert(current_y);
        let mut delta = f64::from(start_y - current_y) * units_per_pixel;
        if let Some(quantum) = quantum.filter(|quantum| *quantum > 0.0) {
            delta = (delta / quantum).round() * quantum;
        }
        (self.start_value + delta).clamp(min, max)
    }
}

impl Render for SpinDrag {
    fn render(&mut self, _window: &mut Window, _cx: &mut gpui::Context<Self>) -> impl IntoElement {
        Empty
    }
}

#[cfg(test)]
mod tests {
    use super::SpinDrag;

    #[test]
    fn anchor_survives_payload_clones_across_renders() {
        let drag = SpinDrag::new("gain", 10.0);
        drag.begin();
        assert_eq!(drag.value_at(100.0, 0.2, 0.0, 20.0, None), 10.0);

        let rerendered_handler_payload = drag.clone();
        assert_eq!(
            rerendered_handler_payload.value_at(75.0, 0.2, 0.0, 20.0, None),
            15.0
        );
    }

    #[test]
    fn quantizes_and_clamps_scrubbed_values() {
        let drag = SpinDrag::new("stepper", 10.3);
        drag.begin();
        assert_eq!(drag.value_at(50.0, 0.2, 0.0, 20.0, Some(1.0)), 10.3);
        assert_eq!(drag.value_at(45.0, 0.2, 0.0, 20.0, Some(1.0)), 11.3);
        assert_eq!(drag.value_at(-200.0, 0.2, 0.0, 20.0, Some(1.0)), 20.0);
    }
}
