//! The one owner of the DAW transport key (Space) in the Studio process.
//!
//! # Why this exists
//!
//! Space has to mean play/pause wherever the user is looking, and "wherever the
//! user is looking" spans four different keyboard worlds:
//!
//! - GPUI's own key bindings, for the arrangement and every panel it draws;
//! - in-process plug-in views (ARA, in-process VST3), whose native children own
//!   every key that reaches them — `WM_KEYDOWN` does not bubble to a parent;
//! - the out-of-process plug-in host, which has its own queues entirely;
//! - CEF, which is the only thing that can say whether a *DOM* text field has
//!   focus inside a built-in plug-in editor.
//!
//! Each of those needs its own interception, so there are several claim sites
//! and there is no avoiding that. What there *is* avoiding is several
//! **sinks**: before this module four separate places each ran
//! `transport:play-pause` for whatever they had collected, coalescing only
//! against themselves. Two of them catching the same physical press dispatched
//! the command twice, which plays and immediately stops — indistinguishable
//! from the key doing nothing, and the reason Space felt intermittent.
//!
//! So: claim sites stay plural, the sink is singular. Everything routes here,
//! this deduplicates, and exactly one place drains it.
//!
//! # Deduplication
//!
//! A press is identified by its Win32 message time (`MSG.time`,
//! `KBDLLHOOKSTRUCT.time`, `GetMessageTime()`), which is the same tick count
//! for the same hardware event in *every* process that sees it. That makes it a
//! real identity across the process boundary: the plug-in host's low-level hook
//! and this process's message hook seeing one press report the same number, and
//! the second one is dropped.
//!
//! Claims that cannot carry a time (a counter published by C++ with no
//! timestamp) are deduplicated by a short blind window instead. It is coarser,
//! and it is only ever a backstop — ownership is what keeps double claims from
//! happening in the first place.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Where a transport-key claim came from. Diagnostics only — the router treats
/// every source alike, because the press is the same press whoever saw it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportKeySource {
    /// The `WH_GETMESSAGE` hook, for a key headed into an in-process plug-in
    /// view (ARA / in-process VST3).
    EmbeddedView,
    /// A native editor shell's own window procedure (its chrome held focus).
    EditorShell,
    /// The C++ editor windows in the direct-audio VST3 bridge.
    NativeBridge,
    /// Reported over IPC by the separated plug-in host process.
    PluginHost,
    /// CEF's pre-key handler in a built-in plug-in editor. The only claim site
    /// that can see whether a DOM text field wanted the key.
    WebEditor,
}

impl TransportKeySource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::EmbeddedView => "embedded-view",
            Self::EditorShell => "editor-shell",
            Self::NativeBridge => "native-bridge",
            Self::PluginHost => "plugin-host",
            Self::WebEditor => "web-editor",
        }
    }
}

/// How long a press's message time stays known well enough to recognise a
/// second report of it. Generous: the plug-in host's report crosses a process
/// boundary and waits for the next IPC drain, so it can arrive a few frames
/// after the hook in this process already claimed the same key.
const DEDUP_WINDOW: Duration = Duration::from_millis(400);

/// Dedup window for a claim that arrives without a message time. Wide enough to
/// cover the fastest key auto-repeat Windows will produce (~30 Hz), short
/// enough that two deliberate presses are never merged.
const BLIND_WINDOW: Duration = Duration::from_millis(120);

/// Recent accepted presses kept for the dedup test. Fixed size: this is written
/// from window procedures and a message hook, which must not allocate.
const RECENT: usize = 8;

struct Ledger {
    /// `(message time, when we accepted it)`.
    seen: [(u32, Option<Instant>); RECENT],
    next: usize,
    /// Last accepted claim of any kind, for the timeless backstop.
    last: Option<Instant>,
}

static LEDGER: Mutex<Ledger> = Mutex::new(Ledger {
    seen: [(0, None); RECENT],
    next: 0,
    last: None,
});

/// Presses accepted and not yet turned into a transport command.
static PENDING: AtomicU32 = AtomicU32::new(0);

/// Whether the keyboard-routing trace is on (`FUTUREBOARD_KEY_DEBUG`).
///
/// Every step of this path is invisible from outside — a key is either seen or
/// it is not, and it either belongs to the transport or to whatever has focus —
/// so "Space did nothing" cannot say which happened without this.
pub fn key_debug() -> bool {
    static FLAG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FLAG.get_or_init(|| std::env::var_os("FUTUREBOARD_KEY_DEBUG").is_some())
}

/// Report a Space press claimed for the transport.
///
/// `time_ms` is the press's Win32 message time when the caller has one. Returns
/// whether the claim was accepted; `false` means this exact press was already
/// reported by another claim site and the caller has not lost anything.
///
/// Callers claim *before* the key reaches whatever had focus, so they must
/// swallow the key whatever this returns: a duplicate still means the press is
/// spoken for.
pub fn claim(source: TransportKeySource, time_ms: Option<u32>) -> bool {
    let now = Instant::now();
    let accepted = {
        let Ok(mut ledger) = LEDGER.lock() else {
            // A poisoned ledger must not cost the user the transport key; the
            // worst case without dedup is the behaviour this replaced.
            PENDING.fetch_add(1, Ordering::Relaxed);
            return true;
        };
        let duplicate = match time_ms {
            Some(time) => ledger.seen.iter().any(|&(seen, at)| {
                seen == time && at.is_some_and(|at| now.duration_since(at) < DEDUP_WINDOW)
            }),
            None => ledger
                .last
                .is_some_and(|at| now.duration_since(at) < BLIND_WINDOW),
        };
        if !duplicate {
            let slot = ledger.next;
            ledger.seen[slot] = (time_ms.unwrap_or(0), Some(now));
            ledger.next = (slot + 1) % RECENT;
            ledger.last = Some(now);
        }
        !duplicate
    };
    if accepted {
        PENDING.fetch_add(1, Ordering::Relaxed);
    }
    if key_debug() {
        eprintln!(
            "[Keyboard] claim source={} time={} accepted={accepted}",
            source.as_str(),
            time_ms
                .map(|t| t.to_string())
                .unwrap_or_else(|| "-".to_string()),
        );
    } else if !accepted {
        // Not gated, once: a duplicate reaching here is the shape that used to
        // cancel itself out, and knowing the routing caught it is worth a line.
        static REPORTED: AtomicU32 = AtomicU32::new(0);
        if REPORTED.fetch_add(1, Ordering::Relaxed) == 0 {
            eprintln!(
                "[Keyboard] duplicate transport-key claim from {} dropped (one press, one toggle)",
                source.as_str()
            );
        }
    }
    accepted
}

