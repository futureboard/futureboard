//! Host-side checked views of VST3 ARA COM interfaces.

use super::ffi::{
    ara2_vst3_plugin_entry_bind, ara2_vst3_plugin_entry_get_factory, ara2_vst3_query_interface,
    ara2_vst3_release, Ara2Vst3InterfaceKind, ARA2_VST3_OK,
};
use crate::CompanionRoles;
use ara2_bridge_core::{AraError, ForeignStr};
use ara2_bridge_sys::{ARADocumentControllerRef, ARAFactory, ARAPlugInExtensionInstance};
use std::cell::Cell;
use std::ffi::c_void;
use std::marker::PhantomData;
use std::ptr::NonNull;

const MAXIMUM_VST3_CLASS_NAME_BYTES: usize = 63;
const MAXIMUM_ARA_DISPLAY_BYTES: usize = 16 * 1024;

/// Opaque 16-byte VST3 class identifier copied from `PClassInfo.cid`.
pub type Vst3ClassId = [u8; 16];

fn validate_class_name(name: &str) -> Result<(), AraError> {
    if name.is_empty() || name.len() > MAXIMUM_VST3_CLASS_NAME_BYTES || name.as_bytes().contains(&0)
    {
        Err(AraError::InvalidArgument(
            "VST3 class name must fit the non-empty PClassInfo name field",
        ))
    } else {
        Ok(())
    }
}

/// Discovered VST3 ARA main-factory class metadata.
#[derive(Clone, Debug)]
pub struct Vst3AraMainClass {
    class_id: Vst3ClassId,
    class_name: String,
    plug_in_name: String,
    factory: NonNull<ARAFactory>,
}

impl Vst3AraMainClass {
    /// Copies and validates one class registration and its returned ARA factory.
    ///
    /// # Safety
    ///
    /// `factory` must be a readable live ARA factory whose advertised prefix includes
    /// `plugInName`; that NUL-terminated display string must remain readable during this call.
    pub unsafe fn from_raw(
        class_id: Vst3ClassId,
        class_name: impl Into<String>,
        factory: *const ARAFactory,
    ) -> Result<Self, AraError> {
        let class_name = class_name.into();
        validate_class_name(&class_name)?;
        let factory = NonNull::new(factory.cast_mut())
            .ok_or(AraError::InvalidArgument("null VST3 ARA factory"))?;
        // SAFETY: caller guarantees a readable factory base record.
        let struct_size =
            unsafe { std::ptr::addr_of!((*factory.as_ptr()).structSize).read_unaligned() };
        if struct_size
            < std::mem::offset_of!(ARAFactory, plugInName)
                + std::mem::size_of::<ara2_bridge_sys::ARAUtf8String>()
        {
            return Err(AraError::Abi(
                "VST3 ARA factory prefix does not include plugInName",
            ));
        }
        // SAFETY: checked the complete field in caller-guaranteed readable backing.
        let pointer =
            unsafe { std::ptr::addr_of!((*factory.as_ptr()).plugInName).read_unaligned() };
        // SAFETY: caller forwards the bounded ARA factory display-string contract.
        let plug_in_name =
            unsafe { ForeignStr::copy_display(pointer, MAXIMUM_ARA_DISPLAY_BYTES)? }.into_string();
        validate_class_name(&plug_in_name)?;
        Ok(Self {
            class_id,
            class_name,
            plug_in_name,
            factory,
        })
    }

    /// Returns the VST3 main-factory class ID.
    pub const fn class_id(&self) -> Vst3ClassId {
        self.class_id
    }

    /// Returns the VST3 main-factory class name.
    pub fn class_name(&self) -> &str {
        &self.class_name
    }

    /// Returns the copied `ARAFactory.plugInName`.
    pub fn plug_in_name(&self) -> &str {
        &self.plug_in_name
    }

    /// Returns the stable factory pointer.
    pub const fn factory(&self) -> *const ARAFactory {
        self.factory.as_ptr()
    }
}

/// Discovered VST3 audio-processor class metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Vst3ProcessorClass {
    class_id: Vst3ClassId,
    class_name: String,
}

impl Vst3ProcessorClass {
    /// Copies a `kVstAudioEffectClass` registration.
    pub fn new(class_id: Vst3ClassId, class_name: impl Into<String>) -> Result<Self, AraError> {
        let class_name = class_name.into();
        validate_class_name(&class_name)?;
        Ok(Self {
            class_id,
            class_name,
        })
    }

