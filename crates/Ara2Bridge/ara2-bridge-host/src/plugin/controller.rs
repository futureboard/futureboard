//! Validated ownership of one foreign ARA document-controller instance.

use super::{dispatch, generated_dispatch, LoadedFactory, DISPATCH_METHODS};
use crate::HostServices;
use ara2_bridge_core::{ApiGeneration, AraError, DocumentProperties};
use ara2_bridge_sys::{
    access::read_field, kARADocumentControllerInstanceMinSize, ARADocumentControllerInstance,
    ARADocumentControllerInterface, ARADocumentControllerRef, ARADocumentControllerRefMarkupType,
    ARAFactory, ARASize,
};
use std::marker::PhantomData;
use std::mem::offset_of;
use std::ptr::NonNull;

/// One validated foreign document controller tied to its factory and host services.
pub struct DocumentController<'factory, 'services> {
    controller_ref: NonNull<ARADocumentControllerRefMarkupType>,
    interface: NonNull<ARADocumentControllerInterface>,
    factory: *const ARAFactory,
    generation: ApiGeneration,
    destroyed: bool,
    _factory: PhantomData<&'factory ()>,
    _services: PhantomData<&'services HostServices>,
}

impl DocumentController<'_, '_> {
    /// Returns the selected API generation.
    pub fn generation(&self) -> ApiGeneration {
        self.generation
    }

    /// Returns the stable opaque plug-in controller reference.
    pub fn as_raw_ref(&self) -> ARADocumentControllerRef {
        self.controller_ref.as_ptr()
    }

    /// Returns the validated foreign controller interface.
    pub fn interface_ptr(&self) -> *const ARADocumentControllerInterface {
        self.interface.as_ptr()
    }

    /// Returns the factory identity reported by the controller.
    pub fn factory_ptr(&self) -> *const ARAFactory {
        self.factory
    }

    /// Returns the number of optional processing algorithms advertised by the controller.
    ///
    /// Controllers whose shorter prefix omits the complete processing-algorithm capability use
    /// the ARA compatibility fallback of zero algorithms.
    pub fn processing_algorithms_count(&mut self) -> Result<usize, AraError> {
        // SAFETY: this controller owns a live validated interface for the duration of the call.
        if !unsafe { dispatch::slot_present(self.interface.as_ptr(), DISPATCH_METHODS[47])? } {
            return Ok(0);
        }
        // SAFETY: presence was checked above and the generated shell supplies the owned ref.
        let count = unsafe { self.raw_get_processing_algorithms_count()? };
        usize::try_from(count).map_err(|_| AraError::Peer("negative processing-algorithm count"))
    }
}

impl Drop for DocumentController<'_, '_> {
    fn drop(&mut self) {
        if !self.destroyed {
            // SAFETY: creation validated this required callback and the controller remains owned.
            let _ = unsafe {
                generated_dispatch::destroy_document_controller(
                    self.interface.as_ptr(),
                    self.controller_ref.as_ptr(),
                )
            };
            self.destroyed = true;
        }
    }
}

pub(crate) unsafe fn create<'factory, 'services>(
    factory: &'factory LoadedFactory<'factory>,
    services: &'services HostServices,
    properties: &DocumentProperties,
) -> Result<DocumentController<'factory, 'services>, AraError> {
    let ffi_properties = properties.as_ffi();
    // SAFETY: the factory callback was validated during loading; host services and property
    // backing remain live through this synchronous call.
    let instance = unsafe {
        (factory.create_callback())(services.instance_ptr(), ffi_properties.as_ref().as_ptr())
    };
    let instance = NonNull::new(instance.cast_mut())
        .ok_or(AraError::Abi("factory returned a null controller instance"))?;
    let base = instance.as_ptr().cast::<u8>();
    // SAFETY: the factory contract makes the returned instance header readable.
    let struct_size = unsafe { read_field::<ARASize>(base, 0) };
    if struct_size < kARADocumentControllerInstanceMinSize as usize {
        return Err(AraError::Abi("truncated document-controller instance"));
    }
    // SAFETY: the minimum instance prefix represents both required fields.
    let controller_ref = unsafe {
        read_field::<ARADocumentControllerRef>(
            base,
            offset_of!(ARADocumentControllerInstance, documentControllerRef),
        )
    };
    let controller_ref =
        NonNull::new(controller_ref).ok_or(AraError::Abi("null document-controller reference"))?;
    // SAFETY: the minimum instance prefix represents the interface pointer.
    let interface = unsafe {
        read_field::<*const ARADocumentControllerInterface>(
            base,
            offset_of!(ARADocumentControllerInstance, documentControllerInterface),
        )
    };
    let interface = NonNull::new(interface.cast_mut())
        .ok_or(AraError::Abi("null document-controller interface"))?;

    // SAFETY: factory/controller ownership keeps the returned interface backing live.
    if let Err(error) = unsafe {
        validate_interface(
            interface.as_ptr(),
            factory.generation(),
            factory.metadata().stores_audio_file_chunks(),
        )
    } {
        // SAFETY: cleanup is attempted only when the destroy slot itself is valid.
        let _ = unsafe {
            generated_dispatch::destroy_document_controller(
                interface.as_ptr(),
                controller_ref.as_ptr(),
            )
        };
        return Err(error);
    }
    // SAFETY: `validate_interface` proved the required identity callback is represented/non-null.
    let reported_factory =
        unsafe { generated_dispatch::get_factory(interface.as_ptr(), controller_ref.as_ptr())? };
    if !std::ptr::eq(reported_factory, factory.as_raw()) {
        // SAFETY: the validated required destroy callback balances the created controller.
        let _ = unsafe {
            generated_dispatch::destroy_document_controller(
                interface.as_ptr(),
                controller_ref.as_ptr(),
            )
        };
        return Err(AraError::Abi("controller reported a different factory"));
    }

    Ok(DocumentController {
        controller_ref,
        interface,
        factory: reported_factory,
        generation: factory.generation(),
        destroyed: false,
        _factory: PhantomData,
        _services: PhantomData,
    })
}

unsafe fn validate_interface(
    interface: *const ARADocumentControllerInterface,
    generation: ApiGeneration,
    requires_audio_file_chunks: bool,
) -> Result<(), AraError> {
    let required_count = match generation {
        ApiGeneration::V1Draft | ApiGeneration::V1Final => 41,
        ApiGeneration::V2Draft => 45,
        ApiGeneration::V2Final | ApiGeneration::V2xDraft | ApiGeneration::V23Final => 47,
    };
    // SAFETY: the factory guarantees the interface header stays readable.
    let interface_size = unsafe { dispatch::interface_size(interface)? };
    if interface_size < DISPATCH_METHODS[required_count - 1].field_extent {
        return Err(AraError::Abi("truncated required controller prefix"));
    }
    for method in DISPATCH_METHODS {
        if interface_size < method.field_extent {
            continue;
        }
        // SAFETY: the factory guarantees the interface backing remains live for the controller.
        if !unsafe { dispatch::slot_present(interface, *method)? } {
            return Err(AraError::Abi(method.c_name));
        }
    }
    let processing = &DISPATCH_METHODS[47..51];
    if interface_size >= processing[0].field_extent
        && interface_size < processing[processing.len() - 1].field_extent
    {
        return Err(AraError::Abi("partial processing-algorithm capability"));
    }
    if generation >= ApiGeneration::V2Final
        && requires_audio_file_chunks
        && interface_size < DISPATCH_METHODS[52].field_extent
    {
        return Err(AraError::Abi(
            "factory advertises unavailable audio-file chunk storage",
        ));
    }
    Ok(())
}
