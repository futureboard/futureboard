//! CLAP host-side ARA discovery and binding.

use super::sys::{
    ClapAraFactory, ClapAraPluginExtension, ClapPlugin, CLAP_EXT_ARA_PLUGIN_EXTENSION,
    CLAP_EXT_ARA_PLUGIN_EXTENSION_COMPAT,
};
use crate::CompanionRoles;
use ara2_bridge_core::{AraError, ForeignStr};
use ara2_bridge_sys::{ARADocumentControllerRef, ARAFactory, ARAPlugInExtensionInstance};
use std::cell::Cell;
use std::ffi::CString;
use std::marker::PhantomData;
use std::ptr::NonNull;

const MAXIMUM_ID_BYTES: usize = 16 * 1024;
const MAXIMUM_FACTORIES: usize = 65_536;

/// Owned copy of one CLAP plug-in ↔ ARA factory association.
#[derive(Clone, Debug)]
pub struct DiscoveredClapFactory {
    plugin_id: String,
    ara_factory: NonNull<ARAFactory>,
}

impl DiscoveredClapFactory {
    /// Returns the associated CLAP descriptor ID.
    pub fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    /// Returns the stable ARA factory pointer published by the CLAP entry.
    pub const fn ara_factory(&self) -> *const ARAFactory {
        self.ara_factory.as_ptr()
    }
}

/// Checked host view of a CLAP entry-level ARA factory extension.
pub struct ClapAraHostFactory<'entry> {
    interface: NonNull<ClapAraFactory>,
    count: usize,
    _lifetime: PhantomData<&'entry ClapAraFactory>,
}

impl<'entry> ClapAraHostFactory<'entry> {
    /// Validates a CLAP ARA factory extension returned by a live entry.
    ///
    /// # Safety
    ///
    /// `interface` must be readable and remain live for `'entry`; its callbacks and returned
    /// factory/ID backing must satisfy `ARACLAP.h` through entry deinitialization.
    pub unsafe fn discover(interface: *const ClapAraFactory) -> Result<Self, AraError> {
        let interface = NonNull::new(interface.cast_mut())
            .ok_or(AraError::Abi("null CLAP ARA factory extension"))?;
        // SAFETY: caller guarantees one readable complete interface record.
        let value = unsafe { interface.as_ref() };
        let count = value
            .get_factory_count
            .ok_or(AraError::Abi("null CLAP ARA factory-count callback"))?;
        if value.get_ara_factory.is_none() || value.get_plugin_id.is_none() {
            return Err(AraError::Abi("incomplete CLAP ARA factory extension"));
        }
        // SAFETY: validated callback receives its originating live interface.
        let count = unsafe { count(interface.as_ptr()) } as usize;
        if count > MAXIMUM_FACTORIES {
            return Err(AraError::Peer(
                "CLAP ARA factory count exceeds safety bound",
            ));
        }
        Ok(Self {
            interface,
            count,
            _lifetime: PhantomData,
        })
    }

    /// Returns the number of ARA-capable CLAP associations.
    pub const fn factory_count(&self) -> usize {
        self.count
    }

    /// Copies one association by index.
    pub fn factory(&self, index: usize) -> Result<DiscoveredClapFactory, AraError> {
        if index >= self.count {
            return Err(AraError::InvalidArgument(
                "CLAP ARA factory index is out of bounds",
            ));
        }
        // SAFETY: constructor validated both callbacks and index bounds.
        let interface = unsafe { self.interface.as_ref() };
        // SAFETY: same validated interface/index contract.
        let factory = unsafe {
            interface.get_ara_factory.expect("validated")(self.interface.as_ptr(), index as u32)
        };
        let ara_factory = NonNull::new(factory.cast_mut())
            .ok_or(AraError::Peer("CLAP returned a null ARA factory"))?;
        // SAFETY: same validated interface/index contract.
        let plugin_id = unsafe {
            interface.get_plugin_id.expect("validated")(self.interface.as_ptr(), index as u32)
        };
        // SAFETY: CLAP retains the association ID through entry deinitialization; copy now.
        let plugin_id =
            unsafe { ForeignStr::copy_persistent_id(plugin_id, MAXIMUM_ID_BYTES)? }.into_string();
        Ok(DiscoveredClapFactory {
            plugin_id,
            ara_factory,
        })
    }
}

