//! CEF host for built-in plugin editors.
//!
//! Built-in plugins ship their editor as a compiled React app embedded in the
//! plugin library. This module owns the browser-process side: the CEF runtime,
//! the `mikoplugin://` asset registry, and the child views parented into the
//! editor window shell.
//!
//! ## Availability
//!
//! Everything here is behind the `builtin-plugin-editor` feature. Without it,
//! [`availability`] reports why the editor cannot open, and the caller surfaces
//! that instead of showing an empty window. That is deliberate: a checkout with
//! no CEF SDK must still build and run.
//!
//! ## Threading
//!
//! CEF is initialized lazily on the GPUI UI thread and must only be driven from
//! there ([`CefRuntime`] enforces this). [`pump`] has to be called from the UI
//! loop or the browser never paints.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

/// Why a built-in editor can or cannot be hosted right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostAvailability {
    /// CEF is compiled in and the plugin has embedded UI assets.
    Ready,
    /// The binary was built without the `builtin-plugin-editor` feature.
    NotCompiledIn,
    /// The plugin id is not a built-in that ships an editor.
    NoEditorForPlugin(String),
    /// The plugin's editor `dist/` was not built when the library was compiled.
    UiNotEmbedded(String),
    /// CEF failed to start.
    RuntimeFailed(String),
}

impl fmt::Display for HostAvailability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ready => write!(f, "ready"),
            Self::NotCompiledIn => write!(
                f,
                "built-in plugin editors are not available in this build \
                 (rebuild with --features builtin-plugin-editor)"
            ),
            Self::NoEditorForPlugin(id) => write!(f, "{id} does not ship an editor UI"),
            Self::UiNotEmbedded(id) => write!(
                f,
                "{id} was compiled without its editor UI (run `bun run build` in its editor bundle)"
            ),
            Self::RuntimeFailed(err) => write!(f, "CEF failed to start: {err}"),
        }
    }
}

/// The `mikoplugin://` origin for a built-in plugin.
///
/// Accepts both identifier forms a built-in travels under: the catalog id
/// (`builtin:rodharerist`) and the class id (`rodharerist`) that insert slots
/// actually store. Resolution is validated against the built-in catalog, so an
/// unprefixed external class id is never treated as a built-in.
pub fn origin_for_plugin_id(plugin_id: &str) -> Option<&'static str> {
    SpherePluginHost::resolve_builtin_stem(plugin_id)
}

/// Map an editor's string param id to `plugin_id`'s u32 wire index (the id
/// carried by the shared param ring into the plugin-host process). `None` for
/// unknown plugins or ids — the caller logs and drops, never guesses.
#[cfg(feature = "builtin-plugin-editor")]
pub fn builtin_param_index(plugin_id: &str, param_id: &str) -> Option<u32> {
    match origin_for_plugin_id(plugin_id)? {
        rodharerist::ui::UI_ORIGIN => rodharerist::ui_param_index(param_id),
        equz8::ui::UI_ORIGIN => equz8::ui_param_index(param_id),
        verbspace::ui::UI_ORIGIN => verbspace::ui_param_index(param_id),
        echospace::ui::UI_ORIGIN => echospace::ui_param_index(param_id),
        fa2a::ui::UI_ORIGIN => fa2a::ui_param_index(param_id),
        fa76::ui::UI_ORIGIN => fa76::ui_param_index(param_id),
        burnlimit::ui::UI_ORIGIN => burnlimit::ui_param_index(param_id),
        clipper67::ui::UI_ORIGIN => clipper67::ui_param_index(param_id),
        transient::ui::UI_ORIGIN => transient::ui_param_index(param_id),
        wrapsynth::ui::UI_ORIGIN => wrapsynth::ui_param_index(param_id),
        zcomp::ui::UI_ORIGIN => zcomp::ui_param_index(param_id),
        mixstation::ui::UI_ORIGIN => mixstation::ui_param_index(param_id),
        _ => None,
    }
}

/// Without the editor feature no plugin table is linked in; every id is
/// unknown (and no editor exists to send one anyway).
#[cfg(not(feature = "builtin-plugin-editor"))]
pub fn builtin_param_index(_plugin_id: &str, _param_id: &str) -> Option<u32> {
    None
}

/// Authoritative main-process mirror of every built-in insert's parameter
/// state, keyed by insert slot id. The single source of truth for what a
/// built-in insert *is*: seeded from the project blob, updated by every
/// forwarded editor edit, read back for `selectInstance` state, project save,
/// and host restore/respawn replay. Process-wide `Mutex` like `INBOUND` —
/// touched only from the UI thread today, but nothing about it is
/// thread-bound.
#[cfg(feature = "builtin-plugin-editor")]
mod state_mirror {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    use super::origin_for_plugin_id;

    /// One insert's mirrored params, tagged with the built-in that owns them.
    ///
    /// The map is keyed by insert slot id alone, and a slot can be re-pointed at
    /// a different plugin, so the tag is what proves a stored blob belongs to
    /// the plugin asking for it. Untagged, an EQ-Z8 edit would fold into
    /// rodharerist's parameter table and be written to the project as such.
    /// Boxed so a slot of the map stays small regardless of which DSP core has
    /// the largest `Params`.
    enum BuiltinParams {
        Rodhareist(Box<rodharerist::Params>),
        Equz8(Box<equz8::Params>),
        Verbspace(Box<verbspace::Params>),
        Echospace(Box<echospace::Params>),
        Fa2a(Box<fa2a::Params>),
        Fa76(Box<fa76::Params>),
        BurnLimit(Box<burnlimit::Params>),
        Clipper67(Box<clipper67::Params>),
        Transient(Box<transient::Params>),
        WrapSynth(Box<wrapsynth::Params>),
        Zcomp(Box<zcomp::Params>),
        MixStation(Box<mixstation::Params>),
    }

    impl BuiltinParams {
        fn origin(&self) -> &'static str {
            match self {
                Self::Rodhareist(_) => rodharerist::ui::UI_ORIGIN,
                Self::Equz8(_) => equz8::ui::UI_ORIGIN,
                Self::Verbspace(_) => verbspace::ui::UI_ORIGIN,
                Self::Echospace(_) => echospace::ui::UI_ORIGIN,
                Self::Fa2a(_) => fa2a::ui::UI_ORIGIN,
                Self::Fa76(_) => fa76::ui::UI_ORIGIN,
                Self::BurnLimit(_) => burnlimit::ui::UI_ORIGIN,
                Self::Clipper67(_) => clipper67::ui::UI_ORIGIN,
                Self::Transient(_) => transient::ui::UI_ORIGIN,
                Self::WrapSynth(_) => wrapsynth::ui::UI_ORIGIN,
                Self::Zcomp(_) => zcomp::ui::UI_ORIGIN,
                Self::MixStation(_) => mixstation::ui::UI_ORIGIN,
            }
        }

