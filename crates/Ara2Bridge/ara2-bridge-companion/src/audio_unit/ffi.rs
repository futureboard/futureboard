//! Narrow C ABI for Audio Unit v2 ARA instance properties.

use ara2_bridge_sys::{ARADocumentControllerRef, ARAFactory, ARAPlugInExtensionInstance};
use std::ffi::c_void;

/// Audio Component cache tag indicating possible ARA support.
pub const ARA_AUDIO_COMPONENT_TAG: &str = "ARA";
/// Magic value required in every ARA Audio Unit property record.
pub const ARA_AUDIO_UNIT_MAGIC: u32 = u32::from_be_bytes(*b"Ara!");
/// Read-only factory instance property.
pub const AUDIO_UNIT_PROPERTY_ARA_FACTORY: u32 = u32::from_be_bytes(*b"AraF");
/// Deprecated generation-1 binding instance property.
pub const AUDIO_UNIT_PROPERTY_ARA_BINDING: u32 = u32::from_be_bytes(*b"AraB");
/// Role-aware generation-2 binding instance property.
pub const AUDIO_UNIT_PROPERTY_ARA_BINDING_WITH_ROLES: u32 = u32::from_be_bytes(*b"AraE");
/// Audio Unit global property scope.
pub const AUDIO_UNIT_SCOPE_GLOBAL: u32 = 0;

/// Factory property input/output record.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AraAudioUnitFactory {
    /// Collision-detection token, preserved on success.
    pub in_out_magic_number: u32,
    /// Stable factory pointer written on success.
    pub out_factory: *const ARAFactory,
}

/// Binding property input/output record.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AraAudioUnitPluginExtensionBinding {
    /// Collision-detection token, preserved on success.
    pub in_out_magic_number: u32,
    /// Host-owned controller being bound.
    pub in_document_controller_ref: ARADocumentControllerRef,
    /// Stable extension instance pointer written on success.
    pub out_plugin_extension: *const ARAPlugInExtensionInstance,
    /// Roles understood by the host.
    pub known_roles: i32,
    /// Roles assigned to this instance.
    pub assigned_roles: i32,
}

/// Callback returning the plug-in's stable factory.
pub type Ara2AudioUnitGetFactoryCallback =
    Option<unsafe extern "C" fn(context: *mut c_void) -> *const ARAFactory>;
/// Callback performing one generation-1 or role-aware binding.
pub type Ara2AudioUnitBindCallback = Option<
    unsafe extern "C" fn(
        context: *mut c_void,
        controller: ARADocumentControllerRef,
        known_roles: i32,
        assigned_roles: i32,
    ) -> *const ARAPlugInExtensionInstance,
>;
/// Callback destroying plug-in property-handler state.
pub type Ara2AudioUnitDropCallback = Option<unsafe extern "C" fn(context: *mut c_void)>;

/// Callbacks owned by one native Audio Unit property handler.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Ara2AudioUnitPluginCallbacks {
    /// Opaque implementation context.
    pub context: *mut c_void,
    /// Factory lookup callback.
    pub get_factory: Ara2AudioUnitGetFactoryCallback,
    /// Binding callback.
    pub bind: Ara2AudioUnitBindCallback,
    /// Final context destruction callback.
    pub drop: Ara2AudioUnitDropCallback,
}

unsafe extern "C" {
    /// Creates one external-processor property handler.
    pub fn ara2_audio_unit_plugin_create(
        callbacks: *const Ara2AudioUnitPluginCallbacks,
        output: *mut *mut c_void,
    ) -> i32;
    /// Destroys one property handler and its callback state.
    pub fn ara2_audio_unit_plugin_destroy(handler: *mut c_void);
    /// Implements `AUBase::GetPropertyInfo` for ARA properties.
    pub fn ara2_audio_unit_plugin_get_property_info(
        handler: *mut c_void,
        property: u32,
        scope: u32,
        element: u32,
        output_size: *mut u32,
        output_writable: *mut u8,
    ) -> i32;
    /// Implements `AUBase::GetProperty` for ARA properties.
    pub fn ara2_audio_unit_plugin_get_property(
        handler: *mut c_void,
        property: u32,
        scope: u32,
        element: u32,
        data: *mut c_void,
        data_size: u32,
    ) -> i32;
    /// Discovers an ARA factory through a live Audio Unit instance property.
    pub fn ara2_audio_unit_host_get_factory(
        audio_unit: *mut c_void,
        output: *mut *const ARAFactory,
    ) -> i32;
    /// Binds through the role-aware property with optional generation-1 fallback.
    pub fn ara2_audio_unit_host_bind(
        audio_unit: *mut c_void,
        controller: ARADocumentControllerRef,
        known_roles: i32,
        assigned_roles: i32,
        allow_legacy_fallback: u8,
        output: *mut *const ARAPlugInExtensionInstance,
    ) -> i32;
}
