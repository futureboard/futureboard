//! Companion-neutral one-shot processor/controller binding.

use crate::LifecycleEvent;
use ara2_bridge_core::AraError;
use ara2_bridge_sys::{
    kARAEditorRendererRole, kARAEditorViewRole, kARAPlaybackRendererRole, ARADocumentControllerRef,
    ARAFactory,
};
use std::collections::HashMap;
use std::marker::PhantomData;
use std::ptr::NonNull;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak};
use std::thread::ThreadId;

const MAXIMUM_FACTORY_ID_BYTES: usize = 16 * 1024;

bitflags::bitflags! {
    /// ARA roles negotiated by a companion binding.
    #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
    pub struct CompanionRoles: i32 {
        /// Playback renderer role.
        const PLAYBACK_RENDERER = kARAPlaybackRendererRole as i32;
        /// Editor renderer role.
        const EDITOR_RENDERER = kARAEditorRendererRole as i32;
        /// Editor view role.
        const EDITOR_VIEW = kARAEditorViewRole as i32;
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

type ControllerDestroyHandler = dyn Fn() + Send + Sync + 'static;

/// Diagnostic snapshot captured by a format adapter at exact document-controller destruction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ControllerDestroySnapshot {
    /// Whether the processor-side binding owner was alive before dropping the controller owner.
    pub processor_alive_before_controller_drop: bool,
    /// Whether the controller-side binding owner was alive before dropping it.
    pub controller_alive_before_controller_drop: bool,
    /// Whether the processor-side binding owner remained alive after dropping the controller owner.
    pub processor_alive_after_controller_drop: bool,
    /// Whether the controller-side binding owner remained alive after dropping it.
    pub controller_alive_after_controller_drop: bool,
}

fn controller_destroy_registry(
) -> &'static Mutex<HashMap<usize, Vec<Weak<ControllerDestroyHandler>>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<usize, Vec<Weak<ControllerDestroyHandler>>>>> =
        OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn controller_destroy_observation_registry(
) -> &'static Mutex<HashMap<usize, Vec<ControllerDestroySnapshot>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<usize, Vec<ControllerDestroySnapshot>>>> =
        OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// RAII registration for one exact document-controller destroy notification.
#[must_use]
pub struct ControllerDestroyRegistration {
    controller: usize,
    handler: Arc<ControllerDestroyHandler>,
}

impl Drop for ControllerDestroyRegistration {
    fn drop(&mut self) {
        let mut registry = lock(controller_destroy_registry());
        if let Some(handlers) = registry.get_mut(&self.controller) {
            handlers.retain(|handler| {
                handler
                    .upgrade()
                    .is_some_and(|current| !Arc::ptr_eq(&current, &self.handler))
            });
            if handlers.is_empty() {
                registry.remove(&self.controller);
            }
        }
    }
}

/// Registers a companion-owned action for the exact factory-side document controller reference.
pub fn register_controller_destroy_handler(
    controller: ARADocumentControllerRef,
    handler: impl Fn() + Send + Sync + 'static,
) -> ControllerDestroyRegistration {
    let controller = controller as usize;
    let handler: Arc<ControllerDestroyHandler> = Arc::new(handler);
    lock(controller_destroy_registry())
        .entry(controller)
        .or_default()
        .push(Arc::downgrade(&handler));
    ControllerDestroyRegistration {
        controller,
        handler,
    }
}

/// Notifies all live handlers registered for one exact factory-side document controller reference.
pub fn notify_document_controller_destroyed(controller: ARADocumentControllerRef) {
    let controller = controller as usize;
    let handlers = {
        let mut registry = lock(controller_destroy_registry());
        let Some(handlers) = registry.get_mut(&controller) else {
            return;
        };
        let live = handlers
            .iter()
            .filter_map(Weak::upgrade)
            .collect::<Vec<_>>();
        handlers.retain(|handler| Weak::strong_count(handler) > 0);
        if handlers.is_empty() {
            registry.remove(&controller);
        }
        live
    };
    for handler in handlers {
        handler();
    }
}

#[cfg(any(feature = "clap", feature = "vst3", feature = "audio-unit-v2"))]
pub(crate) fn record_controller_destroy_snapshot(
    controller: ARADocumentControllerRef,
    snapshot: ControllerDestroySnapshot,
) {
    lock(controller_destroy_observation_registry())
        .entry(controller as usize)
        .or_default()
        .push(snapshot);
}

/// Returns and clears controller-destroy snapshots captured for one exact controller reference.
pub fn take_controller_destroy_snapshots(
    controller: ARADocumentControllerRef,
) -> Vec<ControllerDestroySnapshot> {
    lock(controller_destroy_observation_registry())
        .remove(&(controller as usize))
        .unwrap_or_default()
}

/// Counts live destroy handlers for one exact controller reference, pruning stale registrations.
pub fn controller_destroy_handler_count(controller: ARADocumentControllerRef) -> usize {
    let controller = controller as usize;
    let mut registry = lock(controller_destroy_registry());
    let Some(handlers) = registry.get_mut(&controller) else {
        return 0;
    };
    handlers.retain(|handler| Weak::strong_count(handler) > 0);
    let count = handlers.len();
    if count == 0 {
        registry.remove(&controller);
    }
    count
}

/// Stable companion-visible association between an ID and one ARA factory.
#[derive(Clone)]
pub struct CompanionFactory<'factory> {
    id: String,
    raw: NonNull<ARAFactory>,
    _lifetime: PhantomData<&'factory ARAFactory>,
}

