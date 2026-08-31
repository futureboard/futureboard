//! Plug-in-side VST3 ARA COM adapters.

use super::ffi::{
    ara2_vst3_plugin_entry_create, ara2_vst3_query_interface, ara2_vst3_release,
    Ara2Vst3InterfaceKind, Ara2Vst3MainFactoryCallbacks, Ara2Vst3PluginEntryCallbacks,
    ARA2_VST3_OK,
};
use crate::{
    notify_document_controller_destroyed, record_controller_destroy_snapshot,
    register_controller_destroy_handler, CompanionControllerBinding, CompanionFactory,
    CompanionProcessorBinding, CompanionRoles, ControllerDestroyRegistration,
    ControllerDestroySnapshot, LifecycleEvent,
};
use ara2_bridge_core::AraError;
use ara2_bridge_sys::ARAPlugInExtensionInstance;
use std::ffi::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr::NonNull;
use std::sync::{Arc, Mutex, MutexGuard};

type ExtensionBuilder = dyn Fn(
        ara2_bridge_sys::ARADocumentControllerRef,
        CompanionRoles,
        CompanionRoles,
    ) -> Result<*const ARAPlugInExtensionInstance, AraError>
    + Send
    + Sync;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn native_error(operation: &'static str, result: i32) -> AraError {
    if result == ARA2_VST3_OK {
        AraError::Abi("unexpected successful VST3 result mapping")
    } else {
        AraError::Peer(operation)
    }
}

struct MainFactoryState {
    factory: CompanionFactory<'static>,
}

unsafe extern "C" fn main_get_factory(context: *mut c_void) -> *const c_void {
    catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: native COM object owns this Box until its final release callback.
        unsafe { &*context.cast::<MainFactoryState>() }
            .factory
            .as_raw()
            .cast()
    }))
    .unwrap_or(std::ptr::null())
}

unsafe extern "C" fn main_drop(context: *mut c_void) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: the native object invokes its drop callback once at reference count zero.
        drop(unsafe { Box::from_raw(context.cast::<MainFactoryState>()) });
    }));
}

/// Reference-counted native `ARA::IMainFactory` class implementation.
pub struct Vst3MainFactoryAdapter {
    interface: NonNull<c_void>,
    class_name: String,
}

impl Vst3MainFactoryAdapter {
    /// Creates a main-factory class whose registration name identifies the associated processor.
    pub fn new(
        class_name: impl Into<String>,
        factory: CompanionFactory<'static>,
    ) -> Result<Self, AraError> {
        let class_name = class_name.into();
        if class_name.is_empty() || class_name != factory.id() {
            return Err(AraError::InvalidArgument(
                "VST3 main-factory class name must equal its companion factory association ID",
            ));
        }
        // SAFETY: `CompanionFactory` admission guarantees stable readable factory backing.
        let metadata = unsafe {
            super::Vst3AraMainClass::from_raw([0; 16], class_name.clone(), factory.as_raw())
        }?;
        if metadata.plug_in_name() != class_name {
            return Err(AraError::InvalidArgument(
                "VST3 main-factory class name must equal ARAFactory.plugInName",
            ));
        }
        let context = Box::into_raw(Box::new(MainFactoryState { factory })).cast();
        let callbacks = Ara2Vst3MainFactoryCallbacks {
            context,
            get_factory: Some(main_get_factory),
            drop: Some(main_drop),
        };
        let mut interface = std::ptr::null_mut();
        // SAFETY: the context is transferred to the COM object on success.
        let result =
            unsafe { super::ffi::ara2_vst3_main_factory_create(&callbacks, &mut interface) };
        if result != ARA2_VST3_OK {
            // SAFETY: failed creation did not take ownership of the context.
            drop(unsafe { Box::from_raw(context.cast::<MainFactoryState>()) });
            return Err(native_error("VST3 main-factory creation failed", result));
        }
        Ok(Self {
            interface: NonNull::new(interface)
                .ok_or(AraError::Abi("VST3 shim returned a null main factory"))?,
            class_name,
        })
    }

    /// Returns the VST3 class-registration name, which must also name the processor class.
    pub fn class_name(&self) -> &str {
        &self.class_name
    }

    /// Returns the borrowed primary `IMainFactory` interface pointer.
    pub const fn as_raw(&self) -> *mut c_void {
        self.interface.as_ptr()
    }

    /// Queries one interface and transfers the returned COM reference to the caller.
    pub fn query_interface(&self, kind: Ara2Vst3InterfaceKind) -> Result<*mut c_void, AraError> {
        let mut output = std::ptr::null_mut();
        // SAFETY: the adapter retains its owning reference during this synchronous query.
        let result = unsafe { ara2_vst3_query_interface(self.as_raw(), kind, &mut output) };
        if result != ARA2_VST3_OK || output.is_null() {
            Err(native_error(
                "VST3 main-factory interface query failed",
                result,
            ))
        } else {
            Ok(output)
        }
    }
}

impl Drop for Vst3MainFactoryAdapter {
    fn drop(&mut self) {
        let mut remaining = 0;
        // SAFETY: this consumes the adapter's unique original owning COM reference.
        let _ = unsafe { ara2_vst3_release(self.as_raw(), &mut remaining) };
    }
}

struct EntryState {
    processor: CompanionProcessorBinding<'static>,
    factory_id: String,
    controller: Mutex<Option<CompanionControllerBinding<'static>>>,
    controller_destroy_registration: Mutex<Option<ControllerDestroyRegistration>>,
    extension_builder: Box<ExtensionBuilder>,
}

