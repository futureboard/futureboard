//! Checked host control of companion-owned ARA extension interfaces.

use crate::{DocumentSession, PlaybackRegionHandle, RegionSequenceHandle};
use ara2_bridge_core::{ApiGeneration, AraError, ContentTimeRange, RegistrySession};
use ara2_bridge_sys::{access::read_field, *};
use bitflags::bitflags;
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::mem::{offset_of, size_of};
use std::ptr::{null, NonNull};
use std::rc::{Rc, Weak};
use std::thread::ThreadId;

type PlaybackRegionFn = unsafe extern "C" fn(ARAPlaybackRendererRef, ARAPlaybackRegionRef);
type EditorRegionFn = unsafe extern "C" fn(ARAEditorRendererRef, ARAPlaybackRegionRef);
type EditorSequenceFn = unsafe extern "C" fn(ARAEditorRendererRef, ARARegionSequenceRef);
type SelectionFn = unsafe extern "C" fn(ARAEditorViewRef, *const ARAViewSelection);
type HiddenSequencesFn =
    unsafe extern "C" fn(ARAEditorViewRef, ARASize, *const ARARegionSequenceRef);
type LegacyRegionFn = unsafe extern "C" fn(ARAPlugInExtensionRef, ARAPlaybackRegionRef);

bitflags! {
    /// ARA 2 roles known or assigned by the companion API.
    #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
    pub struct ExtensionRoles: i32 {
        /// Playback renderer role.
        const PLAYBACK_RENDERER = kARAPlaybackRendererRole as i32;
        /// Editor renderer role.
        const EDITOR_RENDERER = kARAEditorRendererRole as i32;
        /// Editor view role.
        const EDITOR_VIEW = kARAEditorViewRole as i32;
    }
}

/// Renderer role used when assigning a playback region.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum RendererRole {
    /// Realtime/offline playback renderer.
    Playback,
    /// Editor-preview renderer.
    Editor,
}

#[derive(Clone, Copy)]
struct PlaybackDispatch {
    reference: ARAPlaybackRendererRef,
    add: PlaybackRegionFn,
    remove: PlaybackRegionFn,
}

#[derive(Clone, Copy)]
struct EditorDispatch {
    reference: ARAEditorRendererRef,
    add_region: EditorRegionFn,
    remove_region: EditorRegionFn,
    add_sequence: EditorSequenceFn,
    remove_sequence: EditorSequenceFn,
}

#[derive(Clone, Copy)]
struct ViewDispatch {
    reference: ARAEditorViewRef,
    selection: SelectionFn,
    hidden: HiddenSequencesFn,
}

#[derive(Clone, Copy)]
struct LegacyDispatch {
    reference: ARAPlugInExtensionRef,
    set: LegacyRegionFn,
    remove: LegacyRegionFn,
}

pub(crate) struct ExtensionState {
    session: RegistrySession,
    model_thread: ThreadId,
    active: Cell<bool>,
    rendering: Cell<bool>,
    generation: ApiGeneration,
    playback: Option<PlaybackDispatch>,
    editor: Option<EditorDispatch>,
    view: Option<ViewDispatch>,
    legacy: Option<LegacyDispatch>,
    playback_assignments: RefCell<HashSet<(RendererRole, ARAPlaybackRegionRef)>>,
    sequence_assignments: RefCell<HashSet<ARARegionSequenceRef>>,
}

impl ExtensionState {
    fn require_session(&self, session: RegistrySession) -> Result<(), AraError> {
        if std::thread::current().id() != self.model_thread {
            Err(AraError::InvalidThread("extension controller model thread"))
        } else if !self.active.get() {
            Err(AraError::InvalidState("extension controller is closed"))
        } else if session != self.session {
            Err(AraError::InvalidArgument(
                "extension controller belongs to another document",
            ))
        } else {
            Ok(())
        }
    }

    fn remove_playback(&self, role: RendererRole, peer: ARAPlaybackRegionRef) {
        if !self.playback_assignments.borrow_mut().remove(&(role, peer)) {
            return;
        }
        // SAFETY: dispatch was prefix-validated and extension backing remains caller-owned.
        unsafe {
            match (self.generation < ApiGeneration::V2Draft, role) {
                (true, _) => {
                    if let Some(dispatch) = self.legacy {
                        (dispatch.remove)(dispatch.reference, peer);
                    }
                }
                (false, RendererRole::Playback) => {
                    if let Some(dispatch) = self.playback {
                        (dispatch.remove)(dispatch.reference, peer);
                    }
                }
                (false, RendererRole::Editor) => {
                    if let Some(dispatch) = self.editor {
                        (dispatch.remove_region)(dispatch.reference, peer);
                    }
                }
            }
        }
    }