impl<'factory> CompanionFactory<'factory> {
    /// Admits one stable raw factory for companion discovery.
    ///
    /// # Safety
    ///
    /// `factory` must remain readable at the same address for `'factory`. Its own ARA callbacks
    /// and nested backing must satisfy the host or plug-in runtime contract when later consumed.
    pub unsafe fn from_raw(id: &str, factory: &'factory ARAFactory) -> Result<Self, AraError> {
        if id.is_empty()
            || id.len() > MAXIMUM_FACTORY_ID_BYTES
            || !id.is_ascii()
            || id.as_bytes().contains(&0)
        {
            return Err(AraError::InvalidArgument(
                "companion factory ID must be bounded non-empty ASCII without NUL",
            ));
        }
        Ok(Self {
            id: id.to_owned(),
            raw: NonNull::from(factory),
            _lifetime: PhantomData,
        })
    }

    /// Returns the companion association ID.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the stable raw ARA factory pointer.
    pub const fn as_raw(&self) -> *const ARAFactory {
        self.raw.as_ptr()
    }
}

// SAFETY: the constructor requires process-external immutable factory backing for the retained
// lifetime. ARA factory callbacks are explicitly shared companion discovery state.
unsafe impl Send for CompanionFactory<'_> {}
// SAFETY: same immutable factory-backing contract as `Send`.
unsafe impl Sync for CompanionFactory<'_> {}

struct LifecycleState {
    bound: bool,
    boundary_observed_before_binding: bool,
    active: bool,
    rendering: bool,
    processor_alive: bool,
    controller_alive: bool,
    controller: Option<NonNull<ara2_bridge_sys::ARADocumentControllerRefMarkupType>>,
    known_roles: CompanionRoles,
    assigned_roles: CompanionRoles,
    enabled_roles: CompanionRoles,
}

// SAFETY: the opaque controller pointer is never dereferenced by shared neutral state. Adapters
// must obey the bind safety contract before using the typed pointer on an appropriate thread.
unsafe impl Send for LifecycleState {}

struct BindingState<'factory> {
    factories: Box<[CompanionFactory<'factory>]>,
    supported_roles: CompanionRoles,
    model_thread: ThreadId,
    lifecycle: Mutex<LifecycleState>,
}

/// Processor-owned companion boundary that permits exactly one ARA controller binding.
pub struct CompanionProcessorBinding<'factory> {
    state: Arc<BindingState<'factory>>,
}

