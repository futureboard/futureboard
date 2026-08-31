//! Common validation for generated calls into a foreign controller vtable.

use super::DispatchMethod;
use ara2_bridge_core::AraError;
use ara2_bridge_sys::{access::read_field, ARADocumentControllerInterface, ARASize};
use std::mem::size_of;
use std::ptr::NonNull;

/// Reads one represented, non-null callback without creating a packed-field reference.
///
/// # Safety
///
/// `interface` must either be null or point to a foreign interface whose advertised prefix stays
/// readable for the call. `offset` and `extent` must describe an `Option<T>` field of that type.
pub(crate) unsafe fn callback<T: Copy>(
    interface: *const ARADocumentControllerInterface,
    extent: usize,
    offset: usize,
    name: &'static str,
) -> Result<T, AraError> {
    let interface = NonNull::new(interface.cast_mut())
        .ok_or(AraError::Abi("null document-controller interface"))?;
    let base = interface.as_ptr().cast::<u8>();
    // SAFETY: the caller guarantees that the interface header is readable.
    let struct_size = unsafe { read_field::<ARASize>(base, 0) };
    if struct_size < extent || offset + size_of::<Option<T>>() > struct_size {
        return Err(AraError::Unsupported(name));
    }
    // SAFETY: the represented-prefix check proves the entire packed callback field is readable.
    unsafe { read_field::<Option<T>>(base, offset) }.ok_or(AraError::Abi(name))
}

/// Returns whether a generated callback slot is represented and non-null.
///
/// # Safety
///
/// `interface` must point to a foreign interface whose advertised prefix remains readable.
pub(crate) unsafe fn slot_present(
    interface: *const ARADocumentControllerInterface,
    method: DispatchMethod,
) -> Result<bool, AraError> {
    let interface = NonNull::new(interface.cast_mut())
        .ok_or(AraError::Abi("null document-controller interface"))?;
    let base = interface.as_ptr().cast::<u8>();
    // SAFETY: the caller guarantees that the interface header is readable.
    let struct_size = unsafe { read_field::<ARASize>(base, 0) };
    if struct_size < method.field_extent {
        return Ok(false);
    }
    // Function pointers have one pointer-sized nullable representation for all ARA callback
    // signatures. The value is inspected for presence only and is never invoked through this type.
    type ErasedCallback = unsafe extern "C" fn();
    // SAFETY: the generated offset/extent identifies one fully represented callback field.
    Ok(unsafe { read_field::<Option<ErasedCallback>>(base, method.field_offset) }.is_some())
}

/// Returns the advertised byte size of a foreign controller interface.
///
/// # Safety
///
/// `interface` must point to a readable interface header.
pub(crate) unsafe fn interface_size(
    interface: *const ARADocumentControllerInterface,
) -> Result<usize, AraError> {
    let interface = NonNull::new(interface.cast_mut())
        .ok_or(AraError::Abi("null document-controller interface"))?;
    // SAFETY: the caller guarantees that the interface header is readable.
    Ok(unsafe { read_field::<ARASize>(interface.as_ptr().cast(), 0) })
}