    fn require_assignment(&self, session: RegistrySession) -> Result<(), AraError> {
        self.require_session(session)?;
        if self.rendering.get() {
            Err(AraError::InvalidState(
                "renderer assignments cannot change while rendering",
            ))
        } else {
            Ok(())
        }
    }

    fn remove_sequence(&self, peer: ARARegionSequenceRef) {
        if !self.sequence_assignments.borrow_mut().remove(&peer) {
            return;
        }
        if let Some(dispatch) = self.editor {
            // SAFETY: dispatch was prefix-validated and extension backing remains caller-owned.
            unsafe { (dispatch.remove_sequence)(dispatch.reference, peer) };
        }
    }

    pub(crate) fn shutdown(&self) -> Result<(), AraError> {
        if !self.active.get() {
            return Ok(());
        }
        if std::thread::current().id() != self.model_thread {
            return Err(AraError::InvalidThread(
                "extension controller teardown thread",
            ));
        }
        let playback = self
            .playback_assignments
            .borrow()
            .iter()
            .copied()
            .collect::<Vec<_>>();
        for (role, peer) in playback {
            self.remove_playback(role, peer);
        }
        let sequences = self
            .sequence_assignments
            .borrow()
            .iter()
            .copied()
            .collect::<Vec<_>>();
        for peer in sequences {
            self.remove_sequence(peer);
        }
        self.active.set(false);
        Ok(())
    }
}

/// Bound, non-owning control of one companion plug-in extension instance.
pub struct ExtensionController<'extension> {
    state: Rc<ExtensionState>,
    _backing: std::marker::PhantomData<&'extension ARAPlugInExtensionInstance>,
}