impl<'factory> CompanionProcessorBinding<'factory> {
    /// Creates a processor binding with stable factories and supported ARA roles.
    pub fn new(
        factories: impl IntoIterator<Item = CompanionFactory<'factory>>,
        supported_roles: CompanionRoles,
    ) -> Result<Self, AraError> {
        let factories = factories.into_iter().collect::<Vec<_>>();
        if factories.is_empty() {
            return Err(AraError::InvalidArgument(
                "a companion processor requires at least one ARA factory",
            ));
        }
        for (index, factory) in factories.iter().enumerate() {
            if factories[..index]
                .iter()
                .any(|candidate| candidate.id == factory.id)
            {
                return Err(AraError::InvalidArgument(
                    "duplicate companion factory association ID",
                ));
            }
        }
        Ok(Self {
            state: Arc::new(BindingState {
                factories: factories.into_boxed_slice(),
                supported_roles,
                model_thread: std::thread::current().id(),
                lifecycle: Mutex::new(LifecycleState {
                    bound: false,
                    boundary_observed_before_binding: false,
                    active: false,
                    rendering: false,
                    processor_alive: true,
                    controller_alive: false,
                    controller: None,
                    known_roles: CompanionRoles::empty(),
                    assigned_roles: CompanionRoles::empty(),
                    enabled_roles: CompanionRoles::empty(),
                }),
            }),
        })
    }

    /// Returns the number of stable companion factory associations.
    pub fn factory_count(&self) -> usize {
        self.state.factories.len()
    }

    /// Returns one stable companion factory association by index.
    pub fn factory(&self, index: usize) -> Option<&CompanionFactory<'factory>> {
        self.state.factories.get(index)
    }

    /// Returns one stable companion factory association by exact ID.
    pub fn factory_for_id(&self, id: &str) -> Option<&CompanionFactory<'factory>> {
        self.state.factories.iter().find(|factory| factory.id == id)
    }

    /// Returns the ARA roles supported by this processor implementation.
    pub fn supported_roles(&self) -> CompanionRoles {
        self.state.supported_roles
    }

    /// Binds the processor to one document controller exactly once.
    ///
    /// # Safety
    ///
    /// `controller` must be a live controller reference from one of this processor's advertised
    /// factories and remain valid until the returned controller binding is dropped or tombstoned
    /// by its companion adapter. Calls that dereference it must additionally obey ARA threading.
    pub unsafe fn bind(
        &self,
        controller: ARADocumentControllerRef,
        known_roles: CompanionRoles,
        assigned_roles: CompanionRoles,
    ) -> Result<CompanionControllerBinding<'factory>, AraError> {
        if std::thread::current().id() != self.state.model_thread {
            return Err(AraError::InvalidState(
                "companion binding must occur on the processor model thread",
            ));
        }
        let controller = NonNull::new(controller).ok_or(AraError::InvalidArgument(
            "null companion controller reference",
        ))?;
        if !known_roles.contains(assigned_roles) {
            return Err(AraError::InvalidArgument(
                "assigned companion roles must be a subset of known roles",
            ));
        }
        let mut lifecycle = lock(&self.state.lifecycle);
        if lifecycle.bound {
            return Err(AraError::InvalidState(
                "companion processor is already bound",
            ));
        }
        if lifecycle.boundary_observed_before_binding {
            return Err(AraError::InvalidState(
                "companion binding occurred after a processor boundary",
            ));
        }
        let enabled_roles = self.state.supported_roles
            & (CompanionRoles::from_bits_retain(!known_roles.bits()) | assigned_roles);
        lifecycle.bound = true;
        lifecycle.controller_alive = true;
        lifecycle.controller = Some(controller);
        lifecycle.known_roles = known_roles;
        lifecycle.assigned_roles = assigned_roles;
        lifecycle.enabled_roles = enabled_roles;
        drop(lifecycle);
        Ok(CompanionControllerBinding {
            state: Arc::clone(&self.state),
            active: true,
        })
    }

    /// Records and validates one processor lifecycle boundary.
    pub fn observe(&self, event: LifecycleEvent) -> Result<(), AraError> {
        let mut lifecycle = lock(&self.state.lifecycle);
        if !lifecycle.processor_alive {
            return Err(AraError::InvalidState("companion processor is destroyed"));
        }
        if !lifecycle.bound {
            lifecycle.boundary_observed_before_binding = true;
            return Err(AraError::InvalidState(
                "ARA binding must precede processor boundaries",
            ));
        }
        if !lifecycle.controller_alive {
            return Err(AraError::InvalidState(
                "ARA controller binding is destroyed",
            ));
        }
        match event {
            LifecycleEvent::StateLoad | LifecycleEvent::CreateView => {
                if lifecycle.active {
                    return Err(AraError::InvalidState(
                        "state/view boundary is unavailable while active",
                    ));
                }
            }
            LifecycleEvent::Activate => {
                if lifecycle.active {
                    return Err(AraError::InvalidState("processor is already active"));
                }
                lifecycle.active = true;
            }
            LifecycleEvent::Deactivate => {
                if !lifecycle.active || lifecycle.rendering {
                    return Err(AraError::InvalidState(
                        "processor cannot deactivate in its current state",
                    ));
                }
                lifecycle.active = false;
            }
            LifecycleEvent::Process => {
                if !lifecycle.active {
                    return Err(AraError::InvalidState("processing requires activation"));
                }
            }
            LifecycleEvent::BeginRendering => {
                if !lifecycle.active || lifecycle.rendering {
                    return Err(AraError::InvalidState(
                        "rendering cannot begin in the current state",
                    ));
                }
                lifecycle.rendering = true;
            }
            LifecycleEvent::EndRendering => {
                if !lifecycle.rendering {
                    return Err(AraError::InvalidState("processor is not rendering"));
                }
                lifecycle.rendering = false;
            }
            LifecycleEvent::ModelMutation => {
                if std::thread::current().id() != self.state.model_thread {
                    return Err(AraError::InvalidState(
                        "model mutation must run on the processor model thread",
                    ));
                }
                if lifecycle.rendering {
                    return Err(AraError::InvalidState(
                        "model mutation is unavailable while rendering",
                    ));
                }
            }
        }
        Ok(())
    }

    /// Returns a weak lifetime observer for teardown-order tests and diagnostics.
    pub fn lifetime_probe(&self) -> CompanionLifetimeProbe<'factory> {
        CompanionLifetimeProbe {
            state: Arc::downgrade(&self.state),
        }
    }
}