/// Whether the Space key-up now on its way belongs to the transport.
///
/// GPUI activates a focused, clickable element on the **key-up** of Space or
/// Enter — the keyboard equivalent of a click (see `div.rs`, "Press enter,
/// space to trigger click"). `Window::prevent_default` cannot stop that from
/// the key-down, because `default_prevented` is cleared for every platform
/// input event, so a key-down's veto is already gone by the time the key-up is
/// dispatched.
///
/// That is one press doing two opposite things: click Play with the mouse (which
/// focuses it), then press Space — the key-down pauses, and the key-up presses
/// the still-focused Play or Record button again. It reads as the transport
/// re-arming itself, and it keeps happening because the button stays focused.
///
/// So the key-down claims the key-up as well: whoever routed Space to the
/// transport sets this, and the key-up handler takes it and swallows the event
/// before it reaches the focused control. Enter is untouched — buttons keep
/// their keyboard activation, they just do not answer to the transport key.
static KEY_UP_CLAIMED: AtomicBool = AtomicBool::new(false);

/// Claim the key-up of the Space press being handled right now.
///
/// Called from every key-down path that turns Space into a transport command,
/// including auto-repeats: the key-up of a held Space is still the transport's.
pub fn claim_space_key_up() {
    KEY_UP_CLAIMED.store(true, Ordering::Relaxed);
}

/// Drop a claim left over from an earlier press.
///
/// Called at the top of a key-down handler for a *fresh* press (not a repeat).
/// A key-up can only follow a key-down, so this bounds a claim's life to the
/// press that made it: one lost key-up — the window deactivating mid-press, say
/// — cannot make the next unrelated key-up disappear.
pub fn forget_space_key_up() {
    KEY_UP_CLAIMED.store(false, Ordering::Relaxed);
}

/// Take the claim. `true` means this key-up is the transport's and the handler
/// must swallow it rather than let it activate whatever holds focus.
pub fn take_space_key_up_claim() -> bool {
    KEY_UP_CLAIMED.swap(false, Ordering::Relaxed)
}

/// Take the presses waiting to become transport commands.
///
/// Drained by exactly one caller — `StudioLayout::poll_native_audio`. More than
/// one press per drain is a burst that piled up behind a plug-in's own modal
/// loop, not two deliberate presses, so the caller coalesces them into a single
/// play/pause rather than replaying each.
pub fn take() -> u32 {
    PENDING.swap(0, Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialises the tests: the ledger is process-wide, which is the point of
    /// it, and two tests interleaving would deduplicate each other's presses.
    static LOCK: Mutex<()> = Mutex::new(());

    fn reset() {
        {
            let mut ledger = LEDGER.lock().unwrap_or_else(|e| e.into_inner());
            ledger.seen = [(0, None); RECENT];
            ledger.next = 0;
            ledger.last = None;
        }
        PENDING.store(0, Ordering::Relaxed);
    }

    #[test]
    fn one_press_seen_by_two_claim_sites_is_one_toggle() {
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        assert!(claim(TransportKeySource::EmbeddedView, Some(12_345)));
        assert!(!claim(TransportKeySource::PluginHost, Some(12_345)));
        assert_eq!(take(), 1);
    }

    #[test]
    fn two_presses_are_two_toggles() {
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        assert!(claim(TransportKeySource::EmbeddedView, Some(1_000)));
        assert!(claim(TransportKeySource::EmbeddedView, Some(1_400)));
        assert_eq!(take(), 2);
    }

    #[test]
    fn a_timeless_claim_falls_back_to_the_blind_window() {
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        assert!(claim(TransportKeySource::NativeBridge, None));
        assert!(!claim(TransportKeySource::NativeBridge, None));
        assert_eq!(take(), 1);
    }

    /// One press, one toggle — the key-up must not add a second one by
    /// clicking whatever the mouse left focused.
    #[test]
    fn a_claimed_key_up_is_swallowed_exactly_once() {
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        forget_space_key_up();
        assert!(!take_space_key_up_claim());
        claim_space_key_up();
        assert!(take_space_key_up_claim());
        // The transport took it; a second key-up (a different key, or one that
        // arrived without its press) is nobody's but the focused control's.
        assert!(!take_space_key_up_claim());
    }

    /// A press whose key-up never arrived — the window deactivated mid-press —
    /// must not cost the next press's key-up.
    #[test]
    fn a_new_press_drops_a_stale_claim() {
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        claim_space_key_up();
        forget_space_key_up();
        assert!(!take_space_key_up_claim());
    }

    #[test]
    fn draining_leaves_nothing_behind() {
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        assert!(claim(TransportKeySource::EditorShell, Some(7)));
        assert_eq!(take(), 1);
        assert_eq!(take(), 0);
    }
}
