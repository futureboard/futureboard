//! CLAP plug-in-side ARA factory exposure and one-shot instance binding.

use super::sys::{
    ClapAraFactory, ClapAraPluginExtension, ClapPlugin, CLAP_EXT_ARA_FACTORY,
    CLAP_EXT_ARA_FACTORY_COMPAT, CLAP_EXT_ARA_PLUGIN_EXTENSION,
    CLAP_EXT_ARA_PLUGIN_EXTENSION_COMPAT,
};
use crate::{
    notify_document_controller_destroyed, record_controller_destroy_snapshot,
    register_controller_destroy_handler, CompanionControllerBinding, CompanionFactory,
    CompanionProcessorBinding, CompanionRoles, ControllerDestroyRegistration,
    ControllerDestroySnapshot, LifecycleEvent,
};
use ara2_bridge_core::AraError;
use ara2_bridge_sys::{ARADocumentControllerRef, ARAFactory, ARAPlugInExtensionInstance};
use std::collections::HashMap;
use std::ffi::{c_char, c_void, CStr, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr::NonNull;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct EntryRecord<'factory> {
    factory: CompanionFactory<'factory>,
    plugin_id: CString,
}

#[repr(C)]
struct EntryAllocation<'factory> {
    interface: ClapAraFactory,
    records: Box<[EntryRecord<'factory>]>,
}

unsafe extern "C" fn entry_count(factory: *const ClapAraFactory) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if factory.is_null() {
            return 0;
        }
        // SAFETY: factory points to the first field of a live `EntryAllocation` published below.
        let allocation = unsafe { &*factory.cast::<EntryAllocation<'_>>() };
        u32::try_from(allocation.records.len()).unwrap_or(0)
    }))
    .unwrap_or(0)
}

unsafe extern "C" fn entry_factory(
    factory: *const ClapAraFactory,
    index: u32,
) -> *const ARAFactory {
    catch_unwind(AssertUnwindSafe(|| {
        if factory.is_null() {
            return std::ptr::null();
        }
        // SAFETY: same first-field allocation identity as `entry_count`.
        let allocation = unsafe { &*factory.cast::<EntryAllocation<'_>>() };
        allocation
            .records
            .get(index as usize)
            .map_or(std::ptr::null(), |record| record.factory.as_raw())
    }))
    .unwrap_or(std::ptr::null())
}

unsafe extern "C" fn entry_plugin_id(factory: *const ClapAraFactory, index: u32) -> *const c_char {
    catch_unwind(AssertUnwindSafe(|| {
        if factory.is_null() {
            return std::ptr::null();
        }
        // SAFETY: same first-field allocation identity as `entry_count`.
        let allocation = unsafe { &*factory.cast::<EntryAllocation<'_>>() };
        allocation
            .records
            .get(index as usize)
            .map_or(std::ptr::null(), |record| record.plugin_id.as_ptr())
    }))
    .unwrap_or(std::ptr::null())
}

/// Plug-in entry owner exposing ARA-capable CLAP associations without instantiation.
pub struct ClapAraEntry<'factory> {
    allocation: Box<EntryAllocation<'factory>>,
}

impl<'factory> ClapAraEntry<'factory> {
    /// Creates a stable CLAP ARA factory extension from the ARA-capable subset.
    pub fn new(
        factories: impl IntoIterator<Item = CompanionFactory<'factory>>,
    ) -> Result<Self, AraError> {
        let records = factories
            .into_iter()
            .map(|factory| {
                let plugin_id = CString::new(factory.id())
                    .map_err(|_| AraError::InvalidArgument("CLAP plug-in ID contains NUL"))?;
                Ok(EntryRecord { factory, plugin_id })
            })
            .collect::<Result<Vec<_>, AraError>>()?;
        if records.len() > u32::MAX as usize {
            return Err(AraError::InvalidArgument("too many CLAP ARA factories"));
        }
        for (index, record) in records.iter().enumerate() {
            if records[..index]
                .iter()
                .any(|candidate| candidate.plugin_id == record.plugin_id)
            {
                return Err(AraError::InvalidArgument(
                    "duplicate CLAP ARA plug-in association ID",
                ));
            }
        }
        Ok(Self {
            allocation: Box::new(EntryAllocation {
                interface: ClapAraFactory {
                    get_factory_count: Some(entry_count),
                    get_ara_factory: Some(entry_factory),
                    get_plugin_id: Some(entry_plugin_id),
                },
                records: records.into_boxed_slice(),
            }),
        })
    }

    /// Returns the number of ARA-capable associations.
    pub fn factory_count(&self) -> usize {
        self.allocation.records.len()
    }

    /// Returns whether an extension ID is the stable or accepted compatible factory ID.
    pub fn supports_factory_id(&self, extension_id: &str) -> bool {
        matches!(
            extension_id,
            CLAP_EXT_ARA_FACTORY | CLAP_EXT_ARA_FACTORY_COMPAT
        )
    }

    /// Returns the stable raw entry-level ARA factory extension.
    pub fn as_raw(&self) -> *const ClapAraFactory {
        // Cast from the complete allocation so callbacks retain provenance for both the public
        // first field and the private association backing recovered from that same address.
        std::ptr::from_ref(self.allocation.as_ref()).cast()
    }

    /// Resolves a stable/draft CLAP factory query exactly as an entry callback would.
    pub fn extension(&self, extension_id: &str) -> *const c_void {
        if self.supports_factory_id(extension_id) {
            self.as_raw().cast()
        } else {
            std::ptr::null()
        }
    }
}

type ExtensionBuilder = dyn Fn(
        ARADocumentControllerRef,
        CompanionRoles,
        CompanionRoles,
    ) -> Result<*const ARAPlugInExtensionInstance, AraError>
    + Send
    + Sync;

struct PluginState {
    interface: ClapAraPluginExtension,
    processor: CompanionProcessorBinding<'static>,
    factory_id: String,
    controller: Mutex<Option<CompanionControllerBinding<'static>>>,
    controller_destroy_registration: Mutex<Option<ControllerDestroyRegistration>>,
    extension_builder: Box<ExtensionBuilder>,
}

fn registry() -> &'static Mutex<HashMap<usize, Weak<PluginState>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<usize, Weak<PluginState>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn plugin_state(plugin: *const ClapPlugin) -> Option<Arc<PluginState>> {
    lock(registry())
        .get(&(plugin as usize))
        .and_then(Weak::upgrade)
}