    /// Returns the processor class ID.
    pub const fn class_id(&self) -> Vst3ClassId {
        self.class_id
    }

    /// Returns the processor `PClassInfo.name`.
    pub fn class_name(&self) -> &str {
        &self.class_name
    }
}

/// Unambiguous main-factory ↔ processor association.
#[derive(Clone, Debug)]
pub struct Vst3ClassMatch {
    /// Matched main-factory metadata.
    pub main_factory: Vst3AraMainClass,
    /// Matched audio-processor metadata.
    pub processor: Vst3ProcessorClass,
}

/// Validates unique class IDs/names/factories and matches every ARA class by exact name.
pub fn match_vst3_classes(
    main_factories: impl IntoIterator<Item = Vst3AraMainClass>,
    processors: impl IntoIterator<Item = Vst3ProcessorClass>,
) -> Result<Vec<Vst3ClassMatch>, AraError> {
    let main_factories = main_factories.into_iter().collect::<Vec<_>>();
    let processors = processors.into_iter().collect::<Vec<_>>();
    for (index, current) in main_factories.iter().enumerate() {
        if current.class_name != current.plug_in_name
            || main_factories[..index].iter().any(|other| {
                other.class_id == current.class_id
                    || other.class_name == current.class_name
                    || other.factory == current.factory
            })
        {
            return Err(AraError::Peer(
                "ambiguous or inconsistent VST3 ARA main-factory class",
            ));
        }
    }
    for (index, current) in processors.iter().enumerate() {
        if processors[..index].iter().any(|other| {
            other.class_id == current.class_id || other.class_name == current.class_name
        }) {
            return Err(AraError::Peer("ambiguous VST3 audio-processor class"));
        }
    }
    main_factories
        .into_iter()
        .map(|main_factory| {
            let mut matches = processors
                .iter()
                .filter(|processor| processor.class_name == main_factory.class_name);
            let processor = matches.next().cloned().ok_or(AraError::Peer(
                "VST3 ARA main factory has no matching processor",
            ))?;
            if matches.next().is_some() {
                return Err(AraError::Peer(
                    "VST3 ARA main factory has multiple matching processors",
                ));
            }
            Ok(Vst3ClassMatch {
                main_factory,
                processor,
            })
        })
        .collect()
}

fn query(
    unknown: *mut c_void,
    kind: Ara2Vst3InterfaceKind,
    message: &'static str,
) -> Result<NonNull<c_void>, AraError> {
    let mut output = std::ptr::null_mut();
    // SAFETY: caller supplies a live FUnknown and output is writable.
    let result = unsafe { ara2_vst3_query_interface(unknown, kind, &mut output) };
    if result != ARA2_VST3_OK {
        return Err(AraError::Unsupported(message));
    }
    NonNull::new(output).ok_or(AraError::Abi("VST3 query returned a null interface"))
}

fn release(interface: NonNull<c_void>) {
    let mut remaining = 0;
    // SAFETY: interface owns one reference returned by the native query boundary.
    let _ = unsafe { ara2_vst3_release(interface.as_ptr(), &mut remaining) };
}

/// Owning host view of one queried `ARA::IMainFactory` interface.
pub struct Vst3HostMainFactory<'object> {
    interface: NonNull<c_void>,
    _lifetime: PhantomData<&'object c_void>,
}

impl<'object> Vst3HostMainFactory<'object> {
    /// Queries `IMainFactory` on a live VST3 object.
    ///
    /// # Safety
    ///
    /// `unknown` must be a live VST3 `FUnknown` pointer for `'object`.
    pub unsafe fn discover(unknown: *mut c_void) -> Result<Self, AraError> {
        if unknown.is_null() {
            return Err(AraError::InvalidArgument("null VST3 object"));
        }
        Ok(Self {
            interface: query(
                unknown,
                Ara2Vst3InterfaceKind::MainFactory,
                "VST3 ARA main factory",
            )?,
            _lifetime: PhantomData,
        })
    }

    /// Returns the stable ARA factory exposed by this class instance.
    pub fn factory(&self) -> Result<*const ARAFactory, AraError> {
        let mut output = std::ptr::null();
        // SAFETY: discovery retains a queried IMainFactory reference.
        let result = unsafe {
            super::ffi::ara2_vst3_main_factory_get_factory(self.interface.as_ptr(), &mut output)
        };
        if result != ARA2_VST3_OK || output.is_null() {
            Err(AraError::Peer("VST3 main factory returned no ARA factory"))
        } else {
            Ok(output.cast())
        }
    }
}