        /// The plugin's own defaults, or `None` for a built-in with no mirror.
        fn defaults(origin: &str) -> Option<Self> {
            match origin {
                rodharerist::ui::UI_ORIGIN => {
                    Some(Self::Rodhareist(Box::new(rodharerist::default_params())))
                }
                equz8::ui::UI_ORIGIN => Some(Self::Equz8(Box::new(equz8::default_params()))),
                verbspace::ui::UI_ORIGIN => {
                    Some(Self::Verbspace(Box::new(verbspace::default_params())))
                }
                echospace::ui::UI_ORIGIN => {
                    Some(Self::Echospace(Box::new(echospace::default_params())))
                }
                fa2a::ui::UI_ORIGIN => Some(Self::Fa2a(Box::new(fa2a::default_params()))),
                fa76::ui::UI_ORIGIN => Some(Self::Fa76(Box::new(fa76::default_params()))),
                burnlimit::ui::UI_ORIGIN => {
                    Some(Self::BurnLimit(Box::new(burnlimit::default_params())))
                }
                clipper67::ui::UI_ORIGIN => {
                    Some(Self::Clipper67(Box::new(clipper67::default_params())))
                }
                transient::ui::UI_ORIGIN => {
                    Some(Self::Transient(Box::new(transient::default_params())))
                }
                wrapsynth::ui::UI_ORIGIN => {
                    Some(Self::WrapSynth(Box::new(wrapsynth::default_params())))
                }
                zcomp::ui::UI_ORIGIN => Some(Self::Zcomp(Box::new(zcomp::default_params()))),
                mixstation::ui::UI_ORIGIN => {
                    Some(Self::MixStation(Box::new(mixstation::default_params())))
                }
                _ => None,
            }
        }
    }

    static BUILTIN_STATE: OnceLock<Mutex<HashMap<String, BuiltinParams>>> = OnceLock::new();

    fn map() -> &'static Mutex<HashMap<String, BuiltinParams>> {
        BUILTIN_STATE.get_or_init(|| Mutex::new(HashMap::new()))
    }

    /// The entry for `insert_id`, created at `origin`'s defaults when absent —
    /// or when a *different* built-in left state behind in this slot.
    fn entry_for<'a>(
        states: &'a mut HashMap<String, BuiltinParams>,
        insert_id: &str,
        origin: &str,
    ) -> Option<&'a mut BuiltinParams> {
        if states.get(insert_id).map(BuiltinParams::origin) != Some(origin) {
            states.insert(insert_id.to_string(), BuiltinParams::defaults(origin)?);
        }
        states.get_mut(insert_id)
    }

    /// Fold one forwarded editor edit (wire index + raw value) into the
    /// insert's mirrored params. Creates the entry at defaults on first edit.
    /// `clear_clip` (an action, not state) is ignored.
    pub fn builtin_state_apply(plugin_id: &str, insert_id: &str, wire_index: u32, value: f32) {
        let Some(origin) = origin_for_plugin_id(plugin_id) else {
            return;
        };
        // Resolve the index *before* touching the map, so an unknown one never
        // materializes default state for an insert that had none.
        let known = match origin {
            rodharerist::ui::UI_ORIGIN => rodharerist::ui_param_id(wire_index).is_some(),
            equz8::ui::UI_ORIGIN => equz8::ui_param_id(wire_index).is_some(),
            verbspace::ui::UI_ORIGIN => verbspace::ui_param_id(wire_index).is_some(),
            echospace::ui::UI_ORIGIN => echospace::ui_param_id(wire_index).is_some(),
            fa2a::ui::UI_ORIGIN => fa2a::ui_param_id(wire_index).is_some(),
            fa76::ui::UI_ORIGIN => fa76::ui_param_id(wire_index).is_some(),
            burnlimit::ui::UI_ORIGIN => burnlimit::ui_param_id(wire_index).is_some(),
            clipper67::ui::UI_ORIGIN => clipper67::ui_param_id(wire_index).is_some(),
            transient::ui::UI_ORIGIN => transient::ui_param_id(wire_index).is_some(),
            wrapsynth::ui::UI_ORIGIN => wrapsynth::ui_param_id(wire_index).is_some(),
            zcomp::ui::UI_ORIGIN => zcomp::ui_param_id(wire_index).is_some(),
            mixstation::ui::UI_ORIGIN => mixstation::ui_param_id(wire_index).is_some(),
            _ => false,
        };
        if !known {
            return;
        }
        let Ok(mut states) = map().lock() else {
            return;
        };
        match entry_for(&mut states, insert_id, origin) {
            Some(BuiltinParams::Rodhareist(params)) => {
                if let Some(id) = rodharerist::ui_param_id(wire_index) {
                    let _ = rodharerist::apply_to_params(params, id, value);
                }
            }
            Some(BuiltinParams::Equz8(params)) => {
                let _ = equz8::ipc::apply_wire_param(params, wire_index, value);
            }
            Some(BuiltinParams::Verbspace(params)) => {
                let _ = verbspace::ipc::apply_wire_param(params, wire_index, value);
            }
            Some(BuiltinParams::Echospace(params)) => {
                let _ = echospace::ipc::apply_wire_param(params, wire_index, value);
            }
            Some(BuiltinParams::Fa2a(params)) => {
                let _ = fa2a::ipc::apply_wire_param(params, wire_index, value);
            }
            Some(BuiltinParams::Fa76(params)) => {
                let _ = fa76::ipc::apply_wire_param(params, wire_index, value);
            }
            Some(BuiltinParams::BurnLimit(params)) => {
                let _ = burnlimit::ipc::apply_wire_param(params, wire_index, value);
            }
            Some(BuiltinParams::Clipper67(params)) => {
                let _ = clipper67::ipc::apply_wire_param(params, wire_index, value);
            }
            Some(BuiltinParams::Transient(params)) => {
                let _ = transient::ipc::apply_wire_param(params, wire_index, value);
            }
            Some(BuiltinParams::WrapSynth(params)) => {
                let _ = wrapsynth::ipc::apply_wire_param(params, wire_index, value);
            }
            Some(BuiltinParams::Zcomp(params)) => {
                let _ = zcomp::ipc::apply_wire_param(params, wire_index, value);
            }
            Some(BuiltinParams::MixStation(params)) => {
                let _ = mixstation::ipc::apply_wire_param(params, wire_index, value);
            }
            None => {}
        }
    }

    /// Seed the mirror from `plugin_id`'s persisted state blob — only when this
    /// plugin has no entry yet, so live edits always win over stale disk state.
    pub fn builtin_state_seed(plugin_id: &str, insert_id: &str, state_bytes: &[u8]) {
        let Some(origin) = origin_for_plugin_id(plugin_id) else {
            return;
        };
        let Ok(text) = std::str::from_utf8(state_bytes) else {
            return;
        };
        let parsed = match origin {
            rodharerist::ui::UI_ORIGIN => rodharerist::RodhareistState::from_json(text)
                .ok()
                .map(|state| BuiltinParams::Rodhareist(Box::new(state.params))),
            equz8::ui::UI_ORIGIN => equz8::ipc::Equz8State::from_json(text)
                .ok()
                .map(|state| BuiltinParams::Equz8(Box::new(state.params))),
            verbspace::ui::UI_ORIGIN => verbspace::ipc::VerbspaceState::from_json(text)
                .ok()
                .map(|state| BuiltinParams::Verbspace(Box::new(state.params))),
            echospace::ui::UI_ORIGIN => echospace::ipc::EchospaceState::from_json(text)
                .ok()
                .map(|state| BuiltinParams::Echospace(Box::new(state.params))),
            fa2a::ui::UI_ORIGIN => fa2a::ipc::Fa2aState::from_json(text)
                .ok()
                .map(|state| BuiltinParams::Fa2a(Box::new(state.params))),
            fa76::ui::UI_ORIGIN => fa76::ipc::Fa76State::from_json(text)
                .ok()
                .map(|state| BuiltinParams::Fa76(Box::new(state.params))),
            burnlimit::ui::UI_ORIGIN => burnlimit::ipc::BurnLimitState::from_json(text)
                .ok()
                .map(|state| BuiltinParams::BurnLimit(Box::new(state.params))),
            clipper67::ui::UI_ORIGIN => clipper67::ipc::Clipper67State::from_json(text)
                .ok()
                .map(|state| BuiltinParams::Clipper67(Box::new(state.params))),
            transient::ui::UI_ORIGIN => transient::ipc::TransientState::from_json(text)
                .ok()
                .map(|state| BuiltinParams::Transient(Box::new(state.params))),
            wrapsynth::ui::UI_ORIGIN => wrapsynth::ipc::WrapSynthState::from_json(text)
                .ok()
                .map(|state| BuiltinParams::WrapSynth(Box::new(state.params))),
            zcomp::ui::UI_ORIGIN => zcomp::ipc::ZcompState::from_json(text)
                .ok()
                .map(|state| BuiltinParams::Zcomp(Box::new(state.params))),
            mixstation::ui::UI_ORIGIN => mixstation::ipc::MixStationState::from_json(text)
                .ok()
                .map(|state| BuiltinParams::MixStation(Box::new(state.params))),
            _ => None,
        };
        let Some(parsed) = parsed else {
            return;
        };
        if let Ok(mut states) = map().lock() {
            match states.get(insert_id) {
                // This plugin's live state is authoritative — leave it alone.
                Some(existing) if existing.origin() == origin => {}
                // Absent, or another plugin's leftovers in the same slot.
                _ => {
                    states.insert(insert_id.to_string(), parsed);
                }
            }
        }
    }

    /// Serialized state blob for persistence / `selectInstance`. `None` when the
    /// slot holds no state, or holds a *different* built-in's state — that is
    /// never handed to the asking plugin.
    pub fn builtin_state_bytes(plugin_id: &str, insert_id: &str) -> Option<Vec<u8>> {
        let origin = origin_for_plugin_id(plugin_id)?;
        let states = map().lock().ok()?;
        let json = match states.get(insert_id)? {
            BuiltinParams::Rodhareist(params) if origin == rodharerist::ui::UI_ORIGIN => {
                rodharerist::RodhareistState::new((**params).clone())
                    .to_json()
                    .ok()?
            }
            BuiltinParams::Equz8(params) if origin == equz8::ui::UI_ORIGIN => {
                equz8::ipc::Equz8State::new((**params).clone())
                    .to_json()
                    .ok()?
            }
            BuiltinParams::Verbspace(params) if origin == verbspace::ui::UI_ORIGIN => {
                verbspace::ipc::VerbspaceState::new((**params).clone())
                    .to_json()
                    .ok()?
            }
            BuiltinParams::Echospace(params) if origin == echospace::ui::UI_ORIGIN => {
                echospace::ipc::EchospaceState::new((**params).clone())
                    .to_json()
                    .ok()?
            }
            BuiltinParams::Fa2a(params) if origin == fa2a::ui::UI_ORIGIN => {
                fa2a::ipc::Fa2aState::new((**params).clone())
                    .to_json()
                    .ok()?
            }
            BuiltinParams::Fa76(params) if origin == fa76::ui::UI_ORIGIN => {
                fa76::ipc::Fa76State::new((**params).clone())
                    .to_json()
                    .ok()?
            }
            BuiltinParams::BurnLimit(params) if origin == burnlimit::ui::UI_ORIGIN => {
                burnlimit::ipc::BurnLimitState::new((**params).clone())
                    .to_json()
                    .ok()?
            }
            BuiltinParams::Clipper67(params) if origin == clipper67::ui::UI_ORIGIN => {
                clipper67::ipc::Clipper67State::new((**params).clone())
                    .to_json()
                    .ok()?
            }
            BuiltinParams::Transient(params) if origin == transient::ui::UI_ORIGIN => {
                transient::ipc::TransientState::new((**params).clone())
                    .to_json()
                    .ok()?
            }
            BuiltinParams::WrapSynth(params) if origin == wrapsynth::ui::UI_ORIGIN => {
                wrapsynth::ipc::WrapSynthState::new((**params).clone())
                    .to_json()
                    .ok()?
            }
            BuiltinParams::Zcomp(params) if origin == zcomp::ui::UI_ORIGIN => {
                zcomp::ipc::ZcompState::new((**params).clone())
                    .to_json()
                    .ok()?
            }
            BuiltinParams::MixStation(params) if origin == mixstation::ui::UI_ORIGIN => {
                mixstation::ipc::MixStationState::new((**params).clone())
                    .to_json()
                    .ok()?
            }
            _ => return None,
        };
        Some(json.into_bytes())
    }

    /// The mirrored state as `(wire index, raw value)` pairs, in replay-safe
    /// order — pushed through the live param channel to rebuild a host DSP
    /// after project open or host respawn. Empty when the insert has no
    /// mirrored state for this plugin (fresh insert: host defaults already
    /// match).
    pub fn builtin_state_replay(plugin_id: &str, insert_id: &str) -> Vec<(u32, f32)> {
        let Some(origin) = origin_for_plugin_id(plugin_id) else {
            return Vec::new();
        };
        let Ok(states) = map().lock() else {
            return Vec::new();
        };
        match states.get(insert_id) {
            Some(BuiltinParams::Rodhareist(params)) if origin == rodharerist::ui::UI_ORIGIN => {
                rodharerist::ui_values(params)
                    .into_iter()
                    .filter_map(|(id, value)| rodharerist::ui_param_index(id).map(|i| (i, value)))
                    .collect()
            }
            Some(BuiltinParams::Equz8(params)) if origin == equz8::ui::UI_ORIGIN => {
                equz8::ipc::ui_values(params)
                    .into_iter()
                    .filter_map(|(id, value)| equz8::ui_param_index(id).map(|i| (i, value)))
                    .collect()
            }
            Some(BuiltinParams::Verbspace(params)) if origin == verbspace::ui::UI_ORIGIN => {
                verbspace::ipc::ui_values(params)
                    .into_iter()
                    .filter_map(|(id, value)| verbspace::ui_param_index(id).map(|i| (i, value)))
                    .collect()
            }
            Some(BuiltinParams::Echospace(params)) if origin == echospace::ui::UI_ORIGIN => {
                echospace::ipc::ui_values(params)
                    .into_iter()
                    .filter_map(|(id, value)| echospace::ui_param_index(id).map(|i| (i, value)))
                    .collect()
            }
            Some(BuiltinParams::Fa2a(params)) if origin == fa2a::ui::UI_ORIGIN => {
                fa2a::ipc::ui_values(params)
                    .into_iter()
                    .filter_map(|(id, value)| fa2a::ui_param_index(id).map(|i| (i, value)))
                    .collect()
            }
            Some(BuiltinParams::Fa76(params)) if origin == fa76::ui::UI_ORIGIN => {
                fa76::ipc::ui_values(params)
                    .into_iter()
                    .filter_map(|(id, value)| fa76::ui_param_index(id).map(|i| (i, value)))
                    .collect()
            }
            Some(BuiltinParams::BurnLimit(params)) if origin == burnlimit::ui::UI_ORIGIN => {
                burnlimit::ipc::ui_values(params)
                    .into_iter()
                    .filter_map(|(id, value)| burnlimit::ui_param_index(id).map(|i| (i, value)))
                    .collect()
            }
            Some(BuiltinParams::Clipper67(params)) if origin == clipper67::ui::UI_ORIGIN => {
                clipper67::ipc::ui_values(params)
                    .into_iter()
                    .filter_map(|(id, value)| clipper67::ui_param_index(id).map(|i| (i, value)))
                    .collect()
            }
            Some(BuiltinParams::Transient(params)) if origin == transient::ui::UI_ORIGIN => {
                transient::ipc::ui_values(params)
                    .into_iter()
                    .filter_map(|(id, value)| transient::ui_param_index(id).map(|i| (i, value)))
                    .collect()
            }
            Some(BuiltinParams::WrapSynth(params)) if origin == wrapsynth::ui::UI_ORIGIN => {
                wrapsynth::ipc::ui_values(params)
                    .into_iter()
                    .filter_map(|(id, value)| wrapsynth::ui_param_index(id).map(|i| (i, value)))
                    .collect()
            }
            Some(BuiltinParams::Zcomp(params)) if origin == zcomp::ui::UI_ORIGIN => {
                zcomp::ipc::ui_values(params)
                    .into_iter()
                    .filter_map(|(id, value)| zcomp::ui_param_index(id).map(|i| (i, value)))
                    .collect()
            }
            Some(BuiltinParams::MixStation(params)) if origin == mixstation::ui::UI_ORIGIN => {
                mixstation::ipc::ui_values(params)
                    .into_iter()
                    .enumerate()
                    .map(|(index, value)| (index as u32, value))
                    .collect()
            }
            _ => Vec::new(),
        }
    }

    /// Drop one insert's mirrored state (insert removed/unloaded). Not keyed by
    /// plugin: the slot is going away whoever owned it.
    pub fn builtin_state_remove(insert_id: &str) {
        if let Ok(mut states) = map().lock() {
            states.remove(insert_id);
        }
    }

    /// Drop everything (project close).
    pub fn builtin_state_clear() {
        if let Ok(mut states) = map().lock() {
            states.clear();
        }
    }
}

