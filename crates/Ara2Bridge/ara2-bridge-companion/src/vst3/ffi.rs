//! Narrow C ABI exported by the audited C++ VST3 shim.

use std::ffi::{c_char, c_void};

/// Successful shim operation.
pub const ARA2_VST3_OK: i32 = 0;
/// A required pointer, enum, or role argument was invalid.
pub const ARA2_VST3_INVALID_ARGUMENT: i32 = -1;
/// The queried COM interface is unavailable.
pub const ARA2_VST3_NO_INTERFACE: i32 = -2;
/// A foreign peer returned a null or otherwise unusable result.
pub const ARA2_VST3_PEER_ERROR: i32 = -3;
/// A C++ exception was caught at the C ABI boundary.
pub const ARA2_VST3_EXCEPTION: i32 = -4;

/// Logical VST3 interface queried through the shim.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Ara2Vst3InterfaceKind {
    /// `Steinberg::FUnknown` identity.
    Unknown = 0,
    /// `ARA::IMainFactory`.
    MainFactory = 1,
    /// `ARA::IPlugInEntryPoint`.
    PluginEntry = 2,
    /// `ARA::IPlugInEntryPoint2`.
    PluginEntry2 = 3,
}

/// Platform-neutral representation of the four words used to declare a VST3 IID.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Ara2Vst3InterfaceId {
    /// IID words in the order supplied to `DECLARE_CLASS_IID`.
    pub words: [u32; 4],
}

/// Callback returning a stable ARA factory pointer.
pub type Ara2Vst3GetFactoryCallback =
    Option<unsafe extern "C" fn(context: *mut c_void) -> *const c_void>;
/// Callback binding a processor to one ARA document controller.
pub type Ara2Vst3BindCallback = Option<
    unsafe extern "C" fn(
        context: *mut c_void,
        document_controller: *mut c_void,
        known_roles: i32,
        assigned_roles: i32,
    ) -> *const c_void,
>;
/// Callback destroying the Rust context owned by a native COM adapter.
pub type Ara2Vst3DropCallback = Option<unsafe extern "C" fn(context: *mut c_void)>;

/// Callbacks retained by a native `IMainFactory` implementation.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Ara2Vst3MainFactoryCallbacks {
    /// Opaque callback context.
    pub context: *mut c_void,
    /// Factory lookup callback.
    pub get_factory: Ara2Vst3GetFactoryCallback,
    /// Final context destruction callback.
    pub drop: Ara2Vst3DropCallback,
}

/// Callbacks retained by a native entry-point implementation.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Ara2Vst3PluginEntryCallbacks {
    /// Opaque callback context.
    pub context: *mut c_void,
    /// Factory lookup callback.
    pub get_factory: Ara2Vst3GetFactoryCallback,
    /// Legacy and role-aware binding callback.
    pub bind: Ara2Vst3BindCallback,
    /// Final context destruction callback.
    pub drop: Ara2Vst3DropCallback,
}

unsafe extern "C" {
    /// Copies the canonical IID words for one supported interface.
    pub fn ara2_vst3_interface_id(
        kind: Ara2Vst3InterfaceKind,
        output: *mut Ara2Vst3InterfaceId,
    ) -> i32;
    /// Returns the static VST3 ARA main-factory class category.
    pub fn ara2_vst3_main_factory_category() -> *const c_char;
    /// Creates a reference-counted `IMainFactory` adapter with one owning reference.
    pub fn ara2_vst3_main_factory_create(
        callbacks: *const Ara2Vst3MainFactoryCallbacks,
        output: *mut *mut c_void,
    ) -> i32;
    /// Creates a reference-counted entry-point adapter with one owning reference.
    pub fn ara2_vst3_plugin_entry_create(
        callbacks: *const Ara2Vst3PluginEntryCallbacks,
        output: *mut *mut c_void,
    ) -> i32;
    /// Queries an interface and returns one owning reference on success.
    pub fn ara2_vst3_query_interface(
        unknown: *mut c_void,
        kind: Ara2Vst3InterfaceKind,
        output: *mut *mut c_void,
    ) -> i32;
    /// Adds one COM reference and returns the resulting count.
    pub fn ara2_vst3_add_ref(unknown: *mut c_void, output: *mut u32) -> i32;
    /// Releases one COM reference and returns the resulting count.
    pub fn ara2_vst3_release(unknown: *mut c_void, output: *mut u32) -> i32;
    /// Reads the factory pointer from an `IMainFactory`-capable object.
    pub fn ara2_vst3_main_factory_get_factory(
        unknown: *mut c_void,
        output: *mut *const c_void,
    ) -> i32;
    /// Reads the factory pointer from an entry-point-capable object.
    pub fn ara2_vst3_plugin_entry_get_factory(
        unknown: *mut c_void,
        output: *mut *const c_void,
    ) -> i32;
    /// Invokes legacy or role-aware entry-point binding.
    pub fn ara2_vst3_plugin_entry_bind(
        unknown: *mut c_void,
        document_controller: *mut c_void,
        known_roles: i32,
        assigned_roles: i32,
        use_role_aware_entry: u8,
        output: *mut *const c_void,
    ) -> i32;
    /// Verifies that a deliberately thrown C++ exception is translated to a result code.
    pub fn ara2_vst3_probe_exception_boundary(throw_exception: u8) -> i32;
}
