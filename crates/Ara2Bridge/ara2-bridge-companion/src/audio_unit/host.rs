//! Host-side Audio Unit v2 ARA instance-property adapter.

use super::ffi::{ara2_audio_unit_host_bind, ara2_audio_unit_host_get_factory};
use crate::CompanionRoles;
use ara2_bridge_core::AraError;
use ara2_bridge_sys::{ARADocumentControllerRef, ARAFactory, ARAPlugInExtensionInstance};
use std::cell::Cell;
use std::ffi::c_void;
use std::marker::PhantomData;
use std::ptr::NonNull;

/// Borrowed checked host view of one live Audio Unit v2 instance.
pub struct AudioUnitHostInstance<'instance> {
    audio_unit: NonNull<c_void>,
    bound: Cell<bool>,
    _lifetime: PhantomData<&'instance c_void>,
}

impl<'instance> AudioUnitHostInstance<'instance> {
    /// Admits a live Audio Unit v2 instance.
    ///
    /// # Safety
    ///
    /// `audio_unit` must remain a live `AudioUnit` for `'instance` and all methods must run on
    /// threads permitted by the Audio Unit and ARA property contracts.
    pub unsafe fn from_raw(audio_unit: *mut c_void) -> Result<Self, AraError> {
        Ok(Self {
            audio_unit: NonNull::new(audio_unit)
                .ok_or(AraError::InvalidArgument("null Audio Unit instance"))?,
            bound: Cell::new(false),
            _lifetime: PhantomData,
        })
    }

    /// Discovers and validates the instance-level ARA factory property.
    pub fn factory(&self) -> Result<*const ARAFactory, AraError> {
        let mut output = std::ptr::null();
        // SAFETY: constructor retains the live instance lifetime and output is writable.
        let status =
            unsafe { ara2_audio_unit_host_get_factory(self.audio_unit.as_ptr(), &mut output) };
        if status != 0 || output.is_null() {
            Err(AraError::Unsupported("Audio Unit v2 ARA factory property"))
        } else {
            Ok(output)
        }
    }

    /// Binds through the role-aware property, optionally falling back to generation 1.
    ///
    /// # Safety
    ///
    /// `controller` must belong to this instance's exact factory and remain valid through the
    /// permitted Audio Unit/controller teardown order.
    pub unsafe fn bind(
        &self,
        controller: ARADocumentControllerRef,
        known_roles: CompanionRoles,
        assigned_roles: CompanionRoles,
        allow_legacy_fallback: bool,
    ) -> Result<*const ARAPlugInExtensionInstance, AraError> {
        if self.bound.get() {
            return Err(AraError::InvalidState("Audio Unit is already ARA-bound"));
        }
        if controller.is_null() || !known_roles.contains(assigned_roles) {
            return Err(AraError::InvalidArgument(
                "invalid Audio Unit ARA controller or role set",
            ));
        }
        let mut output = std::ptr::null();
        // SAFETY: caller forwards controller ownership; constructor retains the Audio Unit.
        let status = unsafe {
            ara2_audio_unit_host_bind(
                self.audio_unit.as_ptr(),
                controller,
                known_roles.bits(),
                assigned_roles.bits(),
                u8::from(allow_legacy_fallback),
                &mut output,
            )
        };
        if status != 0 || output.is_null() {
            Err(AraError::Peer("Audio Unit rejected ARA binding"))
        } else {
            self.bound.set(true);
            Ok(output)
        }
    }
}