unsafe extern "C" fn plugin_factory(plugin: *const ClapPlugin) -> *const ARAFactory {
    catch_unwind(AssertUnwindSafe(|| {
        let Some(state) = plugin_state(plugin) else {
            return std::ptr::null();
        };
        state
            .processor
            .factory_for_id(&state.factory_id)
            .map_or(std::ptr::null(), CompanionFactory::as_raw)
    }))
    .unwrap_or(std::ptr::null())
}

unsafe extern "C" fn plugin_bind(
    plugin: *const ClapPlugin,
    controller: ARADocumentControllerRef,
    known_roles: i32,
    assigned_roles: i32,
) -> *const ARAPlugInExtensionInstance {
    catch_unwind(AssertUnwindSafe(|| {
        let Some(state) = plugin_state(plugin) else {
            return std::ptr::null();
        };
        let Some(known_roles) = CompanionRoles::from_bits(known_roles) else {
            return std::ptr::null();
        };
        let Some(assigned_roles) = CompanionRoles::from_bits(assigned_roles) else {
            return std::ptr::null();
        };
        let mut current = lock(&state.controller);
        if current.is_some() {
            return std::ptr::null();
        }
        let Ok(extension) = (state.extension_builder)(controller, known_roles, assigned_roles)
        else {
            return std::ptr::null();
        };
        if extension.is_null() {
            return std::ptr::null();
        }
        // SAFETY: the CLAP host owns the controller through both permitted teardown orders.
        let Ok(binding) = (unsafe {
            state
                .processor
                .bind(controller, known_roles, assigned_roles)
        }) else {
            return std::ptr::null();
        };
        let state_weak = Arc::downgrade(&state);
        let controller_key = controller as usize;
        let registration = register_controller_destroy_handler(controller, move || {
            if let Some(state) = state_weak.upgrade() {
                let probe = state.processor.lifetime_probe();
                let processor_alive_before_controller_drop = probe.processor_alive();
                let controller_alive_before_controller_drop = probe.controller_alive();
                lock(&state.controller).take();
                lock(&state.controller_destroy_registration).take();
                record_controller_destroy_snapshot(
                    controller_key as ARADocumentControllerRef,
                    ControllerDestroySnapshot {
                        processor_alive_before_controller_drop,
                        controller_alive_before_controller_drop,
                        processor_alive_after_controller_drop: probe.processor_alive(),
                        controller_alive_after_controller_drop: probe.controller_alive(),
                    },
                );
            }
        });
        *current = Some(binding);
        *lock(&state.controller_destroy_registration) = Some(registration);
        extension
    }))
    .unwrap_or(std::ptr::null())
}

/// Registry-backed CLAP `get_extension` callback for an attached external processor.
///
/// # Safety
///
/// `plugin` must be a live identity registered by [`ClapAraPluginAdapter::attach`], and `id` must
/// point to a readable NUL-terminated CLAP extension ID for the duration of the call.
pub unsafe extern "C" fn clap_ara_get_extension(
    plugin: *const ClapPlugin,
    id: *const c_char,
) -> *const c_void {
    catch_unwind(AssertUnwindSafe(|| {
        if id.is_null() {
            return std::ptr::null();
        }
        let Some(state) = plugin_state(plugin) else {
            return std::ptr::null();
        };
        // SAFETY: callback precondition makes the extension ID readable through NUL.
        let Ok(id) = (unsafe { CStr::from_ptr(id) }).to_str() else {
            return std::ptr::null();
        };
        if matches!(
            id,
            CLAP_EXT_ARA_PLUGIN_EXTENSION | CLAP_EXT_ARA_PLUGIN_EXTENSION_COMPAT
        ) {
            std::ptr::from_ref(&state.interface).cast()
        } else {
            std::ptr::null()
        }
    }))
    .unwrap_or(std::ptr::null())
}

