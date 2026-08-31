//! Checked-in direct declarations generated from the pinned CLAP and ARA headers.

use ara2_bridge_sys::{
    ARADocumentControllerRef, ARAFactory, ARAPlugInExtensionInstance, ARAPlugInInstanceRoleFlags,
};
use std::ffi::{c_char, c_void};

/// CLAP 1.1.9 semantic version.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClapVersion {
    /// ABI/API major version.
    pub major: u32,
    /// Compatible feature minor version.
    pub minor: u32,
    /// Revision version.
    pub revision: u32,
}

/// Pinned CLAP version used by these declarations.
pub const CLAP_VERSION: ClapVersion = ClapVersion {
    major: 1,
    minor: 1,
    revision: 9,
};

/// Stable ARA CLAP factory extension ID.
pub const CLAP_EXT_ARA_FACTORY: &str = "org.ara-audio.ara.factory/2";
/// Latest ABI-compatible draft ARA CLAP factory extension ID.
pub const CLAP_EXT_ARA_FACTORY_COMPAT: &str = "org.ara-audio.ara.factory.draft/2";
/// Stable ARA CLAP plug-in extension ID.
pub const CLAP_EXT_ARA_PLUGIN_EXTENSION: &str = "org.ara-audio.ara.pluginextension/2";
/// Latest ABI-compatible draft ARA CLAP plug-in extension ID.
pub const CLAP_EXT_ARA_PLUGIN_EXTENSION_COMPAT: &str = "org.ara-audio.ara.pluginextension.draft/2";
/// CLAP descriptor feature for optional ARA support.
pub const CLAP_PLUGIN_FEATURE_ARA_SUPPORTED: &str = "ara:supported";
/// CLAP descriptor feature for plug-ins that require ARA.
pub const CLAP_PLUGIN_FEATURE_ARA_REQUIRED: &str = "ara:required";
/// Standard CLAP plug-in factory ID.
pub const CLAP_PLUGIN_FACTORY_ID: &str = "clap.plugin-factory";

/// Opaque CLAP host record used by factory creation callbacks.
#[repr(C)]
pub struct ClapHost {
    _private: [u8; 0],
}

/// Opaque CLAP process record passed through the audio processor boundary.
#[repr(C)]
pub struct ClapProcess {
    _private: [u8; 0],
}

/// CLAP process callback status.
pub type ClapProcessStatus = i32;

/// CLAP plug-in metadata retained through entry deinitialization.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ClapPluginDescriptor {
    /// CLAP version used by the descriptor.
    pub clap_version: ClapVersion,
    /// Unique plug-in ID.
    pub id: *const c_char,
    /// Display name.
    pub name: *const c_char,
    /// Vendor name.
    pub vendor: *const c_char,
    /// Product URL.
    pub url: *const c_char,
    /// Manual URL.
    pub manual_url: *const c_char,
    /// Support URL.
    pub support_url: *const c_char,
    /// Version string.
    pub version: *const c_char,
    /// Description.
    pub description: *const c_char,
    /// Null-terminated feature string array.
    pub features: *const *const c_char,
}

/// Complete CLAP plug-in instance interface.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ClapPlugin {
    /// Stable descriptor.
    pub desc: *const ClapPluginDescriptor,
    /// Implementation-owned state pointer.
    pub plugin_data: *mut c_void,
    /// Initializes the instance.
    pub init: Option<unsafe extern "C" fn(*const ClapPlugin) -> bool>,
    /// Destroys the instance.
    pub destroy: Option<unsafe extern "C" fn(*const ClapPlugin)>,
    /// Activates processing.
    pub activate: Option<unsafe extern "C" fn(*const ClapPlugin, f64, u32, u32) -> bool>,
    /// Deactivates processing.
    pub deactivate: Option<unsafe extern "C" fn(*const ClapPlugin)>,
    /// Enters the processing state.
    pub start_processing: Option<unsafe extern "C" fn(*const ClapPlugin) -> bool>,
    /// Leaves the processing state.
    pub stop_processing: Option<unsafe extern "C" fn(*const ClapPlugin)>,
    /// Resets processor state.
    pub reset: Option<unsafe extern "C" fn(*const ClapPlugin)>,
    /// Processes one block.
    pub process:
        Option<unsafe extern "C" fn(*const ClapPlugin, *const ClapProcess) -> ClapProcessStatus>,
    /// Queries one extension ID.
    pub get_extension:
        Option<unsafe extern "C" fn(*const ClapPlugin, *const c_char) -> *const c_void>,
    /// Runs the requested main-thread callback.
    pub on_main_thread: Option<unsafe extern "C" fn(*const ClapPlugin)>,
}

/// CLAP dynamic-library entry record.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ClapPluginEntry {
    /// CLAP version implemented by this entry.
    pub clap_version: ClapVersion,
    /// Initializes the entry exactly once.
    pub init: Option<unsafe extern "C" fn(*const c_char) -> bool>,
    /// Deinitializes the entry.
    pub deinit: Option<unsafe extern "C" fn()>,
    /// Queries one factory ID.
    pub get_factory: Option<unsafe extern "C" fn(*const c_char) -> *const c_void>,
}

/// Standard CLAP plug-in discovery and creation factory.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ClapPluginFactory {
    /// Returns the descriptor count.
    pub get_plugin_count: Option<unsafe extern "C" fn(*const ClapPluginFactory) -> u32>,
    /// Returns one descriptor by index.
    pub get_plugin_descriptor:
        Option<unsafe extern "C" fn(*const ClapPluginFactory, u32) -> *const ClapPluginDescriptor>,
    /// Creates a plug-in by exact descriptor ID.
    pub create_plugin: Option<
        unsafe extern "C" fn(
            *const ClapPluginFactory,
            *const ClapHost,
            *const c_char,
        ) -> *const ClapPlugin,
    >,
}

/// CLAP entry-level ARA factory discovery extension.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ClapAraFactory {
    /// Returns the number of ARA-capable plug-in associations.
    pub get_factory_count: Option<unsafe extern "C" fn(*const ClapAraFactory) -> u32>,
    /// Returns one stable ARA factory pointer.
    pub get_ara_factory:
        Option<unsafe extern "C" fn(*const ClapAraFactory, u32) -> *const ARAFactory>,
    /// Returns the associated CLAP descriptor ID.
    pub get_plugin_id: Option<unsafe extern "C" fn(*const ClapAraFactory, u32) -> *const c_char>,
}

/// CLAP instance-level ARA extension.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ClapAraPluginExtension {
    /// Returns the exact ARA factory associated with this plug-in.
    pub get_factory: Option<unsafe extern "C" fn(*const ClapPlugin) -> *const ARAFactory>,
    /// Binds the instance to one document controller exactly once.
    pub bind_to_document_controller: Option<
        unsafe extern "C" fn(
            *const ClapPlugin,
            ARADocumentControllerRef,
            ARAPlugInInstanceRoleFlags,
            ARAPlugInInstanceRoleFlags,
        ) -> *const ARAPlugInExtensionInstance,
    >,
}