/// Checked host view of one CLAP instance-level ARA extension.
pub struct ClapAraHostPlugin<'plugin> {
    plugin: NonNull<ClapPlugin>,
    interface: NonNull<ClapAraPluginExtension>,
    bound: Cell<bool>,
    _lifetime: PhantomData<&'plugin ClapPlugin>,
}

impl<'plugin> ClapAraHostPlugin<'plugin> {
    /// Discovers the stable or latest compatible draft ARA extension on a live CLAP plug-in.
    ///
    /// # Safety
    ///
    /// `plugin` must be initialized, readable, and live for `'plugin`. Its `get_extension`
    /// callback and any returned ARA extension must obey the CLAP thread/lifetime contract.
    pub unsafe fn discover(plugin: *const ClapPlugin) -> Result<Self, AraError> {
        let plugin = NonNull::new(plugin.cast_mut())
            .ok_or(AraError::InvalidArgument("null CLAP plug-in"))?;
        // SAFETY: caller supplies a readable initialized CLAP record.
        let get_extension = unsafe { plugin.as_ref() }
            .get_extension
            .ok_or(AraError::Abi("CLAP plug-in has no extension callback"))?;
        let stable = CString::new(CLAP_EXT_ARA_PLUGIN_EXTENSION).expect("static ID");
        // SAFETY: callback and ID are valid through this synchronous query.
        let mut interface = unsafe { get_extension(plugin.as_ptr(), stable.as_ptr()) };
        if interface.is_null() {
            let compat =
                CString::new(CLAP_EXT_ARA_PLUGIN_EXTENSION_COMPAT).expect("static compat ID");
            // SAFETY: same callback/query contract.
            interface = unsafe { get_extension(plugin.as_ptr(), compat.as_ptr()) };
        }
        let interface = NonNull::new(interface.cast_mut().cast::<ClapAraPluginExtension>())
            .ok_or(AraError::Unsupported("CLAP ARA plug-in extension"))?;
        // SAFETY: CLAP extension query publishes a readable complete record.
        let value = unsafe { interface.as_ref() };
        if value.get_factory.is_none() || value.bind_to_document_controller.is_none() {
            return Err(AraError::Abi("incomplete CLAP ARA plug-in extension"));
        }
        Ok(Self {
            plugin,
            interface,
            bound: Cell::new(false),
            _lifetime: PhantomData,
        })
    }

    /// Returns the exact ARA factory associated with this CLAP instance.
    pub fn factory(&self) -> Result<*const ARAFactory, AraError> {
        // SAFETY: discovery validated the callback and retained both lifetimes.
        let factory = unsafe {
            self.interface.as_ref().get_factory.expect("validated")(self.plugin.as_ptr())
        };
        if factory.is_null() {
            Err(AraError::Peer("CLAP instance returned a null ARA factory"))
        } else {
            Ok(factory)
        }
    }

    /// Binds the CLAP instance to one controller exactly once.
    ///
    /// # Safety
    ///
    /// The controller and returned extension instance must remain valid according to the
    /// companion/controller teardown contract. `controller` must belong to this instance's exact
    /// factory and binding must precede all CLAP processor boundaries.
    pub unsafe fn bind(
        &self,
        controller: ARADocumentControllerRef,
        known_roles: CompanionRoles,
        assigned_roles: CompanionRoles,
    ) -> Result<*const ARAPlugInExtensionInstance, AraError> {
        if self.bound.get() {
            return Err(AraError::InvalidState("CLAP instance is already ARA-bound"));
        }
        if controller.is_null() || !known_roles.contains(assigned_roles) {
            return Err(AraError::InvalidArgument(
                "invalid CLAP ARA controller or role set",
            ));
        }
        // SAFETY: caller forwards controller lifetime/identity; discovery validated callback.
        let extension = unsafe {
            self.interface
                .as_ref()
                .bind_to_document_controller
                .expect("validated")(
                self.plugin.as_ptr(),
                controller,
                known_roles.bits(),
                assigned_roles.bits(),
            )
        };
        if extension.is_null() {
            Err(AraError::Peer("CLAP instance rejected ARA binding"))
        } else {
            self.bound.set(true);
            Ok(extension)
        }
    }
}