impl<'extension> ExtensionController<'extension> {
    /// Validates a companion-owned extension instance and its role/interface pairs.
    ///
    /// # Safety
    ///
    /// `instance` and every represented interface/reference pair must remain valid until this
    /// controller and all assignments produced from it have been dropped. The instance must
    /// already be bound to `document` by its companion API.
    pub(crate) unsafe fn bind(
        instance: *const ARAPlugInExtensionInstance,
        generation: ApiGeneration,
        known: ExtensionRoles,
        assigned: ExtensionRoles,
        document: &DocumentSession<'_, '_>,
    ) -> Result<Self, AraError> {
        if !(assigned & !known).is_empty() {
            return Err(AraError::InvalidArgument(
                "assigned extension roles must be declared known",
            ));
        }
        let instance = NonNull::new(instance.cast_mut())
            .ok_or(AraError::InvalidArgument("null extension instance"))?;
        // SAFETY: caller guarantees a readable instance header.
        let struct_size = unsafe { read_field::<ARASize>(instance.as_ptr().cast(), 0) };
        if struct_size < kARAPlugInExtensionInstanceMinSize as usize {
            return Err(AraError::Abi("truncated extension instance"));
        }
        let base = instance.as_ptr().cast::<u8>();
        let represented = |offset: usize, extent: usize| offset + extent <= struct_size;
        macro_rules! field {
            ($name:ident, $type:ty) => {{
                if !represented(
                    offset_of!(ARAPlugInExtensionInstance, $name),
                    size_of::<$type>(),
                ) {
                    return Err(AraError::Abi("extension instance field outside prefix"));
                }
                // SAFETY: the represented-field check covers the packed field.
                unsafe { read_field::<$type>(base, offset_of!(ARAPlugInExtensionInstance, $name)) }
            }};
        }

        let (legacy, playback, editor, view) = if generation < ApiGeneration::V2Draft {
            let reference = field!(plugInExtensionRef, ARAPlugInExtensionRef);
            let interface = field!(plugInExtensionInterface, *const ARAPlugInExtensionInterface);
            let interface =
                validate_interface(interface, kARAPlugInExtensionInterfaceMinSize as usize)?;
            // SAFETY: the validated minimum prefix represents both callbacks.
            let set = unsafe {
                read_field::<Option<LegacyRegionFn>>(
                    interface.as_ptr().cast(),
                    offset_of!(ARAPlugInExtensionInterface, setPlaybackRegion),
                )
            }
            .ok_or(AraError::Abi("null legacy set-playback-region callback"))?;
            // SAFETY: same validated prefix.
            let remove = unsafe {
                read_field::<Option<LegacyRegionFn>>(
                    interface.as_ptr().cast(),
                    offset_of!(ARAPlugInExtensionInterface, removePlaybackRegion),
                )
            }
            .ok_or(AraError::Abi("null legacy remove-playback-region callback"))?;
            if reference.is_null() {
                return Err(AraError::Abi("null legacy extension reference"));
            }
            (
                Some(LegacyDispatch {
                    reference,
                    set,
                    remove,
                }),
                None,
                None,
                None,
            )
        } else {
            let playback = validate_role_pair(
                field!(playbackRendererRef, ARAPlaybackRendererRef),
                field!(
                    playbackRendererInterface,
                    *const ARAPlaybackRendererInterface
                ),
                ExtensionRoles::PLAYBACK_RENDERER,
                known,
                assigned,
                |reference, interface| {
                    let interface = validate_interface(
                        interface,
                        kARAPlaybackRendererInterfaceMinSize as usize,
                    )?;
                    // SAFETY: the validated prefix represents both callbacks.
                    let add = unsafe {
                        read_field::<Option<PlaybackRegionFn>>(
                            interface.as_ptr().cast(),
                            offset_of!(ARAPlaybackRendererInterface, addPlaybackRegion),
                        )
                    }
                    .ok_or(AraError::Abi("null playback add callback"))?;
                    // SAFETY: same validated prefix.
                    let remove = unsafe {
                        read_field::<Option<PlaybackRegionFn>>(
                            interface.as_ptr().cast(),
                            offset_of!(ARAPlaybackRendererInterface, removePlaybackRegion),
                        )
                    }
                    .ok_or(AraError::Abi("null playback remove callback"))?;
                    Ok(PlaybackDispatch {
                        reference,
                        add,
                        remove,
                    })
                },
            )?;
            let editor = validate_role_pair(
                field!(editorRendererRef, ARAEditorRendererRef),
                field!(editorRendererInterface, *const ARAEditorRendererInterface),
                ExtensionRoles::EDITOR_RENDERER,
                known,
                assigned,
                |reference, interface| {
                    let interface =
                        validate_interface(interface, kARAEditorRendererInterfaceMinSize as usize)?;
                    // SAFETY: the validated prefix represents every required editor callback.
                    let add_region = unsafe {
                        read_field::<Option<EditorRegionFn>>(
                            interface.as_ptr().cast(),
                            offset_of!(ARAEditorRendererInterface, addPlaybackRegion),
                        )
                    }
                    .ok_or(AraError::Abi("null editor add-region callback"))?;
                    // SAFETY: same validated prefix.
                    let remove_region = unsafe {
                        read_field::<Option<EditorRegionFn>>(
                            interface.as_ptr().cast(),
                            offset_of!(ARAEditorRendererInterface, removePlaybackRegion),
                        )
                    }
                    .ok_or(AraError::Abi("null editor remove-region callback"))?;
                    // SAFETY: same validated prefix.
                    let add_sequence = unsafe {
                        read_field::<Option<EditorSequenceFn>>(
                            interface.as_ptr().cast(),
                            offset_of!(ARAEditorRendererInterface, addRegionSequence),
                        )
                    }
                    .ok_or(AraError::Abi("null editor add-sequence callback"))?;
                    // SAFETY: same validated prefix.
                    let remove_sequence = unsafe {
                        read_field::<Option<EditorSequenceFn>>(
                            interface.as_ptr().cast(),
                            offset_of!(ARAEditorRendererInterface, removeRegionSequence),
                        )
                    }
                    .ok_or(AraError::Abi("null editor remove-sequence callback"))?;
                    Ok(EditorDispatch {
                        reference,
                        add_region,
                        remove_region,
                        add_sequence,
                        remove_sequence,
                    })
                },
            )?;
            let view = validate_role_pair(
                field!(editorViewRef, ARAEditorViewRef),
                field!(editorViewInterface, *const ARAEditorViewInterface),
                ExtensionRoles::EDITOR_VIEW,
                known,
                assigned,
                |reference, interface| {
                    let interface =
                        validate_interface(interface, kARAEditorViewInterfaceMinSize as usize)?;
                    // SAFETY: the validated prefix represents both callbacks.
                    let selection = unsafe {
                        read_field::<Option<SelectionFn>>(
                            interface.as_ptr().cast(),
                            offset_of!(ARAEditorViewInterface, notifySelection),
                        )
                    }
                    .ok_or(AraError::Abi("null editor-view selection callback"))?;
                    // SAFETY: same validated prefix.
                    let hidden = unsafe {
                        read_field::<Option<HiddenSequencesFn>>(
                            interface.as_ptr().cast(),
                            offset_of!(ARAEditorViewInterface, notifyHideRegionSequences),
                        )
                    }
                    .ok_or(AraError::Abi("null editor-view hidden-sequence callback"))?;
                    Ok(ViewDispatch {
                        reference,
                        selection,
                        hidden,
                    })
                },
            )?;
            (None, playback, editor, view)
        };

        Ok(Self {
            state: Rc::new(ExtensionState {
                session: document.extension_session_id(),
                model_thread: std::thread::current().id(),
                active: Cell::new(true),
                rendering: Cell::new(false),
                generation,
                playback,
                editor,
                view,
                legacy,
                playback_assignments: RefCell::new(HashSet::new()),
                sequence_assignments: RefCell::new(HashSet::new()),
            }),
            _backing: std::marker::PhantomData,
        })
    }