impl Drop for Vst3HostMainFactory<'_> {
    fn drop(&mut self) {
        release(self.interface);
    }
}

/// Owning host view of a processor's ARA entry-point interfaces.
pub struct Vst3HostPlugin<'object> {
    interface: NonNull<c_void>,
    role_aware: bool,
    bound: Cell<bool>,
    _lifetime: PhantomData<&'object c_void>,
}

impl<'object> Vst3HostPlugin<'object> {
    /// Queries the required generation-1 entry and detects generation-2 support.
    ///
    /// # Safety
    ///
    /// `unknown` must be the live `FUnknown` identity of an initialized VST3 processor component.
    pub unsafe fn discover(unknown: *mut c_void) -> Result<Self, AraError> {
        if unknown.is_null() {
            return Err(AraError::InvalidArgument("null VST3 processor"));
        }
        let interface = query(
            unknown,
            Ara2Vst3InterfaceKind::PluginEntry,
            "VST3 ARA plug-in entry point",
        )?;
        let role_aware = match query(
            interface.as_ptr(),
            Ara2Vst3InterfaceKind::PluginEntry2,
            "VST3 ARA role-aware entry point",
        ) {
            Ok(entry2) => {
                release(entry2);
                true
            }
            Err(_) => false,
        };
        Ok(Self {
            interface,
            role_aware,
            bound: Cell::new(false),
            _lifetime: PhantomData,
        })
    }

    /// Returns whether `IPlugInEntryPoint2` is available.
    pub const fn supports_role_aware_binding(&self) -> bool {
        self.role_aware
    }

    /// Returns the processor's exact ARA factory.
    pub fn factory(&self) -> Result<*const ARAFactory, AraError> {
        let mut output = std::ptr::null();
        // SAFETY: discovery retains a live entry-point interface.
        let result =
            unsafe { ara2_vst3_plugin_entry_get_factory(self.interface.as_ptr(), &mut output) };
        if result != ARA2_VST3_OK || output.is_null() {
            Err(AraError::Peer("VST3 processor returned no ARA factory"))
        } else {
            Ok(output.cast())
        }
    }

    /// Binds once before activation/state/process-context/view boundaries.
    ///
    /// # Safety
    ///
    /// `controller` must belong to the exact factory returned by [`Self::factory`] and remain
    /// valid through the permitted controller/processor teardown ordering.
    pub unsafe fn bind(
        &self,
        controller: ARADocumentControllerRef,
        known_roles: CompanionRoles,
        assigned_roles: CompanionRoles,
        allow_legacy_fallback: bool,
    ) -> Result<*const ARAPlugInExtensionInstance, AraError> {
        if self.bound.get() {
            return Err(AraError::InvalidState(
                "VST3 processor is already ARA-bound",
            ));
        }
        if controller.is_null() || !known_roles.contains(assigned_roles) {
            return Err(AraError::InvalidArgument(
                "invalid VST3 ARA controller or role set",
            ));
        }
        let all_roles = CompanionRoles::all();
        let use_role_aware = self.role_aware;
        if !use_role_aware
            && (!allow_legacy_fallback || known_roles != all_roles || assigned_roles != all_roles)
        {
            return Err(AraError::Unsupported(
                "VST3 role-aware ARA binding is unavailable",
            ));
        }
        let mut output = std::ptr::null();
        // SAFETY: caller supplies the controller lifetime; discovery retained the entry object.
        let mut result = unsafe {
            ara2_vst3_plugin_entry_bind(
                self.interface.as_ptr(),
                controller.cast(),
                known_roles.bits(),
                assigned_roles.bits(),
                u8::from(use_role_aware),
                &mut output,
            )
        };
        if use_role_aware
            && result != ARA2_VST3_OK
            && allow_legacy_fallback
            && known_roles == all_roles
            && assigned_roles == all_roles
        {
            output = std::ptr::null();
            // SAFETY: same controller contract; this is the permitted generation-1 fallback.
            result = unsafe {
                ara2_vst3_plugin_entry_bind(
                    self.interface.as_ptr(),
                    controller.cast(),
                    known_roles.bits(),
                    assigned_roles.bits(),
                    0,
                    &mut output,
                )
            };
        }
        if result != ARA2_VST3_OK || output.is_null() {
            Err(AraError::Peer("VST3 processor rejected ARA binding"))
        } else {
            self.bound.set(true);
            Ok(output.cast())
        }
    }
}

impl Drop for Vst3HostPlugin<'_> {
    fn drop(&mut self) {
        release(self.interface);
    }
}