#[cfg(feature = "builtin-plugin-editor")]
pub use state_mirror::{
    builtin_state_apply, builtin_state_bytes, builtin_state_clear, builtin_state_remove,
    builtin_state_replay, builtin_state_seed,
};

/// Featureless no-ops: without the editor there is no param wire, so there is
/// no state to mirror.
#[cfg(not(feature = "builtin-plugin-editor"))]
mod state_mirror_stubs {
    pub fn builtin_state_apply(_plugin_id: &str, _insert_id: &str, _wire_index: u32, _value: f32) {}
    pub fn builtin_state_seed(_plugin_id: &str, _insert_id: &str, _state_bytes: &[u8]) {}
    pub fn builtin_state_bytes(_plugin_id: &str, _insert_id: &str) -> Option<Vec<u8>> {
        None
    }
    pub fn builtin_state_replay(_plugin_id: &str, _insert_id: &str) -> Vec<(u32, f32)> {
        Vec::new()
    }
    pub fn builtin_state_remove(_insert_id: &str) {}
    pub fn builtin_state_clear() {}
}

#[cfg(not(feature = "builtin-plugin-editor"))]
pub use state_mirror_stubs::{
    builtin_state_apply, builtin_state_bytes, builtin_state_clear, builtin_state_remove,
    builtin_state_replay, builtin_state_seed,
};

/// Physical-pixel rect the editor view occupies inside its parent window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ViewRect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// Whether editors are hosted **off-screen** on this platform.
///
/// Windows parents a real CEF child window into a `WS_CHILD` content host (see
/// `plugin_content_host.rs`). Nothing equivalent exists on Linux: GPUI owns an
/// X11/Wayland surface it composites itself, CEF's X11 child cannot be
/// reparented into it, and under Wayland there is no window id to parent to at
/// all. There the browser renders windowless and the GPUI window draws the
/// framebuffer itself, forwarding input back in.
pub const OFFSCREEN_HOSTING: bool = !cfg!(target_os = "windows");

/// Modifier state accompanying a forwarded input event.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EditorModifiers {
    pub shift: bool,
    pub control: bool,
    pub alt: bool,
    pub command: bool,
    pub left_button: bool,
    pub middle_button: bool,
    pub right_button: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorMouseButton {
    Left,
    Middle,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorKeyKind {
    Down,
    Up,
    /// A typed character. `character` carries the UTF-16 code unit; the key
    /// code is ignored.
    Char,
}

/// `windows_key_code` is Chromium's platform-independent `VKEY_*` value (the
/// same numbering as Win32 `VK_*`), which CEF expects on every OS.
#[derive(Debug, Clone, Copy)]
pub struct EditorKey {
    pub kind: EditorKeyKind,
    pub windows_key_code: i32,
    pub character: u16,
    pub modifiers: EditorModifiers,
}

/// One input event forwarded from the GPUI shell into an off-screen browser.
/// Coordinates are **logical** pixels relative to the view's top-left corner —
/// the same space CEF lays the page out in.
#[derive(Debug, Clone, Copy)]
pub enum EditorInput {
    MouseMove {
        x: i32,
        y: i32,
        modifiers: EditorModifiers,
        leaving: bool,
    },
    MouseButton {
        x: i32,
        y: i32,
        button: EditorMouseButton,
        pressed: bool,
        click_count: i32,
        modifiers: EditorModifiers,
    },
    MouseWheel {
        x: i32,
        y: i32,
        delta_x: i32,
        delta_y: i32,
        modifiers: EditorModifiers,
    },
    Key(EditorKey),
    Focus(bool),
}

/// Process-unique identity for one concrete editor window.
///
/// The logical editor id (`track::insert`) can be reused after a window closes;
/// native host commands must not be, otherwise a delayed close from the old
/// window can tear down the newly opened browser.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ViewId(u64);

pub fn allocate_view_id() -> ViewId {
    static NEXT_VIEW_ID: AtomicU64 = AtomicU64::new(1);
    ViewId(NEXT_VIEW_ID.fetch_add(1, Ordering::Relaxed))
}

/// Completion events produced by the serialized CEF command processor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewEvent {
    Opened,
    OpenFailed(String),
    Closed,
    /// The renderer process for this browser terminated (crash, OOM-kill,
    /// `chrome://kill` in dev). The browser object and native DSP state on
    /// the Rust side are untouched — only the page's JS state is gone, so
    /// the window stays open and the browser reloads its own URL; once
    /// `bridgeReady` arrives again the current selection is re-sent (see
    /// `BuiltinPluginEditorWindow::push_selected_instance`, gated on
    /// `browser_ready`).
    RendererCrashed,
}

#[cfg(not(feature = "builtin-plugin-editor"))]
mod imp {
    use super::{EditorInput, HostAvailability, ViewEvent, ViewId, ViewRect};

    pub fn availability(_plugin_id: &str) -> HostAvailability {
        HostAvailability::NotCompiledIn
    }

    pub fn pump() {}

    pub fn open_view(
        _view_id: ViewId,
        _editor_id: &str,
        _plugin_id: &str,
        _parent_hwnd: u64,
        _rect: ViewRect,
        _scale_factor: f32,
    ) -> Result<(), HostAvailability> {
        Err(HostAvailability::NotCompiledIn)
    }

    pub fn view_frame_generation(_view_id: ViewId) -> u64 {
        0
    }

    pub fn with_view_frame<R>(
        _view_id: ViewId,
        _read: impl FnOnce(&[u8], i32, i32) -> R,
    ) -> Option<R> {
        None
    }

    pub fn send_view_input(_view_id: ViewId, _input: EditorInput) {}

    pub fn init_at_boot() -> Result<(), HostAvailability> {
        Err(HostAvailability::NotCompiledIn)
    }

    pub fn preload() {}

    pub fn set_view_bounds(_view_id: ViewId, _rect: ViewRect, _scale_factor: f32) {}

    pub fn close_view(_view_id: ViewId) {}

    pub fn take_view_events(_view_id: ViewId) -> Vec<ViewEvent> {
        Vec::new()
    }

    pub fn is_view_open(_view_id: ViewId) -> bool {
        false
    }

    pub fn take_inbound(_origin: &str) -> Vec<Vec<u8>> {
        Vec::new()
    }

    pub fn send_to_view(_view_id: ViewId, _code: &str) {}

    pub fn reload_view(_view_id: ViewId) {}

    pub fn take_global_play_pause_requests(_view_id: ViewId) -> u32 {
        0
    }
}