    pub(crate) fn weak_state(&self) -> Weak<ExtensionState> {
        Rc::downgrade(&self.state)
    }

    /// Updates the companion instance render-state gate for assignment validation.
    pub fn set_rendering(&self, rendering: bool) -> Result<(), AraError> {
        self.state.require_session(self.state.session)?;
        self.state.rendering.set(rendering);
        Ok(())
    }

    /// Assigns a live playback region to one enabled renderer role.
    pub fn assign_playback_region(
        &self,
        document: &DocumentSession<'_, '_>,
        role: RendererRole,
        handle: PlaybackRegionHandle,
    ) -> Result<PlaybackRegionAssignment, AraError> {
        self.state
            .require_assignment(document.extension_session_id())?;
        let peer = document.extension_playback_region_peer(handle)?;
        let key = (role, peer);
        if self.state.playback_assignments.borrow().contains(&key) {
            return Err(AraError::InvalidState(
                "playback region is already assigned to this renderer",
            ));
        }
        // SAFETY: selected dispatch was validated at bind and peer belongs to this document.
        unsafe {
            match (self.state.generation < ApiGeneration::V2Draft, role) {
                (true, _) => {
                    let dispatch = self
                        .state
                        .legacy
                        .ok_or(AraError::Unsupported("legacy extension"))?;
                    if !self.state.playback_assignments.borrow().is_empty() {
                        return Err(AraError::InvalidState(
                            "ARA 1 extension already has a playback region",
                        ));
                    }
                    (dispatch.set)(dispatch.reference, peer);
                }
                (false, RendererRole::Playback) => {
                    let dispatch = self
                        .state
                        .playback
                        .ok_or(AraError::Unsupported("playback renderer role"))?;
                    (dispatch.add)(dispatch.reference, peer);
                }
                (false, RendererRole::Editor) => {
                    let dispatch = self
                        .state
                        .editor
                        .ok_or(AraError::Unsupported("editor renderer role"))?;
                    (dispatch.add_region)(dispatch.reference, peer);
                }
            }
        }
        self.state.playback_assignments.borrow_mut().insert(key);
        Ok(PlaybackRegionAssignment {
            state: self.state.clone(),
            role,
            peer,
            active: true,
        })
    }

    /// Assigns a live region sequence to the enabled editor renderer.
    pub fn assign_region_sequence(
        &self,
        document: &DocumentSession<'_, '_>,
        handle: RegionSequenceHandle,
    ) -> Result<RegionSequenceAssignment, AraError> {
        self.state
            .require_assignment(document.extension_session_id())?;
        let dispatch = self
            .state
            .editor
            .ok_or(AraError::Unsupported("editor renderer role"))?;
        let peer = document.extension_region_sequence_peer(handle)?;
        if self.state.sequence_assignments.borrow().contains(&peer) {
            return Err(AraError::InvalidState(
                "region sequence is already assigned",
            ));
        }
        // SAFETY: dispatch and peer were validated above.
        unsafe { (dispatch.add_sequence)(dispatch.reference, peer) };
        self.state.sequence_assignments.borrow_mut().insert(peer);
        Ok(RegionSequenceAssignment {
            state: self.state.clone(),
            peer,
            active: true,
        })
    }

