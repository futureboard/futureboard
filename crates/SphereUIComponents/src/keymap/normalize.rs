use gpui::{KeyDownEvent, Keystroke};

/// Canonicalize an authored accelerator ("Ctrl+Shift+S") into a stable token.
pub fn canonical_accel(accel: &str) -> Option<String> {
    let mut ctrl = false;
    let mut shift = false;
    let mut alt = false;
    let mut base: Option<String> = None;
    for raw in accel.split('+') {
        let part = raw.trim().to_ascii_lowercase();
        match part.as_str() {
            "" => {}
            "ctrl" | "control" | "cmd" | "command" | "meta" | "super" => ctrl = true,
            "shift" => shift = true,
            "alt" | "option" | "opt" => alt = true,
            other => base = Some(canonical_key(other)),
        }
    }
    Some(join_token(ctrl, shift, alt, base?))
}

pub fn canonical_event(event: &KeyDownEvent) -> Option<String> {
    canonical_keystroke(&event.keystroke)
}

/// True when the keystroke is a bare modifier press. Those arrive as their own
/// key events on some platforms and must never terminate a chord recording.
pub fn is_modifier_only_key(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
        "control"
            | "ctrl"
            | "shift"
            | "alt"
            | "option"
            | "opt"
            | "platform"
            | "cmd"
            | "command"
            | "super"
            | "meta"
            | "win"
            | "function"
            | "fn"
            | "capslock"
    )
}

pub fn canonical_keystroke(keystroke: &Keystroke) -> Option<String> {
    let m = &keystroke.modifiers;
    let ctrl = m.control || m.platform;
    let shift = m.shift;
    let alt = m.alt;
    let key = keystroke.key.to_ascii_lowercase();
    if key.is_empty() || is_modifier_only_key(&key) {
        return None;
    }
    let base = canonical_key(&key);
    if base.is_empty() {
        return None;
    }
    Some(join_token(ctrl, shift, alt, base))
}

pub fn format_accel_display(token: &str) -> String {
    let lower = token.to_ascii_lowercase();
    let mut rest = lower.as_str();
    let mut parts = Vec::new();
    if rest.starts_with("alt+") {
        parts.push("Alt".to_string());
        rest = &rest[4..];
    }
    if rest.starts_with("ctrl+") {
        parts.push("Ctrl".to_string());
        rest = &rest[5..];
    }
    if rest.starts_with("shift+") {
        parts.push("Shift".to_string());
        rest = &rest[6..];
    }
    let key = match rest {
        "space" => "Space",
        "escape" => "Esc",
        "enter" => "Enter",
        "delete" => "Delete",
        "backspace" => "Backspace",
        "tab" => "Tab",
        "home" => "Home",
        "end" => "End",
        "pageup" => "PageUp",
        "pagedown" => "PageDown",
        "left" => "Left",
        "right" => "Right",
        "up" => "Up",
        "down" => "Down",
        other if other.len() == 1 => &other.to_ascii_uppercase(),
        other => other,
    };
    parts.push(key.to_string());
    parts.join("+")
}

pub fn event_to_accel_string(event: &KeyDownEvent) -> Option<String> {
    canonical_event(event).map(|token| format_accel_display(&token))
}

pub fn keystroke_to_accel_string(keystroke: &Keystroke) -> Option<String> {
    canonical_keystroke(keystroke).map(|token| format_accel_display(&token))
}

fn join_token(ctrl: bool, shift: bool, alt: bool, base: String) -> String {
    let mut out = String::new();
    if alt {
        out.push_str("alt+");
    }
    if ctrl {
        out.push_str("ctrl+");
    }
    if shift {
        out.push_str("shift+");
    }
    out.push_str(&base);
    out
}

pub fn canonical_key(key: &str) -> String {
    match key {
        "space" | "spacebar" => "space",
        "escape" | "esc" => "escape",
        "enter" | "return" | "numpad_enter" => "enter",
        "delete" | "del" => "delete",
        "backspace" => "backspace",
        "tab" => "tab",
        "home" => "home",
        "end" => "end",
        "pageup" | "page_up" | "pgup" => "pageup",
        "pagedown" | "page_down" | "pgdn" => "pagedown",
        "arrowleft" | "arrow_left" | "left" => "left",
        "arrowright" | "arrow_right" | "right" => "right",
        "arrowup" | "arrow_up" | "up" => "up",
        "arrowdown" | "arrow_down" | "down" => "down",
        "plus" => "=",
        "minus" => "-",
        other => other,
    }
    .to_string()
}

pub fn global_priority(command: &str) -> u8 {
    if command.starts_with("midi:") || command.starts_with("automation:") {
        2
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_accel_normalizes_modifiers() {
        assert_eq!(
            canonical_accel("Ctrl+Shift+S"),
            Some("ctrl+shift+s".to_string())
        );
        assert_eq!(
            canonical_accel("Shift+Space"),
            Some("shift+space".to_string())
        );
    }

    #[test]
    fn format_accel_display_round_trips() {
        let token = canonical_accel("Ctrl+Shift+S").unwrap();
        assert_eq!(format_accel_display(&token), "Ctrl+Shift+S");
    }
}
