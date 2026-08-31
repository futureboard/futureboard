//! Plug-in-side Audio Unit v2 ARA property adapter.

use super::ffi::{
    ara2_audio_unit_plugin_create, ara2_audio_unit_plugin_destroy,
    ara2_audio_unit_plugin_get_property, ara2_audio_unit_plugin_get_property_info,
    Ara2AudioUnitPluginCallbacks,
};
use crate::{
    notify_document_controller_destroyed, record_controller_destroy_snapshot,
    register_controller_destroy_handler, CompanionControllerBinding, CompanionProcessorBinding,
    CompanionRoles, ControllerDestroyRegistration, ControllerDestroySnapshot, LifecycleEvent,
};
use ara2_bridge_core::AraError;
use ara2_bridge_sys::{ARADocumentControllerRef, ARAFactory, ARAPlugInExtensionInstance};
use std::ffi::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr::NonNull;
use std::sync::{Arc, Mutex, MutexGuard};

type ExtensionBuilder = dyn Fn(
        ARADocumentControllerRef,
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

struct PluginState {
    processor: CompanionProcessorBinding<'static>,
    factory_id: String,
    controller: Mutex<Option<CompanionControllerBinding<'static>>>,
    controller_destroy_registration: Mutex<Option<ControllerDestroyRegistration>>,
    extension_builder: Box<ExtensionBuilder>,
}

unsafe extern "C" fn get_factory(context: *mut c_void) -> *const ARAFactory {
    catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: native handler owns a Box containing this Arc until destruction.
        let state = unsafe { &*context.cast::<Arc<PluginState>>() };
        state
            .processor
            .factory_for_id(&state.factory_id)
            .map_or(std::ptr::null(), |factory| factory.as_raw())
    }))
    .unwrap_or(std::ptr::null())
}

unsafe extern "C" fn bind(
    context: *mut c_void,
    controller: ara2_bridge_sys::ARADocumentControllerRef,
    known_roles: i32,
    assigned_roles: i32,
) -> *const ARAPlugInExtensionInstance {
    catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: native handler owns a Box containing this Arc until destruction.
        let state = unsafe { &*context.cast::<Arc<PluginState>>() };
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
        // SAFETY: the AU host owns the controller through both permitted teardown orders.
        let Ok(binding) = (unsafe {
            state
                .processor
                .bind(controller, known_roles, assigned_roles)
        }) else {
            return std::ptr::null();
        };
        let state_weak = Arc::downgrade(state);
        let controller_key = controller as usize;
        let registration = register_controller_destroy_handler(controller, move || {
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
        extension
    }))
    .unwrap_or(std::ptr::null())
}

unsafe extern "C" fn drop_state(context: *mut c_void) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: the native handler invokes this once during its destruction.
        drop(unsafe { Box::from_raw(context.cast::<Arc<PluginState>>()) });
    }));
}

/// Property delegate embedded by an externally implemented Audio Unit processor.
pub struct AudioUnitPluginAdapter {
    handler: NonNull<c_void>,
    state: Arc<PluginState>,
}

impl AudioUnitPluginAdapter {
    /// Creates an ARA property handler for one Audio Unit instance.
    pub fn new(
        processor: CompanionProcessorBinding<'static>,
        factory_id: impl Into<String>,
        extension_builder: impl Fn(
                ARADocumentControllerRef,
                CompanionRoles,
                CompanionRoles,
            ) -> Result<*const ARAPlugInExtensionInstance, AraError>
            + Send
            + Sync
            + 'static,
    ) -> Result<Self, AraError> {
        let factory_id = factory_id.into();
        if processor.factory_for_id(&factory_id).is_none() {
            return Err(AraError::InvalidArgument(
                "Audio Unit instance has no matching companion factory",
            ));
        }
        let state = Arc::new(PluginState {
            processor,
            factory_id,
            controller: Mutex::new(None),
            controller_destroy_registration: Mutex::new(None),
            extension_builder: Box::new(extension_builder),
        });
        let context = Box::into_raw(Box::new(Arc::clone(&state))).cast();
        let callbacks = Ara2AudioUnitPluginCallbacks {
            context,
            get_factory: Some(get_factory),
            bind: Some(bind),
            drop: Some(drop_state),
        };
        let mut handler = std::ptr::null_mut();
        // SAFETY: context ownership transfers to the native handler on success.
        let status = unsafe { ara2_audio_unit_plugin_create(&callbacks, &mut handler) };
        if status != 0 {
            // SAFETY: failed creation leaves context ownership with the caller.
            drop(unsafe { Box::from_raw(context.cast::<Arc<PluginState>>()) });
            return Err(AraError::Peer(
                "Audio Unit ARA property-handler creation failed",
            ));
        }
        Ok(Self {
            handler: NonNull::new(handler)
                .ok_or(AraError::Abi("Audio Unit shim returned a null handler"))?,
            state,
        })
    }

    /// Delegates ARA property-info handling from `AUBase::GetPropertyInfo`.
    pub fn get_property_info(
        &self,
        property: u32,
        scope: u32,
        element: u32,
        output_size: &mut u32,
        output_writable: &mut bool,
    ) -> i32 {
        let mut writable = u8::from(*output_writable);
        // SAFETY: handler and output storage remain live through the synchronous delegation.
        let status = unsafe {
            ara2_audio_unit_plugin_get_property_info(
                self.handler.as_ptr(),
                property,
                scope,
                element,
                output_size,
                &mut writable,
            )
        };
        if status == 0 {
            *output_writable = writable != 0;
        }
        status
    }

    /// Delegates ARA property reads from `AUBase::GetProperty`.
    ///
    /// # Safety
    ///
    /// `data` must name writable storage of exactly `data_size` bytes. For ARA properties it must
    /// contain the corresponding complete input/output record with the required magic value.
    pub unsafe fn get_property(
        &self,
        property: u32,
        scope: u32,
        element: u32,
        data: *mut c_void,
        data_size: u32,
    ) -> i32 {
        // SAFETY: caller forwards the Audio Unit property buffer contract.
        unsafe {
            ara2_audio_unit_plugin_get_property(
                self.handler.as_ptr(),
                property,
                scope,
                element,
                data,
                data_size,
            )
        }
    }

    /// Records one external Audio Unit lifecycle boundary.
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
                "Audio Unit has no live ARA controller binding",
            ));
        };
        notify_document_controller_destroyed(controller);
        Ok(())
    }
}

impl Drop for AudioUnitPluginAdapter {
    fn drop(&mut self) {
        lock(&self.state.controller_destroy_registration).take();
        let _ = lock(&self.state.controller).take();
        // SAFETY: this is the adapter's unique native handler ownership.
        unsafe { ara2_audio_unit_plugin_destroy(self.handler.as_ptr()) };
    }
}