    /// Publishes a checked editor-view selection using plug-in peer references.
    pub fn notify_selection(
        &self,
        document: &DocumentSession<'_, '_>,
        playback_regions: &[PlaybackRegionHandle],
        region_sequences: &[RegionSequenceHandle],
        time_range: Option<ContentTimeRange>,
    ) -> Result<(), AraError> {
        self.state
            .require_session(document.extension_session_id())?;
        let dispatch = self
            .state
            .view
            .ok_or(AraError::Unsupported("editor view role"))?;
        let playback_regions = playback_regions
            .iter()
            .copied()
            .map(|handle| document.extension_playback_region_peer(handle))
            .collect::<Result<Vec<_>, _>>()?;
        let region_sequences = region_sequences
            .iter()
            .copied()
            .map(|handle| document.extension_region_sequence_peer(handle))
            .collect::<Result<Vec<_>, _>>()?;
        let raw = ARAViewSelection {
            structSize: size_of::<ARAViewSelection>(),
            playbackRegionRefsCount: playback_regions.len(),
            playbackRegionRefs: if playback_regions.is_empty() {
                null()
            } else {
                playback_regions.as_ptr()
            },
            regionSequenceRefsCount: region_sequences.len(),
            regionSequenceRefs: if region_sequences.is_empty() {
                null()
            } else {
                region_sequences.as_ptr()
            },
            timeRange: time_range.as_ref().map_or(null(), ContentTimeRange::as_ptr),
        };
        // SAFETY: all array/range backing remains live through this synchronous call.
        unsafe { (dispatch.selection)(dispatch.reference, &raw) };
        Ok(())
    }

    /// Publishes the checked list of region sequences hidden in the editor view.
    pub fn notify_hidden_region_sequences(
        &self,
        document: &DocumentSession<'_, '_>,
        region_sequences: &[RegionSequenceHandle],
    ) -> Result<(), AraError> {
        self.state
            .require_session(document.extension_session_id())?;
        let dispatch = self
            .state
            .view
            .ok_or(AraError::Unsupported("editor view role"))?;
        let region_sequences = region_sequences
            .iter()
            .copied()
            .map(|handle| document.extension_region_sequence_peer(handle))
            .collect::<Result<Vec<_>, _>>()?;
        // SAFETY: the peer array remains live through this synchronous call.
        unsafe {
            (dispatch.hidden)(
                dispatch.reference,
                region_sequences.len(),
                if region_sequences.is_empty() {
                    null()
                } else {
                    region_sequences.as_ptr()
                },
            )
        };
        Ok(())
    }
}

impl Drop for ExtensionController<'_> {
    fn drop(&mut self) {
        let _ = self.state.shutdown();
    }
}

/// RAII playback-region renderer assignment.
pub struct PlaybackRegionAssignment {
    state: Rc<ExtensionState>,
    role: RendererRole,
    peer: ARAPlaybackRegionRef,
    active: bool,
}

impl Drop for PlaybackRegionAssignment {
    fn drop(&mut self) {
        if self.active && self.state.active.get() {
            self.state.remove_playback(self.role, self.peer);
            self.active = false;
        }
    }
}

/// RAII editor-renderer region-sequence assignment.
pub struct RegionSequenceAssignment {
    state: Rc<ExtensionState>,
    peer: ARARegionSequenceRef,
    active: bool,
}

impl Drop for RegionSequenceAssignment {
    fn drop(&mut self) {
        if self.active && self.state.active.get() {
            self.state.remove_sequence(self.peer);
            self.active = false;
        }
    }
}

fn validate_interface<T>(pointer: *const T, minimum: usize) -> Result<NonNull<T>, AraError> {
    let pointer =
        NonNull::new(pointer.cast_mut()).ok_or(AraError::Abi("null extension interface"))?;
    // SAFETY: the binding contract makes every advertised interface header readable.
    let size = unsafe { read_field::<ARASize>(pointer.as_ptr().cast(), 0) };
    if size < minimum {
        Err(AraError::Abi("truncated extension interface"))
    } else {
        Ok(pointer)
    }
}

fn validate_role_pair<R: Copy, I, T>(
    reference: *mut R,
    interface: *const I,
    role: ExtensionRoles,
    known: ExtensionRoles,
    assigned: ExtensionRoles,
    build: impl FnOnce(*mut R, *const I) -> Result<T, AraError>,
) -> Result<Option<T>, AraError> {
    let present = !reference.is_null() && !interface.is_null();
    if reference.is_null() != interface.is_null() {
        return Err(AraError::Abi(
            "incoherent extension reference/interface pair",
        ));
    }
    if known.contains(role) && !assigned.contains(role) && present {
        return Err(AraError::Abi("unassigned known extension role is present"));
    }
    if assigned.contains(role) && !present {
        return Err(AraError::Abi("assigned extension role is absent"));
    }
    present.then(|| build(reference, interface)).transpose()
}