impl Drop for CompanionProcessorBinding<'_> {
    fn drop(&mut self) {
        lock(&self.state.lifecycle).processor_alive = false;
    }
}

/// Controller-side owner retained independently from the companion processor.
pub struct CompanionControllerBinding<'factory> {
    state: Arc<BindingState<'factory>>,
    active: bool,
}

impl CompanionControllerBinding<'_> {
    /// Returns the exact controller reference supplied at binding.
    pub fn controller(&self) -> ARADocumentControllerRef {
        lock(&self.state.lifecycle)
            .controller
            .map_or(std::ptr::null_mut(), NonNull::as_ptr)
    }

    /// Returns roles the host declared understood.
    pub fn known_roles(&self) -> CompanionRoles {
        lock(&self.state.lifecycle).known_roles
    }

    /// Returns roles the host explicitly assigned.
    pub fn assigned_roles(&self) -> CompanionRoles {
        lock(&self.state.lifecycle).assigned_roles
    }

    /// Returns supported roles enabled by the ARA known/assigned formula.
    pub fn enabled_roles(&self) -> CompanionRoles {
        lock(&self.state.lifecycle).enabled_roles
    }
}

impl Drop for CompanionControllerBinding<'_> {
    fn drop(&mut self) {
        if self.active {
            let mut lifecycle = lock(&self.state.lifecycle);
            lifecycle.controller_alive = false;
            lifecycle.controller = None;
            lifecycle.active = false;
            lifecycle.rendering = false;
            self.active = false;
        }
    }
}

/// Weak diagnostic view of processor/controller teardown state.
pub struct CompanionLifetimeProbe<'factory> {
    state: Weak<BindingState<'factory>>,
}

impl CompanionLifetimeProbe<'_> {
    /// Returns whether either side still retains shared binding storage.
    pub fn storage_is_alive(&self) -> bool {
        self.state.strong_count() != 0
    }

    /// Returns whether the processor owner is still live.
    pub fn processor_alive(&self) -> bool {
        self.state
            .upgrade()
            .is_some_and(|state| lock(&state.lifecycle).processor_alive)
    }

    /// Returns whether the controller owner is still live.
    pub fn controller_alive(&self) -> bool {
        self.state
            .upgrade()
            .is_some_and(|state| lock(&state.lifecycle).controller_alive)
    }
}
