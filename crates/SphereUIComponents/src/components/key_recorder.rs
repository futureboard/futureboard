use gpui::{KeyDownEvent, Keystroke};

use crate::keymap::{is_modifier_only_key, keystroke_to_accel_string};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeyRecorderState {
    pub armed: bool,
    pub captured: Option<String>,
    pub error: Option<String>,
}

impl KeyRecorderState {
    pub fn arm(&mut self) {
        self.armed = true;
        self.captured = None;
        self.error = None;
    }

    pub fn disarm(&mut self) {
        self.armed = false;
    }

    pub fn clear(&mut self) {
        self.captured = None;
        self.error = None;
        self.armed = false;
    }

    pub fn handle_key(&mut self, event: &KeyDownEvent) -> bool {
        if event.is_held {
            return self.armed;
        }
        self.handle_keystroke(&event.keystroke)
    }

    /// Record a single keystroke. Returns `true` when the recorder consumed it,
    /// which the caller uses to stop the keystroke from reaching normal action
    /// dispatch.
    pub fn handle_keystroke(&mut self, keystroke: &Keystroke) -> bool {
        if !self.armed {
            return false;
        }
        let key = keystroke.key.as_str();
        // A bare modifier press is the start of a chord, not the chord itself.
        if is_modifier_only_key(key) {
            return true;
        }
        if key.eq_ignore_ascii_case("escape") && !keystroke.modifiers.modified() {
            self.disarm();
            return true;
        }
        if let Some(accel) = keystroke_to_accel_string(keystroke) {
            self.captured = Some(accel);
            self.error = None;
            self.armed = false;
            return true;
        }
        self.error = Some("Unsupported key chord".to_string());
        true
    }
}

pub fn key_recorder_field(
    state: &KeyRecorderState,
    placeholder: &str,
    armed: bool,
) -> gpui::AnyElement {
    use crate::theme::Colors;
    use gpui::{div, px, IntoElement, ParentElement, Styled};

    let label = state
        .captured
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            if armed {
                "Press keys…".to_string()
            } else {
                placeholder.to_string()
            }
        });

    div()
        .flex()
        .items_center()
        .h(px(28.0))
        .px(px(8.0))
        .rounded_md()
        .bg(Colors::surface_input())
        .border(px(1.0))
        .border_color(if armed {
            Colors::border_focus()
        } else {
            Colors::border_subtle()
        })
        .text_size(px(11.0))
        .text_color(if state.captured.is_some() {
            Colors::text_primary()
        } else {
            Colors::text_muted()
        })
        .child(label)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::Modifiers;

    fn keystroke(key: &str, modifiers: Modifiers) -> Keystroke {
        Keystroke {
            modifiers,
            key: key.to_string(),
            key_char: None,
        }
    }

    #[test]
    fn disarmed_recorder_ignores_keys() {
        let mut state = KeyRecorderState::default();
        assert!(!state.handle_keystroke(&keystroke("s", Modifiers::control())));
        assert_eq!(state.captured, None);
    }

    #[test]
    fn bare_modifier_does_not_end_recording() {
        let mut state = KeyRecorderState::default();
        state.arm();
        assert!(state.handle_keystroke(&keystroke("control", Modifiers::control())));
        assert!(state.armed);
        assert_eq!(state.captured, None);

        assert!(state.handle_keystroke(&keystroke("s", Modifiers::control_shift())));
        assert!(!state.armed);
        assert_eq!(state.captured.as_deref(), Some("Ctrl+Shift+S"));
    }

    #[test]
    fn escape_cancels_without_capturing() {
        let mut state = KeyRecorderState::default();
        state.arm();
        assert!(state.handle_keystroke(&keystroke("escape", Modifiers::none())));
        assert!(!state.armed);
        assert_eq!(state.captured, None);
    }

    #[test]
    fn escape_with_modifier_is_recordable() {
        let mut state = KeyRecorderState::default();
        state.arm();
        assert!(state.handle_keystroke(&keystroke("escape", Modifiers::shift())));
        assert_eq!(state.captured.as_deref(), Some("Shift+Esc"));
    }
}