#[cfg(feature = "builtin-plugin-editor")]
mod imp {
    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, OnceLock};

    use sphere_webview::client::{plugin_browser_client_with_surface, BrowserLifecycle};
    use sphere_webview::osr::{
        OsrInput, OsrKey, OsrKeyKind, OsrModifiers, OsrMouseButton, OsrSurface,
    };
    use sphere_webview::runtime::cef::rc::Rc as _;
    use sphere_webview::runtime::{
        CefRuntime, CefRuntimeConfig, NativeParent, WebView, WebViewConfig, WindowBounds,
    };
    use sphere_webview::scheme::{register_plugin_scheme_factory, BridgeSink, SchemeAsset};

    use super::{
        origin_for_plugin_id, EditorInput, EditorKey, EditorKeyKind, EditorModifiers,
        EditorMouseButton, HostAvailability, ViewEvent, ViewId, ViewRect, OFFSCREEN_HOSTING,
    };

    struct HostedView {
        _editor_id: String,
        // Drop the browser before the explicitly retained client.
        view: WebView<'static>,
        _client: sphere_webview::runtime::cef::Client,
        lifecycle: BrowserLifecycle,
        opened_at: std::time::Instant,
        stability_reported: bool,
    }

    struct ClosingView {
        hosted: HostedView,
        pump_ticks: u16,
    }

    const MAX_CLOSE_PUMP_TICKS: u16 = 250;

    /// Boot-time warm-up browser (see [`preload`]).
    ///
    /// Kept alive for the whole session: it pins Chromium's helper processes
    /// (GPU, network service, a renderer) so every editor open — not just the
    /// first — skips the multi-hundred-millisecond subprocess spawn.
    struct WarmupBrowser {
        // Drop the browser before the explicitly retained client.
        _view: WebView<'static>,
        _client: sphere_webview::runtime::cef::Client,
        _lifecycle: BrowserLifecycle,
        /// Hidden native parent for the windowed (Windows) case; destroyed
        /// after the browser it hosts.
        hidden_parent: u64,
    }

    impl Drop for WarmupBrowser {
        fn drop(&mut self) {
            #[cfg(target_os = "windows")]
            if self.hidden_parent != 0 {
                use windows::Win32::Foundation::HWND;
                use windows::Win32::UI::WindowsAndMessaging::{DestroyWindow, IsWindow};
                unsafe {
                    let hwnd = HWND(self.hidden_parent as *mut core::ffi::c_void);
                    if IsWindow(Some(hwnd)).as_bool() {
                        let _ = DestroyWindow(hwnd);
                    }
                }
            }
            let _ = self.hidden_parent;
        }
    }

    /// One CEF runtime plus every open editor view.
    ///
    /// Field order is load-bearing: Rust drops fields in declaration order, and
    /// the detached views must be released before the runtime that created them
    /// (see `CefRuntime::create_webview_detached`).
    struct Host {
        views: HashMap<ViewId, HostedView>,
        closing_views: HashMap<ViewId, ClosingView>,
        warmup: Option<WarmupBrowser>,
        runtime: CefRuntime,
        // The exact CefApp passed to execute_process in the browser process.
        // Keep it alive until after the runtime shuts down.
        _application: sphere_webview::runtime::cef::App,
    }

    struct PendingOpen {
        editor_id: String,
        origin: &'static str,
        parent_hwnd: u64,
        rect: ViewRect,
    }

    /// Physical rect plus the scale it was measured at. Off-screen browsers are
    /// told a *logical* size and render at `scale`; a windowed child ignores the
    /// scale and uses the physical rect directly.
    #[derive(Debug, Clone, Copy)]
    struct PendingBounds {
        rect: ViewRect,
        scale_factor: f32,
    }

    #[derive(Default)]
    struct PendingViewCommands {
        open: Option<PendingOpen>,
        bounds: Option<PendingBounds>,
        close: bool,
    }

    thread_local! {
        /// Browser-process CEF state, owned by the UI thread because
        /// `CefRuntime` is `!Send`.
        static HOST: RefCell<Option<Host>> = const { RefCell::new(None) };
        /// Browser-process app transferred from the executable after
        /// `execute_process` returns the browser-process sentinel.
        static PROCESS_APP: RefCell<Option<sphere_webview::runtime::cef::App>> = const { RefCell::new(None) };
        /// CEF initialization is process-global and must not be retried after a
        /// partial failure.
        static RUNTIME_FAILURE: RefCell<Option<String>> = const { RefCell::new(None) };
        /// Native operations are queued separately from `HOST`. GPUI/Win32 can
        /// re-enter while CEF is working; nested callbacks may enqueue commands,
        /// but must never try to borrow or call CEF synchronously.
        static COMMANDS: RefCell<HashMap<ViewId, PendingViewCommands>> = RefCell::new(HashMap::new());
        static EVENTS: RefCell<HashMap<ViewId, Vec<ViewEvent>>> = RefCell::new(HashMap::new());
        static PUMPING: Cell<bool> = const { Cell::new(false) };
        /// Editor windows tick independently, but CEF owns one process-wide
        /// message loop. Coalesce near-simultaneous calls so N open plugin
        /// types do not pump Chromium N times and starve GPUI input handling.
        static LAST_CEF_PUMP: Cell<Option<std::time::Instant>> = const { Cell::new(None) };
    }

    struct PumpGuard;

    impl PumpGuard {
        fn enter() -> Option<Self> {
            PUMPING.with(|pumping| {
                if pumping.replace(true) {
                    None
                } else {
                    Some(Self)
                }
            })
        }
    }

    impl Drop for PumpGuard {
        fn drop(&mut self) {
            PUMPING.with(|pumping| pumping.set(false));
        }
    }

    /// Resolve `mikoplugin://<origin>/<path>` to embedded bytes.
    ///
    /// This is the isolation boundary: an origin that is not a known built-in
    /// returns `None`, so one plugin's editor can never read another's assets.
    /// Runs on CEF's IO thread — it only indexes static tables.
    fn resolve_asset(origin: &str, path: &str) -> Option<SchemeAsset> {
        use builtin_ui_embed::EmbeddedPluginUi;
        let asset = match origin {
            rodharerist::ui::UI_ORIGIN => rodharerist::ui::RodhareistUi::resolve_ui_asset(path)?,
            equz8::ui::UI_ORIGIN => equz8::ui::Equz8Ui::resolve_ui_asset(path)?,
            verbspace::ui::UI_ORIGIN => verbspace::ui::VerbspaceUi::resolve_ui_asset(path)?,
            echospace::ui::UI_ORIGIN => echospace::ui::EchospaceUi::resolve_ui_asset(path)?,
            fa2a::ui::UI_ORIGIN => fa2a::ui::Fa2aUi::resolve_ui_asset(path)?,
            fa76::ui::UI_ORIGIN => fa76::ui::Fa76Ui::resolve_ui_asset(path)?,
            burnlimit::ui::UI_ORIGIN => burnlimit::ui::BurnLimitUi::resolve_ui_asset(path)?,
            clipper67::ui::UI_ORIGIN => clipper67::ui::Clipper67Ui::resolve_ui_asset(path)?,
            transient::ui::UI_ORIGIN => transient::ui::TransientUi::resolve_ui_asset(path)?,
            wrapsynth::ui::UI_ORIGIN => wrapsynth::ui::WrapSynthUi::resolve_ui_asset(path)?,
            zcomp::ui::UI_ORIGIN => zcomp::ui::ZcompUi::resolve_ui_asset(path)?,
            mixstation::ui::UI_ORIGIN => mixstation::ui::MixStationUi::resolve_ui_asset(path)?,
            _ => return None,
        };
        Some(SchemeAsset {
            bytes: asset.bytes,
            mime_type: asset.mime_type,
        })
    }

    /// Whether this build links a built-in's editor at all. Distinct from
    /// [`has_embedded_ui`]: an origin can be hosted here yet carry an empty
    /// asset table when its bundle was never built, and the two cases get
    /// different `HostAvailability` errors.
    fn hosts_editor(origin: &str) -> bool {
        matches!(
            origin,
            rodharerist::ui::UI_ORIGIN
                | equz8::ui::UI_ORIGIN
                | verbspace::ui::UI_ORIGIN
                | echospace::ui::UI_ORIGIN
                | fa2a::ui::UI_ORIGIN
                | fa76::ui::UI_ORIGIN
                | burnlimit::ui::UI_ORIGIN
                | clipper67::ui::UI_ORIGIN
                | transient::ui::UI_ORIGIN
                | wrapsynth::ui::UI_ORIGIN
                | zcomp::ui::UI_ORIGIN
                | mixstation::ui::UI_ORIGIN
        )
    }

    /// Whether a built-in plugin has embedded editor assets to serve.
    fn has_embedded_ui(origin: &str) -> bool {
        match origin {
            rodharerist::ui::UI_ORIGIN => rodharerist::ui::RodhareistUi::is_embedded(),
            equz8::ui::UI_ORIGIN => equz8::ui::Equz8Ui::is_embedded(),
            verbspace::ui::UI_ORIGIN => verbspace::ui::VerbspaceUi::is_embedded(),
            echospace::ui::UI_ORIGIN => echospace::ui::EchospaceUi::is_embedded(),
            fa2a::ui::UI_ORIGIN => fa2a::ui::Fa2aUi::is_embedded(),
            fa76::ui::UI_ORIGIN => fa76::ui::Fa76Ui::is_embedded(),
            burnlimit::ui::UI_ORIGIN => burnlimit::ui::BurnLimitUi::is_embedded(),
            clipper67::ui::UI_ORIGIN => clipper67::ui::Clipper67Ui::is_embedded(),
            transient::ui::UI_ORIGIN => transient::ui::TransientUi::is_embedded(),
            wrapsynth::ui::UI_ORIGIN => wrapsynth::ui::WrapSynthUi::is_embedded(),
            zcomp::ui::UI_ORIGIN => zcomp::ui::ZcompUi::is_embedded(),
            mixstation::ui::UI_ORIGIN => mixstation::ui::MixStationUi::is_embedded(),
            _ => false,
        }
    }

    /// React->native bridge inbound queue, keyed by scheme origin (the same
    /// stem `resolve_asset` matches on, e.g. `"rodharerist"`). Filled from
    /// CEF's IO thread (`bridge_sink`, below) — process-wide `Mutex`, not a
    /// UI-thread `thread_local!` like `HOST`/`COMMANDS`, since the scheme
    /// factory callback does not run on the UI thread. Drained by
    /// `take_inbound`, called from the GPUI pump tick (non-realtime).
    static INBOUND: OnceLock<Mutex<HashMap<String, Vec<Vec<u8>>>>> = OnceLock::new();

    fn inbound_map() -> &'static Mutex<HashMap<String, Vec<Vec<u8>>>> {
        INBOUND.get_or_init(|| Mutex::new(HashMap::new()))
    }

    fn bridge_sink() -> BridgeSink {
        Arc::new(|origin: &str, body: Vec<u8>| {
            if let Ok(mut map) = inbound_map().lock() {
                map.entry(origin.to_string()).or_default().push(body);
            }
        })
    }

    /// Drain every bridge message POSTed by `origin`'s page since the last
    /// call. Never blocks on CEF — just takes whatever `bridge_sink` queued.
    pub fn take_inbound(origin: &str) -> Vec<Vec<u8>> {
        inbound_map()
            .lock()
            .ok()
            .and_then(|mut map| map.remove(origin))
            .unwrap_or_default()
    }

    /// Run `code` in `view_id`'s document. No-op (not an error) if the view
    /// isn't open yet or already closed — callers already gate on
    /// `is_view_open`/`ViewEvent::Opened` where it matters.
    pub fn send_to_view(view_id: ViewId, code: &str) {
        HOST.with(|cell| {
            if let Ok(slot) = cell.try_borrow() {
                if let Some(host) = slot.as_ref() {
                    if let Some(hosted) = host.views.get(&view_id) {
                        if let Err(error) = hosted.view.execute_javascript(code) {
                            eprintln!(
                                "[plugin-bridge] execute_javascript failed view_id={view_id:?} err={error}"
                            );
                        }
                    }
                }
            }
        });
    }

    /// Reload `view_id`'s document. Used by the bridge-ready watchdog to
    /// recover a page whose load silently died (e.g. Chromium's network
    /// service crashed mid-transfer, which it auto-restarts for *future*
    /// requests but does not retry the one already in flight, so a page can
    /// finish HTTP-200 headers and still never actually paint).
    pub fn reload_view(view_id: ViewId) {
        HOST.with(|cell| {
            if let Ok(slot) = cell.try_borrow() {
                if let Some(host) = slot.as_ref() {
                    if let Some(hosted) = host.views.get(&view_id) {
                        if let Err(error) = hosted.view.reload() {
                            eprintln!(
                                "[plugin-bridge] reload failed view_id={view_id:?} err={error}"
                            );
                        }
                    }
                }
            }
        });
    }

    /// Drain global transport shortcuts captured by this view's native CEF
    /// keyboard handler.
    pub fn take_global_play_pause_requests(view_id: ViewId) -> u32 {
        HOST.with(|cell| {
            cell.try_borrow()
                .ok()
                .and_then(|slot| {
                    slot.as_ref()?
                        .views
                        .get(&view_id)
                        .map(|hosted| hosted.lifecycle.take_global_play_pause_requests())
                })
                .unwrap_or(0)
        })
    }

    pub fn availability(plugin_id: &str) -> HostAvailability {
        let Some(origin) = origin_for_plugin_id(plugin_id) else {
            return HostAvailability::NoEditorForPlugin(plugin_id.to_string());
        };
        if !hosts_editor(origin) {
            return HostAvailability::NoEditorForPlugin(plugin_id.to_string());
        }
        if !has_embedded_ui(origin) {
            return HostAvailability::UiNotEmbedded(plugin_id.to_string());
        }
        HostAvailability::Ready
    }

    /// Transfer ownership of the browser process's `CefApp` into the UI-thread
    /// host. CEF requires this exact object for both `execute_process` and
    /// `initialize`.
    pub fn install_process_app(
        app: sphere_webview::runtime::cef::App,
    ) -> Result<(), HostAvailability> {
        PROCESS_APP.with(|cell| {
            let mut slot = cell.borrow_mut();
            if slot.is_some() || HOST.with(|host| host.borrow().is_some()) {
                return Err(HostAvailability::RuntimeFailed(
                    "CEF process application was installed more than once".to_owned(),
                ));
            }
            *slot = Some(app);
            Ok(())
        })
    }

    /// Start CEF if it is not already running. Idempotent.
    fn ensure_runtime(slot: &mut Option<Host>) -> Result<(), HostAvailability> {
        if slot.is_some() {
            return Ok(());
        }
        if let Some(error) = RUNTIME_FAILURE.with(|failure| failure.borrow().clone()) {
            return Err(HostAvailability::RuntimeFailed(error));
        }

        let mut app = PROCESS_APP
            .with(|cell| cell.borrow_mut().take())
            .ok_or_else(|| {
                HostAvailability::RuntimeFailed(
                    "CEF process application was not installed before UI startup".to_owned(),
                )
            })?;
        let config = CefRuntimeConfig {
            cache_path: cef_cache_dir(),
            remote_debugging_port: debug_port(),
            browser_subprocess: match sphere_webview::runtime::platform_browser_subprocess() {
                Ok(subprocess) => subprocess,
                Err(error) => return remember_runtime_failure(error.to_string()),
            },
            // Chromium decides this once, at initialize, so the whole process
            // opts in wherever editors are hosted off-screen.
            windowless_rendering: OFFSCREEN_HOSTING,
            ..Default::default()
        };
        let runtime = match CefRuntime::initialize(config, Some(&mut app)) {
            Ok(runtime) => runtime,
            Err(error) => return remember_runtime_failure(error.to_string()),
        };

        // The factory can only be installed once initialize has succeeded.
        let resolver: sphere_webview::scheme::SchemeResolver =
            Arc::new(|origin: &str, path: &str| resolve_asset(origin, path));
        if let Err(error) = register_plugin_scheme_factory(resolver, Some(bridge_sink())) {
            return remember_runtime_failure(error.to_string());
        }

        *slot = Some(Host {
            views: HashMap::new(),
            closing_views: HashMap::new(),
            warmup: None,
            runtime,
            _application: app,
        });
        Ok(())
    }

    fn remember_runtime_failure(error: String) -> Result<(), HostAvailability> {
        RUNTIME_FAILURE.with(|failure| {
            *failure.borrow_mut() = Some(error.clone());
        });
        Err(HostAvailability::RuntimeFailed(error))
    }

    /// Hidden 2×2 native window to parent the warm-up browser to on windowed
    /// platforms. `0` where off-screen hosting needs no parent.
    #[cfg(target_os = "windows")]
    fn create_hidden_warmup_parent() -> u64 {
        use windows::core::w;
        use windows::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, HMENU, WINDOW_EX_STYLE, WS_POPUP,
        };
        // The predefined STATIC class is fine here: the window is never shown,
        // it exists only so CEF has a real HWND to create its child under.
        unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("STATIC"),
                w!("futureboard-cef-warmup"),
                WS_POPUP, // not WS_VISIBLE — never shown
                0,
                0,
                2,
                2,
                None,
                None::<HMENU>,
                None,
                None,
            )
            .map(|hwnd| hwnd.0 as u64)
            .unwrap_or(0)
        }
    }

    #[cfg(not(target_os = "windows"))]
    fn create_hidden_warmup_parent() -> u64 {
        0
    }

    /// Create the boot-time warm-up browser (idempotent).
    ///
    /// The first `CreateBrowserSync` of a session pays for spawning Chromium's
    /// helper processes (GPU, network service, renderer) and initializing the
    /// profile — several hundred milliseconds that used to land inside the
    /// first editor open. Creating a hidden `about:blank` browser during boot
    /// moves that cost behind the loading screen.
    ///
    /// `about:blank` is deliberate: preloading the real editor URL would run
    /// its page JS, whose `bridgeReady` POST lands in the origin-keyed
    /// [`INBOUND`] queue and would be misread as the real editor's handshake
    /// when one opens later.
    fn ensure_warmup(host: &mut Host) {
        ensure_warmup_supported(host);
    }

    fn ensure_warmup_supported(host: &mut Host) {
        if host.warmup.is_some() {
            return;
        }
        let url = "about:blank".to_string();
        let surface = OFFSCREEN_HOSTING.then(|| OsrSurface::new(2, 2, 1.0));
        let hidden_parent = if OFFSCREEN_HOSTING {
            0
        } else {
            let hwnd = create_hidden_warmup_parent();
            if hwnd == 0 {
                eprintln!("[cef-warmup] hidden parent creation failed; skipping warm-up");
                return;
            }
            hwnd
        };
        let (mut client, lifecycle) = plugin_browser_client_with_surface(&url, surface.clone());
        let result = WindowBounds::new(0, 0, 2, 2)
            .map_err(|error| error.to_string())
            .and_then(|bounds| {
                let mut config = WebViewConfig::new(url, bounds);
                if let Some(surface) = surface {
                    config = config.windowless(surface);
                }
                // SAFETY: the warm-up view is stored in `host.warmup`,
                // declared before `host.runtime`, and therefore released
                // first.
                unsafe {
                    let parent = NativeParent::from_raw(hwnd_to_cef(hidden_parent));
                    host.runtime
                        .create_webview_detached(parent, config, Some(&mut client))
                }
                .map_err(|error| error.to_string())
            });
        match result {
            Ok(view) => {
                eprintln!(
                    "[cef-warmup] warm-up browser created browser_id={}",
                    view.browser_identifier()
                );
                host.warmup = Some(WarmupBrowser {
                    _view: view,
                    _client: client,
                    _lifecycle: lifecycle,
                    hidden_parent,
                });
            }
            Err(error) => {
                eprintln!("[cef-warmup] warm-up browser failed: {error}");
                #[cfg(target_os = "windows")]
                if hidden_parent != 0 {
                    use windows::Win32::Foundation::HWND;
                    use windows::Win32::UI::WindowsAndMessaging::DestroyWindow;
                    unsafe {
                        let _ = DestroyWindow(HWND(hidden_parent as *mut core::ffi::c_void));
                    }
                }
            }
        }
    }

    /// Boot-time preload: start CEF *and* spawn the warm-up browser so the
    /// first editor open only pays for its own page. Idempotent; failure is
    /// non-fatal (editors fall back to cold opens).
    ///
    /// Call from the UI thread, then drive [`pump`] for a couple of seconds
    /// (the boot pump loop) so the warm-up finishes spawning Chromium's helper
    /// processes while the loading screen is still up — with no editor window
    /// open nothing else pumps CEF.
    pub fn preload() {
        HOST.with(|cell| {
            let mut slot = cell.borrow_mut();
            if ensure_runtime(&mut slot).is_ok() {
                if let Some(host) = slot.as_mut() {
                    ensure_warmup(host);
                }
            }
        });
    }

    /// Close every CEF browser and finish process-global shutdown on the UI
    /// thread after GPUI's application loop exits.
    pub fn shutdown() {
        HOST.with(|cell| {
            let Some(host) = cell.borrow_mut().take() else {
                return;
            };
            let open_count =
                host.views.len() + host.closing_views.len() + usize::from(host.warmup.is_some());
            eprintln!("[cef-runtime] shutdown requested open_browser_count={open_count}");
            for hosted in host.views.values() {
                let _ = hosted.view.close(true);
            }
            for closing in host.closing_views.values() {
                let _ = closing.hosted.view.close(true);
            }
            if let Some(warmup) = host.warmup.as_ref() {
                let _ = warmup._view.close(true);
            }

            let mut remaining = open_count;
            for _ in 0..MAX_CLOSE_PUMP_TICKS {
                let _ = host.runtime.do_message_loop_work();
                remaining = host
                    .views
                    .values()
                    .filter(|hosted| !hosted.lifecycle.before_close())
                    .count()
                    + host
                        .closing_views
                        .values()
                        .filter(|closing| !closing.hosted.lifecycle.before_close())
                        .count()
                    + host
                        .warmup
                        .as_ref()
                        .filter(|warmup| !warmup._lifecycle.before_close())
                        .map_or(0, |_| 1);
                if remaining == 0 {
                    break;
                }
            }
            eprintln!("[cef-runtime] shutdown close_pump_complete remaining_open={remaining}");
            // Field order now releases all browser/client handles before
            // `CefRuntime::drop` invokes cef_shutdown exactly once.
            drop(host);
        });
    }

    /// Start CEF during application boot, on the UI thread.
    ///
    /// Initialization spawns Chromium's helper processes and takes on the order
    /// of a few hundred milliseconds. Doing it lazily on first editor open means
    /// paying that cost inside a render pass, which stalls the UI thread and
    /// delays the first paint of the editor window. Doing it at boot moves the
    /// cost into startup, where there is already a loading screen.
    ///
    /// The thread that calls this is the thread that must later drive
    /// [`pump`] and create every view — `CefRuntime` enforces that.
    ///
    /// Failure is not fatal: the editor route falls back to reporting the error
    /// in its window rather than bringing down the app.
    pub fn init_at_boot() -> Result<(), HostAvailability> {
        HOST.with(|cell| {
            let mut slot = cell.borrow_mut();
            ensure_runtime(&mut slot)
        })
    }

    /// Queue creation of the editor browser for `plugin_id` as a child of
    /// `parent_hwnd`.
    ///
    /// No CEF API is called here. This function is intentionally safe to invoke
    /// from a GPUI render/update: [`pump`] executes the native operation later,
    /// after GPUI has released its `AppCell` and entity borrows.
    pub fn open_view(
        view_id: ViewId,
        editor_id: &str,
        plugin_id: &str,
        parent_hwnd: u64,
        rect: ViewRect,
        scale_factor: f32,
    ) -> Result<(), HostAvailability> {
        match super::availability(plugin_id) {
            HostAvailability::Ready => {}
            other => return Err(other),
        }
        let Some(origin) = origin_for_plugin_id(plugin_id) else {
            return Err(HostAvailability::NoEditorForPlugin(plugin_id.to_string()));
        };
        // An off-screen browser has no parent window to be a child of; CEF only
        // uses the handle to resolve monitor info, and accepts none.
        if parent_hwnd == 0 && !OFFSCREEN_HOSTING {
            return Err(HostAvailability::RuntimeFailed(
                "editor window has no native parent handle yet".to_string(),
            ));
        }
        WindowBounds::new(rect.x, rect.y, rect.width, rect.height)
            .map_err(|e| HostAvailability::RuntimeFailed(e.to_string()))?;

        COMMANDS.with(|commands| {
            let mut commands = commands.borrow_mut();
            let pending = commands.entry(view_id).or_default();
            if pending.close {
                return Err(HostAvailability::RuntimeFailed(
                    "editor view is already closing".to_string(),
                ));
            }
            pending.open = Some(PendingOpen {
                editor_id: editor_id.to_string(),
                origin,
                parent_hwnd,
                rect,
            });
            pending.bounds = Some(PendingBounds { rect, scale_factor });
            Ok(())
        })
    }

    /// Coalesce a browser resize for the next pump. An unknown id is allowed:
    /// the latest bounds are retained while its open command is still pending.
    pub fn set_view_bounds(view_id: ViewId, rect: ViewRect, scale_factor: f32) {
        if rect.width <= 0 || rect.height <= 0 {
            return;
        }
        COMMANDS.with(|commands| {
            let mut commands = commands.borrow_mut();
            let pending = commands.entry(view_id).or_default();
            if !pending.close {
                pending.bounds = Some(PendingBounds { rect, scale_factor });
            }
        });
    }

    /// Logical size to hand an off-screen browser for a physical rect measured
    /// at `scale_factor`. Clamped to at least one pixel: a zero-sized view rect
    /// makes Chromium drop the browser's compositor frame entirely.
    fn logical_size(rect: ViewRect, scale_factor: f32) -> (i32, i32) {
        let scale = if scale_factor > 0.0 {
            scale_factor
        } else {
            1.0
        };
        (
            ((rect.width as f32) / scale).round().max(1.0) as i32,
            ((rect.height as f32) / scale).round().max(1.0) as i32,
        )
    }

    /// Frame counter for `view_id`'s off-screen surface. `0` while the browser
    /// is windowed, absent, or has not painted yet.
    pub fn view_frame_generation(view_id: ViewId) -> u64 {
        HOST.with(|cell| {
            cell.try_borrow()
                .ok()
                .and_then(|slot| {
                    let host = slot.as_ref()?;
                    let hosted = host.views.get(&view_id)?;
                    Some(hosted.view.osr_surface()?.generation())
                })
                .unwrap_or(0)
        })
    }

    /// Read `view_id`'s latest off-screen frame (BGRA bytes, physical width,
    /// physical height). `None` for a windowed browser or before first paint.
    pub fn with_view_frame<R>(
        view_id: ViewId,
        read: impl FnOnce(&[u8], i32, i32) -> R,
    ) -> Option<R> {
        HOST.with(|cell| {
            let slot = cell.try_borrow().ok()?;
            let host = slot.as_ref()?;
            let hosted = host.views.get(&view_id)?;
            hosted.view.osr_surface()?.with_frame(read)
        })
    }

    /// Forward one input event to an off-screen browser. Silently ignored for
    /// a windowed browser, which receives real platform input directly.
    pub fn send_view_input(view_id: ViewId, input: EditorInput) {
        HOST.with(|cell| {
            let Ok(slot) = cell.try_borrow() else { return };
            let Some(host) = slot.as_ref() else { return };
            let Some(hosted) = host.views.get(&view_id) else {
                return;
            };
            if hosted.view.osr_surface().is_none() {
                return;
            }
            if let Err(error) = hosted.view.send_input(to_osr_input(input)) {
                eprintln!("[plugin-bridge] send_input failed view_id={view_id:?} err={error}");
            }
        });
    }

    fn to_osr_modifiers(modifiers: EditorModifiers) -> OsrModifiers {
        OsrModifiers {
            shift: modifiers.shift,
            control: modifiers.control,
            alt: modifiers.alt,
            command: modifiers.command,
            left_button: modifiers.left_button,
            middle_button: modifiers.middle_button,
            right_button: modifiers.right_button,
        }
    }

    fn to_osr_key(key: EditorKey) -> OsrKey {
        OsrKey {
            kind: match key.kind {
                EditorKeyKind::Down => OsrKeyKind::Down,
                EditorKeyKind::Up => OsrKeyKind::Up,
                EditorKeyKind::Char => OsrKeyKind::Char,
            },
            windows_key_code: key.windows_key_code,
            character: key.character,
            modifiers: to_osr_modifiers(key.modifiers),
        }
    }

    fn to_osr_input(input: EditorInput) -> OsrInput {
        match input {
            EditorInput::MouseMove {
                x,
                y,
                modifiers,
                leaving,
            } => OsrInput::MouseMove {
                x,
                y,
                modifiers: to_osr_modifiers(modifiers),
                leaving,
            },
            EditorInput::MouseButton {
                x,
                y,
                button,
                pressed,
                click_count,
                modifiers,
            } => OsrInput::MouseButton {
                x,
                y,
                button: match button {
                    EditorMouseButton::Left => OsrMouseButton::Left,
                    EditorMouseButton::Middle => OsrMouseButton::Middle,
                    EditorMouseButton::Right => OsrMouseButton::Right,
                },
                pressed,
                click_count,
                modifiers: to_osr_modifiers(modifiers),
            },
            EditorInput::MouseWheel {
                x,
                y,
                delta_x,
                delta_y,
                modifiers,
            } => OsrInput::MouseWheel {
                x,
                y,
                delta_x,
                delta_y,
                modifiers: to_osr_modifiers(modifiers),
            },
            EditorInput::Key(key) => OsrInput::Key(to_osr_key(key)),
            EditorInput::Focus(focused) => OsrInput::Focus(focused),
        }
    }

    /// Queue a close. Close dominates an unprocessed open/resize for this unique
    /// view id, so closing a shell before its first pump never creates a browser
    /// against a dead parent HWND.
    pub fn close_view(view_id: ViewId) {
        COMMANDS.with(|commands| {
            let mut commands = commands.borrow_mut();
            let pending = commands.entry(view_id).or_default();
            pending.open = None;
            pending.bounds = None;
            pending.close = true;
        });
    }

    pub fn take_view_events(view_id: ViewId) -> Vec<ViewEvent> {
        EVENTS.with(|events| events.borrow_mut().remove(&view_id).unwrap_or_default())
    }

    pub fn is_view_open(view_id: ViewId) -> bool {
        let pending_open = COMMANDS.with(|commands| {
            commands
                .try_borrow()
                .ok()
                .and_then(|commands| {
                    commands
                        .get(&view_id)
                        .map(|pending| pending.open.is_some() && !pending.close)
                })
                .unwrap_or(false)
        });
        pending_open
            || HOST.with(|cell| {
                cell.try_borrow()
                    .ok()
                    .and_then(|slot| slot.as_ref().map(|host| host.views.contains_key(&view_id)))
                    .unwrap_or(false)
            })
    }

    /// Execute queued native operations and advance CEF's message loop.
    ///
    /// Call this only from a GPUI foreground task *outside* `AsyncApp::update`.
    /// CEF may synchronously dispatch Win32 messages; keeping every GPUI borrow
    /// out of this stack is what prevents `AppCell` double-borrow panics.
    pub fn pump() {
        let Some(_guard) = PumpGuard::enter() else {
            return;
        };
        const MIN_PUMP_INTERVAL: std::time::Duration = std::time::Duration::from_millis(8);
        let should_pump = LAST_CEF_PUMP.with(|last| {
            let now = std::time::Instant::now();
            if last
                .get()
                .is_some_and(|previous| now.duration_since(previous) < MIN_PUMP_INTERVAL)
            {
                false
            } else {
                last.set(Some(now));
                true
            }
        });
        if !should_pump {
            return;
        }
        let commands = COMMANDS.with(|commands| std::mem::take(&mut *commands.borrow_mut()));
        let mut completed = Vec::new();

        HOST.with(|cell| {
            let mut slot = cell.borrow_mut();
            if let Err(error) = ensure_runtime(&mut slot) {
                for (view_id, pending) in commands {
                    if pending.close {
                        completed.push((view_id, ViewEvent::Closed));
                    } else if pending.open.is_some() {
                        completed.push((view_id, ViewEvent::OpenFailed(error.to_string())));
                    }
                }
                return;
            }
            let host = slot.as_mut().expect("ensure_runtime installs the host");

            for (view_id, pending) in commands {
                if pending.close {
                    if let Some(hosted) = host.views.remove(&view_id) {
                        let browser_id = hosted.view.browser_identifier();
                        let _ = hosted.view.close(true);
                        host.closing_views.insert(
                            view_id,
                            ClosingView {
                                hosted,
                                pump_ticks: 0,
                            },
                        );
                        eprintln!(
                            "[cef-registry] event=close_requested view_id={view_id:?} browser_id={browser_id} editor_count={} removal_deferred_until=OnBeforeClose",
                            host.views.len() + host.closing_views.len()
                        );
                    } else if !host.closing_views.contains_key(&view_id) {
                        // A close that canceled an unprocessed open has no native
                        // browser lifetime to wait for.
                        completed.push((view_id, ViewEvent::Closed));
                    }
                    continue;
                }

                let mut opened_now = false;
                if let Some(open) = pending.open {
                    if host.views.contains_key(&view_id) {
                        completed.push((view_id, ViewEvent::Opened));
                    } else {
                        let bounds_command = pending.bounds.unwrap_or(PendingBounds {
                            rect: open.rect,
                            scale_factor: 1.0,
                        });
                        let rect = bounds_command.rect;
                        let url = diagnostic_control_url().unwrap_or_else(|| {
                            format!("mikoplugin://{}/index.html", open.origin)
                        });
                        // Off-screen: CEF lays out in logical pixels and paints
                        // physical ones into the surface the client owns.
                        let surface = OFFSCREEN_HOSTING.then(|| {
                            let (width, height) =
                                logical_size(rect, bounds_command.scale_factor);
                            OsrSurface::new(width, height, bounds_command.scale_factor)
                        });
                        let (mut client, lifecycle) =
                            plugin_browser_client_with_surface(&url, surface.clone());
                        let result = WindowBounds::new(rect.x, rect.y, rect.width, rect.height)
                            .map_err(|error| error.to_string())
                            .and_then(|bounds| {
                                let mut config = WebViewConfig::new(url, bounds);
                                if let Some(surface) = surface {
                                    config = config.windowless(surface);
                                }
                                // SAFETY: the returned view is stored in
                                // `host.views`, declared before `host.runtime`,
                                // and therefore released first.
                                unsafe {
                                    let parent =
                                        NativeParent::from_raw(hwnd_to_cef(open.parent_hwnd));
                                    host.runtime.create_webview_detached(
                                        parent,
                                        config,
                                        Some(&mut client),
                                    )
                                }
                                .map_err(|error| error.to_string())
                            });
                        eprintln!(
                            "[cef-ref] object_type=cef_client_t event=after_CreateBrowserSync has_one_ref={} has_at_least_one_ref={} thread={:?}",
                            client.has_one_ref(),
                            client.has_at_least_one_ref(),
                            std::thread::current().id()
                        );
                        match result {
                            Ok(view) if lifecycle.after_created() => {
                                if let Err(error) = view.set_zoom_level(0.0) {
                                    eprintln!(
                                        "[plugin-editor] failed to lock zoom view_id={view_id:?} err={error}"
                                    );
                                }
                                let browser_id = view.browser_identifier();
                                host.views.insert(
                                    view_id,
                                    HostedView {
                                        _editor_id: open.editor_id,
                                        view,
                                        _client: client,
                                        lifecycle,
                                        opened_at: std::time::Instant::now(),
                                        stability_reported: false,
                                    },
                                );
                                eprintln!(
                                    "[cef-registry] event=insert source=OnAfterCreated view_id={view_id:?} browser_id={browser_id} editor_count={}",
                                    host.views.len() + host.closing_views.len()
                                );
                                opened_now = true;
                                completed.push((view_id, ViewEvent::Opened));
                            }
                            Ok(view) => {
                                let browser_id = view.browser_identifier();
                                let _ = view.close(true);
                                completed.push((
                                    view_id,
                                    ViewEvent::OpenFailed(format!(
                                        "CreateBrowserSync returned browser {browser_id} before OnAfterCreated"
                                    )),
                                ));
                            }
                            Err(error) => {
                                completed.push((view_id, ViewEvent::OpenFailed(error)));
                            }
                        }
                    }
                }

                if !opened_now {
                    if let Some(PendingBounds { rect, scale_factor }) = pending.bounds {
                        if let Some(hosted) = host.views.get(&view_id) {
                            // A windowed child is placed with the physical rect;
                            // an off-screen browser is told the logical size it
                            // should lay out at, and the scale it renders with.
                            let bounds = match hosted.view.osr_surface() {
                                Some(surface) => {
                                    let (width, height) = logical_size(rect, scale_factor);
                                    surface.set_view_size(width, height, scale_factor);
                                    WindowBounds::new(0, 0, width, height)
                                }
                                None => WindowBounds::new(rect.x, rect.y, rect.width, rect.height),
                            };
                            if let Ok(bounds) = bounds {
                                let _ = hosted.view.set_bounds(bounds);
                            }
                        }
                    }
                }
            }

            let _ = host.runtime.do_message_loop_work();

            // Renderer crash detection (`BrowserLifecycle`, set from
            // `on_render_process_terminated`). The browser object and native
            // DSP state survive a renderer crash — only the page's JS state
            // is gone — so this reloads the same URL rather than tearing the
            // window down; `ViewEvent::RendererCrashed` lets the GPUI window
            // reset `browser_ready` so it re-sends the current selection once
            // the fresh page announces `bridgeReady` again.
            let editor_count = host.views.len() + host.closing_views.len();
            for (view_id, hosted) in &mut host.views {
                if hosted.lifecycle.take_renderer_terminated() {
                    eprintln!("[plugin-scheme] reloading crashed renderer view_id={view_id:?}");
                    let _ = hosted.view.reload();
                    let _ = hosted.view.set_zoom_level(0.0);
                    completed.push((*view_id, ViewEvent::RendererCrashed));
                }
                if !hosted.stability_reported
                    && hosted.opened_at.elapsed() >= std::time::Duration::from_secs(60)
                {
                    hosted.stability_reported = true;
                    eprintln!(
                        "[cef-stability] browser_id={} view_id={view_id:?} elapsed_seconds=60 javascript_executed={} renderer_alive=true editor_count={}",
                        hosted.view.browser_identifier(),
                        hosted.lifecycle.javascript_executed(),
                        editor_count
                    );
                }
            }

            // `close_browser(true)` is asynchronous. Keep both the WebView and
            // the GPUI-owned parent shell alive until CEF's native child HWND is
            // actually gone; only then may the window consume `Closed` and tear
            // down its HWND hierarchy. The timeout is a bounded shutdown escape
            // hatch for a wedged renderer process.
            let mut closed = Vec::new();
            for (view_id, closing) in &mut host.closing_views {
                closing.pump_ticks = closing.pump_ticks.saturating_add(1);
                if closing.hosted.lifecycle.before_close() {
                    closed.push((*view_id, "OnBeforeClose"));
                } else if closing.pump_ticks >= MAX_CLOSE_PUMP_TICKS {
                    closed.push((*view_id, "timeout"));
                }
            }
            for (view_id, reason) in closed {
                let browser_id = host
                    .closing_views
                    .get(&view_id)
                    .map(|closing| closing.hosted.view.browser_identifier())
                    .unwrap_or(-1);
                host.closing_views.remove(&view_id);
                eprintln!(
                    "[cef-registry] event=remove source={reason} view_id={view_id:?} browser_id={browser_id} editor_count={}",
                    host.views.len() + host.closing_views.len()
                );
                completed.push((view_id, ViewEvent::Closed));
            }
        });

        if !completed.is_empty() {
            EVENTS.with(|events| {
                let mut events = events.borrow_mut();
                for (view_id, event) in completed {
                    events.entry(view_id).or_default().push(event);
                }
            });
        }
    }

    /// On Windows `cef_window_handle_t` is cef-dll-sys's own `HWND` newtype,
    /// which is distinct from the `windows` crate's `HWND` the rest of the app
    /// passes around as a `u64`.
    #[cfg(target_os = "windows")]
    fn hwnd_to_cef(handle: u64) -> sphere_webview::runtime::cef::sys::cef_window_handle_t {
        sphere_webview::runtime::cef::sys::HWND(handle as *mut _)
    }

    #[cfg(not(target_os = "windows"))]
    fn hwnd_to_cef(handle: u64) -> sphere_webview::runtime::cef::sys::cef_window_handle_t {
        handle as _
    }

    /// Per-user cache directory. CEF requires a writable path; an unwritable or
    /// missing one degrades to in-memory, which is acceptable for an editor UI.
    fn cef_cache_dir() -> Option<std::path::PathBuf> {
        let base = dirs::cache_dir()?;
        let dir = base.join("Futureboard").join("cef");
        std::fs::create_dir_all(&dir).ok()?;
        Some(dir)
    }

    /// Optional normal-page control. The exact URL is also whitelisted by the
    /// diagnostic client; absent this variable the plugin custom scheme is used.
    fn diagnostic_control_url() -> Option<String> {
        std::env::var("FUTUREBOARD_CEF_CONTROL_URL")
            .ok()
            .filter(|url| !url.trim().is_empty())
    }

    /// Opens Chromium's remote-debugging endpoint (`http://127.0.0.1:<port>`)
    /// so a real browser's devtools can inspect the editor's console/network
    /// when `FUTUREBOARD_PLUGIN_VIEW_DEBUG=1` — otherwise off, since it is a
    /// local unauthenticated debug surface.
    fn debug_port() -> Option<u16> {
        std::env::var_os("FUTUREBOARD_PLUGIN_VIEW_DEBUG").map(|_| 9222)
    }
}