/// ARA adapter attached to one externally owned CLAP processor identity.
pub struct ClapAraPluginAdapter {
    plugin: NonNull<ClapPlugin>,
    state: Arc<PluginState>,
}

impl ClapAraPluginAdapter {
    /// Attaches a one-shot ARA binding and factory association to an external CLAP processor.
    ///
    /// # Safety
    ///
    /// `plugin` must remain at a stable readable address until this adapter is dropped. The
    /// external plug-in must route ARA extension queries to [`clap_ara_get_extension`] and notify
    /// this adapter before every lifecycle boundary represented by its observation methods.
    pub unsafe fn attach(
        plugin: *const ClapPlugin,
        processor: CompanionProcessorBinding<'static>,
        factory_id: &str,
        extension_builder: impl Fn(
                ARADocumentControllerRef,
                CompanionRoles,
                CompanionRoles,
            ) -> Result<*const ARAPlugInExtensionInstance, AraError>
            + Send
            + Sync
            + 'static,
    ) -> Result<Self, AraError> {
        let plugin = NonNull::new(plugin.cast_mut())
            .ok_or(AraError::InvalidArgument("null CLAP plug-in identity"))?;
        if processor.factory_for_id(factory_id).is_none() {
            return Err(AraError::InvalidArgument(
                "CLAP plug-in references an unknown ARA factory association",
            ));
        }
        let state = Arc::new(PluginState {
            interface: ClapAraPluginExtension {
                get_factory: Some(plugin_factory),
                bind_to_document_controller: Some(plugin_bind),
            },
            processor,
            factory_id: factory_id.to_owned(),
            controller: Mutex::new(None),
            controller_destroy_registration: Mutex::new(None),
            extension_builder: Box::new(extension_builder),
        });
        let mut registry = lock(registry());
        if registry
            .insert(plugin.as_ptr() as usize, Arc::downgrade(&state))
            .and_then(|previous| previous.upgrade())
            .is_some()
        {
            return Err(AraError::InvalidState(
                "CLAP plug-in identity already has an ARA adapter",
            ));
        }
        drop(registry);
        Ok(Self { plugin, state })
    }

    /// Returns the exact ARA factory associated with this processor.
    pub fn factory(&self) -> *const ARAFactory {
        self.state
            .processor
            .factory_for_id(&self.state.factory_id)
            .map_or(std::ptr::null(), CompanionFactory::as_raw)
    }

    /// Records state loading before delegating to the external processor.
    pub fn observe_state_load(&self) -> Result<(), AraError> {
        self.state.processor.observe(LifecycleEvent::StateLoad)
    }

    /// Records processor activation before delegating to the external processor.
    pub fn observe_activation(&self) -> Result<(), AraError> {
        self.state.processor.observe(LifecycleEvent::Activate)
    }

    /// Records one processing-related operation.
    pub fn observe_processing(&self) -> Result<(), AraError> {
        self.state.processor.observe(LifecycleEvent::Process)
    }

    /// Records processor deactivation before delegating to the external processor.
    pub fn observe_deactivation(&self) -> Result<(), AraError> {
        self.state.processor.observe(LifecycleEvent::Deactivate)
    }

    /// Records custom-view creation before delegating to the external processor.
    pub fn observe_view_creation(&self) -> Result<(), AraError> {
        self.state.processor.observe(LifecycleEvent::CreateView)
    }

    /// Records host-driven document-controller destruction before the host releases it.
    ///
    /// Fires the destroy handler registered at binding, which drops the controller binding,
    /// releases its registration, and captures a [`ControllerDestroySnapshot`]. Later processor
    /// boundaries then fail without dereferencing the stale controller reference.
    ///
    /// Returns [`AraError::InvalidState`] when no controller binding is live.
    pub fn observe_controller_destruction(&self) -> Result<(), AraError> {
        // The handler locks the same controller slot, so release the guard before notifying.
        let controller = {
            let binding = lock(&self.state.controller);
            binding.as_ref().map(CompanionControllerBinding::controller)
        };
        let Some(controller) = controller else {
            return Err(AraError::InvalidState(
                "CLAP plug-in has no live ARA controller binding",
            ));
        };
        notify_document_controller_destroyed(controller);
        Ok(())
    }
}

impl Drop for ClapAraPluginAdapter {
    fn drop(&mut self) {
        lock(registry()).remove(&(self.plugin.as_ptr() as usize));
        lock(&self.state.controller_destroy_registration).take();
        let _ = lock(&self.state.controller).take();
    }
}