unsafe extern "C" fn entry_get_factory(context: *mut c_void) -> *const c_void {
    catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: native COM object owns a Box containing this Arc until final release.
        let state = unsafe { &*context.cast::<Arc<EntryState>>() };
        state
            .processor
            .factory_for_id(&state.factory_id)
            .map_or(std::ptr::null(), |factory| factory.as_raw().cast())
    }))
    .unwrap_or(std::ptr::null())
}

unsafe extern "C" fn entry_bind(
    context: *mut c_void,
    controller: *mut c_void,
    known_roles: i32,
    assigned_roles: i32,
) -> *const c_void {
    catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: native COM object owns a Box containing this Arc until final release.
        let state = unsafe { &*context.cast::<Arc<EntryState>>() };
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
        let Ok(extension) =
            (state.extension_builder)(controller.cast(), known_roles, assigned_roles)
        else {
            return std::ptr::null();
        };
        if extension.is_null() {
            return std::ptr::null();
        }
        // SAFETY: the VST3 host owns the controller through both permitted teardown orders.
        let Ok(binding) = (unsafe {
            state
                .processor
                .bind(controller.cast(), known_roles, assigned_roles)
        }) else {
            return std::ptr::null();
        };
        let state_weak = Arc::downgrade(state);
        let controller_ref = controller.cast();
        let controller_key = controller_ref as usize;
        let registration = register_controller_destroy_handler(controller_ref, move || {
            if let Some(state) = state_weak.upgrade() {
                let probe = state.processor.lifetime_probe();
                let processor_alive_before_controller_drop = probe.processor_alive();
                let controller_alive_before_controller_drop = probe.controller_alive();
                lock(&state.controller).take();
                lock(&state.controller_destroy_registration).take();
                record_controller_destroy_snapshot(
                    controller_key as ara2_bridge_sys::ARADocumentControllerRef,
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
        extension.cast()
    }))
    .unwrap_or(std::ptr::null())
}

unsafe extern "C" fn entry_drop(context: *mut c_void) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: the native object invokes its drop callback once at reference count zero.
        drop(unsafe { Box::from_raw(context.cast::<Arc<EntryState>>()) });
    }));
}

/// Plug-in entry-point object delegated from an external VST3 audio processor.
pub struct Vst3PluginEntryAdapter {
    interface: NonNull<c_void>,
    state: Arc<EntryState>,
}

impl Vst3PluginEntryAdapter {
    /// Creates generation-1 and role-aware entry interfaces for one processor class.
    pub fn new(
        processor: CompanionProcessorBinding<'static>,
        class_name: impl Into<String>,
        extension_builder: impl Fn(
                ara2_bridge_sys::ARADocumentControllerRef,
                CompanionRoles,
                CompanionRoles,
            ) -> Result<*const ARAPlugInExtensionInstance, AraError>
            + Send
            + Sync
            + 'static,
    ) -> Result<Self, AraError> {
        let class_name = class_name.into();
        if processor.factory_for_id(&class_name).is_none() {
            return Err(AraError::InvalidArgument(
                "VST3 processor class name has no matching companion factory",
            ));
        }
        let state = Arc::new(EntryState {
            processor,
            factory_id: class_name,
            controller: Mutex::new(None),
            controller_destroy_registration: Mutex::new(None),
            extension_builder: Box::new(extension_builder),
        });
        let context = Box::into_raw(Box::new(Arc::clone(&state))).cast();
        let callbacks = Ara2Vst3PluginEntryCallbacks {
            context,
            get_factory: Some(entry_get_factory),
            bind: Some(entry_bind),
            drop: Some(entry_drop),
        };
        let mut interface = std::ptr::null_mut();
        // SAFETY: the context is transferred to the native COM object on success.
        let result = unsafe { ara2_vst3_plugin_entry_create(&callbacks, &mut interface) };
        if result != ARA2_VST3_OK {
            // SAFETY: failed creation did not take ownership of the boxed Arc.
            drop(unsafe { Box::from_raw(context.cast::<Arc<EntryState>>()) });
            return Err(native_error("VST3 entry-point creation failed", result));
        }
        Ok(Self {
            interface: NonNull::new(interface)
                .ok_or(AraError::Abi("VST3 shim returned a null entry point"))?,
            state,
        })
    }

    /// Returns the borrowed primary generation-1 entry interface.
    pub const fn as_raw(&self) -> *mut c_void {
        self.interface.as_ptr()
    }

    /// Delegates an ARA IID query and transfers the returned COM reference to the caller.
    pub fn query_interface(&self, kind: Ara2Vst3InterfaceKind) -> Result<*mut c_void, AraError> {
        let mut output = std::ptr::null_mut();
        // SAFETY: the adapter retains its original reference through the query.
        let result = unsafe { ara2_vst3_query_interface(self.as_raw(), kind, &mut output) };
        if result != ARA2_VST3_OK || output.is_null() {
            Err(native_error(
                "VST3 entry-point interface query failed",
                result,
            ))
        } else {
            Ok(output)
        }
    }

    /// Records one external VST3 processor lifecycle boundary.
    pub fn observe(&self, event: LifecycleEvent) -> Result<(), AraError> {
        self.state.processor.observe(event)
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
                "VST3 processor has no live ARA controller binding",
            ));
        };
        notify_document_controller_destroyed(controller);
        Ok(())
    }
}

impl Drop for Vst3PluginEntryAdapter {
    fn drop(&mut self) {
        lock(&self.state.controller_destroy_registration).take();
        let _ = lock(&self.state.controller).take();
        let mut remaining = 0;
        // SAFETY: this consumes the adapter's unique original owning COM reference.
        let _ = unsafe { ara2_vst3_release(self.as_raw(), &mut remaining) };
    }
}