#[cfg(feature = "builtin-plugin-editor")]
pub use imp::install_process_app;
#[cfg(feature = "builtin-plugin-editor")]
pub use imp::shutdown;
pub use imp::{
    availability, close_view, init_at_boot, is_view_open, open_view, preload, pump, reload_view,
    send_to_view, send_view_input, set_view_bounds, take_global_play_pause_requests, take_inbound,
    take_view_events, view_frame_generation, with_view_frame,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_ids_map_to_their_url_origin() {
        assert_eq!(
            origin_for_plugin_id("builtin:rodharerist"),
            Some("rodharerist")
        );
        assert_eq!(origin_for_plugin_id("builtin:equz8"), Some("equz8"));
        assert_eq!(origin_for_plugin_id("builtin:verbspace"), Some("verbspace"));
        assert_eq!(origin_for_plugin_id("builtin:echospace"), Some("echospace"));
        assert_eq!(origin_for_plugin_id("builtin:fa2a"), Some("fa2a"));
        assert_eq!(origin_for_plugin_id("builtin:fa76"), Some("fa76"));
        assert_eq!(origin_for_plugin_id("builtin:burnlimit"), Some("burnlimit"));
        assert_eq!(origin_for_plugin_id("builtin:clipper67"), Some("clipper67"));
        assert_eq!(origin_for_plugin_id("builtin:transient"), Some("transient"));
        assert_eq!(
            origin_for_plugin_id("builtin:mixstation"),
            Some("mixstation")
        );
    }

    /// Regression guard for the bug that left the editor unopenable in the real
    /// app: an insert slot stores the registry *class id* (`rodharerist`), not
    /// the catalog id (`builtin:rodharerist`), so both forms must resolve.
    #[test]
    fn the_class_id_form_stored_by_insert_slots_also_resolves() {
        assert_eq!(origin_for_plugin_id("rodharerist"), Some("rodharerist"));
        assert_eq!(origin_for_plugin_id("equz8"), Some("equz8"));
        assert_eq!(origin_for_plugin_id("verbspace"), Some("verbspace"));
        assert_eq!(origin_for_plugin_id("echospace"), Some("echospace"));
        assert_eq!(origin_for_plugin_id("fa2a"), Some("fa2a"));
        assert_eq!(origin_for_plugin_id("fa76"), Some("fa76"));
        assert_eq!(origin_for_plugin_id("burnlimit"), Some("burnlimit"));
        assert_eq!(origin_for_plugin_id("clipper67"), Some("clipper67"));
        assert_eq!(origin_for_plugin_id("transient"), Some("transient"));
        assert_eq!(origin_for_plugin_id("mixstation"), Some("mixstation"));
        assert_eq!(
            origin_for_plugin_id("rodharerist"),
            origin_for_plugin_id("builtin:rodharerist"),
            "both identifier forms must resolve to the same origin"
        );
    }

    /// Resolution is catalog-validated, not shape-based: an external plug-in
    /// whose class id merely lacks a prefix must not be mistaken for a built-in.
    #[test]
    fn unknown_unprefixed_ids_are_not_treated_as_built_ins() {
        assert_eq!(origin_for_plugin_id("SomeVst3ControllerClass"), None);
        assert_eq!(origin_for_plugin_id("builtin:not-a-real-plugin"), None);
    }

    /// Regression guard for the routing bug that made built-in editors do
    /// nothing: `open_insert_editor` must dispatch on the plugin id alone.
    ///
    /// Built-ins have no VST3 runtime instance, so their `runtime_state` sits at
    /// `NotLoaded` forever. The editor route therefore cannot depend on runtime
    /// state, load status, plugin path, or plugin format — only on the id. If
    /// this identification ever stops being self-contained, the built-in branch
    /// will fall through into the VST3 gate again and silently return.
    #[test]
    fn built_in_routing_depends_only_on_the_plugin_id() {
        // Both forms, since the editor route is reached from an insert slot.
        for id in [
            "builtin:rodharerist",
            "rodharerist",
            "builtin:equz8",
            "equz8",
            "builtin:mixstation",
            "mixstation",
        ] {
            assert!(
                SpherePluginHost::is_builtin_ref(id),
                "{id} must be routable without consulting runtime state"
            );
            assert!(origin_for_plugin_id(id).is_some());
        }
        for id in ["vst3:foo", "clap:bar", "", "definitely-not-builtin"] {
            assert!(!SpherePluginHost::is_builtin_ref(id));
        }
    }

    #[test]
    fn external_plugin_ids_have_no_origin() {
        assert_eq!(origin_for_plugin_id("vst3:some-plugin"), None);
        assert_eq!(origin_for_plugin_id("some-vst3-class"), None);
        assert_eq!(origin_for_plugin_id(""), None);
    }

    #[test]
    fn availability_explains_itself_rather_than_returning_a_bare_bool() {
        // Whatever the build config, a non-built-in never reports Ready and the
        // reason is always human-readable.
        let result = availability("vst3:whatever");
        assert_ne!(result, HostAvailability::Ready);
        assert!(!result.to_string().is_empty());
    }

    #[cfg(not(feature = "builtin-plugin-editor"))]
    #[test]
    fn without_the_feature_every_plugin_reports_not_compiled_in() {
        assert_eq!(
            availability("builtin:rodharerist"),
            HostAvailability::NotCompiledIn
        );
        assert!(HostAvailability::NotCompiledIn
            .to_string()
            .contains("builtin-plugin-editor"));
    }

    #[cfg(feature = "builtin-plugin-editor")]
    #[test]
    fn builtins_with_an_editor_are_hostable_and_the_rest_are_not() {
        // These embed a UI in any build that ran their build script against a
        // built dist; either way they must never be `NotCompiledIn` here.
        for id in ["builtin:rodharerist", "builtin:equz8", "builtin:mixstation"] {
            assert_ne!(availability(id), HostAvailability::NotCompiledIn);
        }
        // A catalogued built-in that ships no editor bundle is refused by name,
        // not reported as an empty asset table.
        assert_eq!(
            availability("builtin:compresser"),
            HostAvailability::NoEditorForPlugin("builtin:compresser".to_string())
        );
    }

    /// The mirror is keyed by insert slot id, which is not unique across
    /// plugins — one built-in must never read, replay, or persist another's
    /// state out of the same slot.
    #[cfg(feature = "builtin-plugin-editor")]
    #[test]
    fn the_state_mirror_never_hands_one_builtin_another_plugins_state() {
        let insert = "test-insert-state-mirror-isolation";
        let freq = builtin_param_index("equz8", "band1_freq").expect("band1_freq is an EQ-Z8 id");
        builtin_state_apply("equz8", insert, freq, 137.0);

        let bytes = builtin_state_bytes("equz8", insert).expect("EQ-Z8 owns this slot's state");
        let json = String::from_utf8(bytes).expect("state blobs are UTF-8 JSON");
        assert!(json.contains("137"), "the edit is missing from {json}");
        assert!(!builtin_state_replay("equz8", insert).is_empty());

        // Same slot id, different plugin: nothing to hand over.
        assert!(builtin_state_bytes("rodharerist", insert).is_none());
        assert!(builtin_state_replay("rodharerist", insert).is_empty());

        // Re-pointing the slot replaces the state rather than folding into it.
        builtin_state_apply("rodharerist", insert, 0, 0.5);
        assert!(builtin_state_bytes("equz8", insert).is_none());

        builtin_state_remove(insert);
        assert!(builtin_state_bytes("rodharerist", insert).is_none());
    }

    #[cfg(feature = "builtin-plugin-editor")]
    #[test]
    fn param_ids_resolve_per_plugin_and_never_cross_over() {
        // `outputDb` exists in EQ-Z8's table; asking rodharerist for it must not
        // silently resolve against the wrong plugin's indices.
        assert!(builtin_param_index("builtin:equz8", "band3_freq").is_some());
        assert!(builtin_param_index("builtin:equz8", "not-a-param").is_none());
        assert!(builtin_param_index("builtin:compresser", "band3_freq").is_none());
        assert_eq!(
            builtin_param_index("builtin:mixstation", "inputTrimDb"),
            Some(mixstation::ipc::INPUT_TRIM_INDEX)
        );
        assert!(builtin_param_index("builtin:mixstation", "not-a-param").is_none());
    }

    #[cfg(feature = "builtin-plugin-editor")]
    #[test]
    fn mixstation_state_defaults_apply_serialize_seed_and_replay() {
        let insert = "test-mixstation-state-round-trip";
        let trim = builtin_param_index("mixstation", "inputTrimDb").expect("trim index");
        builtin_state_apply("mixstation", insert, trim, 7.5);

        let bytes = builtin_state_bytes("mixstation", insert).expect("state serializes");
        let state = mixstation::ipc::MixStationState::from_json(
            std::str::from_utf8(&bytes).expect("state is UTF-8"),
        )
        .expect("state parses");
        assert_eq!(state.params.input_trim_db, 7.5);
        assert_eq!(
            builtin_state_replay("mixstation", insert)
                .into_iter()
                .find(|(index, _)| *index == trim),
            Some((trim, 7.5))
        );

        let seeded_insert = "test-mixstation-state-seed";
        builtin_state_seed("builtin:mixstation", seeded_insert, &bytes);
        let seeded = builtin_state_bytes("mixstation", seeded_insert).expect("seed persisted");
        let seeded_state = mixstation::ipc::MixStationState::from_json(
            std::str::from_utf8(&seeded).expect("seed is UTF-8"),
        )
        .expect("seed parses");
        assert_eq!(seeded_state.params.input_trim_db, 7.5);

        builtin_state_remove(insert);
        builtin_state_remove(seeded_insert);
    }
}
