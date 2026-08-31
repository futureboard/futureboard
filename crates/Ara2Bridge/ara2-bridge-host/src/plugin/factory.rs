use ara2_bridge_core::{ApiGeneration, AraBool, AraError, ForeignSlice, ForeignStr};
use ara2_bridge_sys::{
    access::read_field, kARAFactoryMinSize, ARAAPIGeneration, ARAAssertFunction, ARAContentType,
    ARAFactory, ARAInterfaceConfiguration, ARAPersistentID, ARAPlaybackTransformationFlags,
    ARASize, ARAUtf8String,
};
use std::collections::HashSet;
use std::ffi::{c_char, c_void};
use std::marker::PhantomData;
use std::mem::{offset_of, size_of};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};

const MAXIMUM_STRING_BYTES: usize = 16 * 1024;

struct SharedAssertion {
    storage: Box<ARAAssertFunction>,
    listeners: Vec<(u64, ARAAssertFunction)>,
}

struct AssertionLease {
    generation: ApiGeneration,
    listener_id: u64,
    pointer: NonNull<ARAAssertFunction>,
}

fn assertion_slots() -> &'static Mutex<Vec<Option<SharedAssertion>>> {
    static SLOTS: OnceLock<Mutex<Vec<Option<SharedAssertion>>>> = OnceLock::new();
    SLOTS.get_or_init(|| Mutex::new((0..6).map(|_| None).collect()))
}

fn lock_assertions() -> MutexGuard<'static, Vec<Option<SharedAssertion>>> {
    assertion_slots()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

unsafe extern "C" fn shared_assertion_trampoline(
    category: ara2_bridge_sys::ARAAssertCategory,
    problem: *const c_void,
    file: *const c_char,
) {
    let listeners = {
        let slots = lock_assertions();
        slots
            .iter()
            .flatten()
            .flat_map(|slot| slot.listeners.iter().filter_map(|(_, listener)| *listener))
            .collect::<Vec<_>>()
    };
    for listener in listeners {
        // SAFETY: each listener was supplied as an ARA assertion function and remains copied.
        unsafe { listener(category, problem, file) };
    }
}

impl AssertionLease {
    fn acquire(
        generation: ApiGeneration,
        assert_function: ARAAssertFunction,
    ) -> Result<Self, AraError> {
        static NEXT_LISTENER: AtomicU64 = AtomicU64::new(1);
        let listener_id = NEXT_LISTENER.fetch_add(1, Ordering::Relaxed);
        if listener_id == 0 {
            return Err(AraError::InvalidState("assertion listener ID overflow"));
        }
        let mut slots = lock_assertions();
        let slot = slots
            .get_mut(generation.as_raw() as usize - 1)
            .ok_or(AraError::Unsupported("assertion generation slot"))?;
        match slot {
            Some(shared) => {
                shared.listeners.push((listener_id, assert_function));
            }
            None => {
                *slot = Some(SharedAssertion {
                    storage: Box::new(Some(shared_assertion_trampoline)),
                    listeners: vec![(listener_id, assert_function)],
                });
            }
        }
        let shared = slot.as_mut().expect("assertion slot was installed");
        Ok(Self {
            generation,
            listener_id,
            pointer: NonNull::from(&mut *shared.storage),
        })
    }

    fn as_ptr(&self) -> *mut ARAAssertFunction {
        self.pointer.as_ptr()
    }
}

impl Drop for AssertionLease {
    fn drop(&mut self) {
        let mut slots = lock_assertions();
        let slot = &mut slots[self.generation.as_raw() as usize - 1];
        if let Some(shared) = slot {
            shared
                .listeners
                .retain(|(listener_id, _)| *listener_id != self.listener_id);
            if shared.listeners.is_empty() {
                *slot = None;
            }
        }
    }
}

/// Owned validated metadata copied from one foreign ARA factory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FactoryMetadata {
    factory_id: String,
    plug_in_name: String,
    manufacturer_name: String,
    information_url: String,
    version: String,
    document_archive_id: String,
    compatible_archive_ids: Vec<String>,
    analyzable_content_types: Vec<ARAContentType>,
    playback_transformations: ARAPlaybackTransformationFlags,
    stores_audio_file_chunks: bool,
}

impl FactoryMetadata {
    /// Returns the stable factory identifier.
    pub fn factory_id(&self) -> &str {
        &self.factory_id
    }
    /// Returns the human-readable plug-in name.
    pub fn plug_in_name(&self) -> &str {
        &self.plug_in_name
    }
    /// Returns the plug-in manufacturer name.
    pub fn manufacturer_name(&self) -> &str {
        &self.manufacturer_name
    }
    /// Returns the plug-in information URL.
    pub fn information_url(&self) -> &str {
        &self.information_url
    }
    /// Returns the plug-in version string.
    pub fn version(&self) -> &str {
        &self.version
    }
    /// Returns the primary document archive identifier.
    pub fn document_archive_id(&self) -> &str {
        &self.document_archive_id
    }
    /// Returns compatible document archive identifiers.
    pub fn compatible_archive_ids(&self) -> &[String] {
        &self.compatible_archive_ids
    }
    /// Returns content types the plug-in can analyze.
    pub fn analyzable_content_types(&self) -> &[ARAContentType] {
        &self.analyzable_content_types
    }
    /// Returns supported playback transformation flags.
    pub fn playback_transformations(&self) -> ARAPlaybackTransformationFlags {
        self.playback_transformations
    }
    /// Returns whether audio-file chunk storage is supported.
    pub fn stores_audio_file_chunks(&self) -> bool {
        self.stores_audio_file_chunks
    }
}

/// One initialized foreign ARA factory entry.
pub struct LoadedFactory<'factory> {
    raw: NonNull<ARAFactory>,
    generation: ApiGeneration,
    metadata: FactoryMetadata,
    assertion_lease: AssertionLease,
    create: unsafe extern "C" fn(
        *const ara2_bridge_sys::ARADocumentControllerHostInstance,
        *const ara2_bridge_sys::ARADocumentProperties,
    ) -> *const ara2_bridge_sys::ARADocumentControllerInstance,
    uninitialize: unsafe extern "C" fn(),
    _lifetime: PhantomData<&'factory ARAFactory>,
}

impl<'factory> LoadedFactory<'factory> {
    /// Validates and initializes a foreign factory for `generation`.
    ///
    /// # Safety
    ///
    /// `factory` and all metadata/count-pointer backing reachable from its advertised prefix must
    /// remain readable and unchanged until the returned object is dropped. The factory entry must
    /// not already be initialized, and its callbacks must obey the ARA C ABI.
    pub unsafe fn load(
        factory: *const ARAFactory,
        generation: ApiGeneration,
        assert_function: ARAAssertFunction,
    ) -> Result<Self, AraError> {
        let raw = NonNull::new(factory.cast_mut())
            .ok_or(AraError::InvalidArgument("null ARA factory"))?;
        let base = factory.cast::<u8>();
        // SAFETY: the caller guarantees the factory header is readable.
        let struct_size =
            unsafe { read_field::<ARASize>(base, offset_of!(ARAFactory, structSize)) };
        if struct_size < kARAFactoryMinSize as usize {
            return Err(AraError::Abi("truncated ARA factory"));
        }
        macro_rules! field {
            ($name:ident, $type:ty) => {{
                if offset_of!(ARAFactory, $name) + size_of::<$type>() > struct_size {
                    return Err(AraError::Abi("factory field outside advertised prefix"));
                }
                // SAFETY: the size check above proves the complete packed field is represented.
                unsafe { read_field::<$type>(base, offset_of!(ARAFactory, $name)) }
            }};
        }
        let lowest =
            ApiGeneration::try_from_raw(field!(lowestSupportedApiGeneration, ARAAPIGeneration))?;
        let highest =
            ApiGeneration::try_from_raw(field!(highestSupportedApiGeneration, ARAAPIGeneration))?;
        if lowest > highest
            || generation < lowest
            || generation > highest
            || !generation.supported_on_target()
        {
            return Err(AraError::Unsupported("factory API generation"));
        }
        let initialize = field!(
            initializeARAWithConfiguration,
            Option<unsafe extern "C" fn(*const ARAInterfaceConfiguration)>
        )
        .ok_or(AraError::Abi("factory initialize callback is null"))?;
        let uninitialize = field!(uninitializeARA, Option<unsafe extern "C" fn()>)
            .ok_or(AraError::Abi("factory uninitialize callback is null"))?;
        let create = field!(
            createDocumentControllerWithDocument,
            Option<
                unsafe extern "C" fn(
                    *const ara2_bridge_sys::ARADocumentControllerHostInstance,
                    *const ara2_bridge_sys::ARADocumentProperties,
                )
                    -> *const ara2_bridge_sys::ARADocumentControllerInstance,
            >
        )
        .ok_or(AraError::Abi("factory create-controller callback is null"))?;

        // SAFETY: all string and array backing is covered by the caller's factory lifetime.
        let metadata = unsafe { copy_metadata(base, struct_size)? };
        let assertion_lease = AssertionLease::acquire(generation, assert_function)?;
        let configuration = ARAInterfaceConfiguration {
            structSize: size_of::<ARAInterfaceConfiguration>(),
            desiredApiGeneration: generation.as_raw(),
            assertFunctionAddress: assertion_lease.as_ptr(),
        };
        // SAFETY: validated callback and complete configuration retained for the call.
        unsafe { initialize(&configuration) };
        Ok(Self {
            raw,
            generation,
            metadata,
            assertion_lease,
            create,
            uninitialize,
            _lifetime: PhantomData,
        })
    }

    /// Returns the selected API generation.
    pub fn generation(&self) -> ApiGeneration {
        self.generation
    }
    /// Returns copied factory metadata.
    pub fn metadata(&self) -> &FactoryMetadata {
        &self.metadata
    }
    /// Returns the raw factory pointer for version-aware controller creation.
    pub fn as_raw(&self) -> *const ARAFactory {
        self.raw.as_ptr()
    }

    /// Creates and validates one document controller owned by the returned guard.
    pub fn create_document_controller<'loaded, 'services>(
        &'loaded self,
        services: &'services crate::HostServices,
        properties: &ara2_bridge_core::DocumentProperties,
    ) -> Result<super::DocumentController<'loaded, 'services>, AraError> {
        // SAFETY: `self` retains initialized factory backing and the returned guard borrows both
        // factory and host services for its entire foreign-controller lifetime.
        unsafe { super::controller::create(self, services, properties) }
    }

    pub(crate) fn create_callback(
        &self,
    ) -> unsafe extern "C" fn(
        *const ara2_bridge_sys::ARADocumentControllerHostInstance,
        *const ara2_bridge_sys::ARADocumentProperties,
    ) -> *const ara2_bridge_sys::ARADocumentControllerInstance {
        self.create
    }
}

impl Drop for LoadedFactory<'_> {
    fn drop(&mut self) {
        let _keep_assert_storage_live = &self.assertion_lease;
        // SAFETY: `load` recorded one successful initialize call for this callback.
        unsafe { (self.uninitialize)() };
    }
}

unsafe fn copy_metadata(base: *const u8, struct_size: usize) -> Result<FactoryMetadata, AraError> {
    macro_rules! read {
        ($name:ident, $type:ty) => {{
            if offset_of!(ARAFactory, $name) + size_of::<$type>() > struct_size {
                return Err(AraError::Abi("factory metadata outside advertised prefix"));
            }
            // SAFETY: checked complete field in caller-validated factory backing.
            unsafe { read_field::<$type>(base, offset_of!(ARAFactory, $name)) }
        }};
    }
    let factory_id_pointer = read!(factoryID, ARAPersistentID);
    // SAFETY: factory lifetime contract includes bounded NUL-terminated string backing.
    let factory_id =
        unsafe { ForeignStr::copy_persistent_id(factory_id_pointer, MAXIMUM_STRING_BYTES)? }
            .into_string();
    if factory_id.is_empty() {
        return Err(AraError::Abi("empty factory ID"));
    }
    let plug_in_name_pointer = read!(plugInName, ARAUtf8String);
    let manufacturer_name_pointer = read!(manufacturerName, ARAUtf8String);
    let information_url_pointer = read!(informationURL, ARAUtf8String);
    let version_pointer = read!(version, ARAUtf8String);
    // SAFETY: all four pointers use the bounded factory display-string lifetime contract.
    let plug_in_name = unsafe { copy_display(plug_in_name_pointer)? };
    // SAFETY: same display-string contract.
    let manufacturer_name = unsafe { copy_display(manufacturer_name_pointer)? };
    // SAFETY: same display-string contract.
    let information_url = unsafe { copy_display(information_url_pointer)? };
    // SAFETY: same display-string contract.
    let version = unsafe { copy_display(version_pointer)? };
    let document_archive_id_pointer = read!(documentArchiveID, ARAPersistentID);
    // SAFETY: same primary archive ID contract.
    let document_archive_id = unsafe {
        ForeignStr::copy_persistent_id(document_archive_id_pointer, MAXIMUM_STRING_BYTES)?
    }
    .into_string();
    if document_archive_id.is_empty() {
        return Err(AraError::Abi("empty document archive ID"));
    }
    let compatible_count = read!(compatibleDocumentArchiveIDsCount, ARASize);
    let compatible_pointer = read!(compatibleDocumentArchiveIDs, *const ARAPersistentID);
    // SAFETY: factory count-pointer pair is readable for the factory lifetime.
    let compatible = unsafe { ForeignSlice::copy_from_raw(compatible_pointer, compatible_count)? };
    let mut compatible_archive_ids = Vec::with_capacity(compatible.as_slice().len());
    for pointer in compatible.into_vec() {
        // SAFETY: each advertised ID pointer has the same bounded factory lifetime.
        compatible_archive_ids.push(
            unsafe { ForeignStr::copy_persistent_id(pointer, MAXIMUM_STRING_BYTES)? }.into_string(),
        );
    }
    let mut unique_archive_ids = HashSet::new();
    if compatible_archive_ids
        .iter()
        .any(|id| id.is_empty() || !unique_archive_ids.insert(id.as_str()))
    {
        return Err(AraError::Abi("invalid compatible document archive IDs"));
    }
    let content_count = read!(analyzeableContentTypesCount, ARASize);
    let content_pointer = read!(analyzeableContentTypes, *const ARAContentType);
    // SAFETY: factory count-pointer pair is readable for the factory lifetime.
    let analyzable_content_types =
        unsafe { ForeignSlice::copy_from_raw(content_pointer, content_count)? }.into_vec();
    let known_content_types = [
        ara2_bridge_sys::kARAContentTypeTempoEntries as ARAContentType,
        ara2_bridge_sys::kARAContentTypeBarSignatures as ARAContentType,
        ara2_bridge_sys::kARAContentTypeNotes as ARAContentType,
        ara2_bridge_sys::kARAContentTypeStaticTuning as ARAContentType,
        ara2_bridge_sys::kARAContentTypeKeySignatures as ARAContentType,
        ara2_bridge_sys::kARAContentTypeSheetChords as ARAContentType,
    ];
    let mut unique_content_types = HashSet::new();
    if analyzable_content_types.iter().any(|content_type| {
        !known_content_types.contains(content_type) || !unique_content_types.insert(*content_type)
    }) {
        return Err(AraError::Abi("invalid analyzable content types"));
    }
    let playback_transformations = read!(
        supportedPlaybackTransformationFlags,
        ARAPlaybackTransformationFlags
    );
    let known_transformations = (ara2_bridge_sys::kARAPlaybackTransformationContentBasedFades
        | ara2_bridge_sys::kARAPlaybackTransformationTimestretch
        | ara2_bridge_sys::kARAPlaybackTransformationTimestretchReflectingTempo)
        as ARAPlaybackTransformationFlags;
    if playback_transformations & !known_transformations != 0 {
        return Err(AraError::Abi("unknown playback transformation flags"));
    }
    let stores_audio_file_chunks =
        if offset_of!(ARAFactory, supportsStoringAudioFileChunks) + size_of::<i32>() <= struct_size
        {
            // SAFETY: optional tail is completely represented.
            AraBool::from_raw(unsafe {
                read_field(base, offset_of!(ARAFactory, supportsStoringAudioFileChunks))
            })
            .get()
        } else {
            false
        };
    Ok(FactoryMetadata {
        factory_id,
        plug_in_name,
        manufacturer_name,
        information_url,
        version,
        document_archive_id,
        compatible_archive_ids,
        analyzable_content_types,
        playback_transformations,
        stores_audio_file_chunks,
    })
}

unsafe fn copy_display(pointer: ARAUtf8String) -> Result<String, AraError> {
    // SAFETY: the factory lifetime contract includes bounded NUL-terminated display backing.
    Ok(unsafe { ForeignStr::copy_display(pointer, MAXIMUM_STRING_BYTES)? }.into_string())
}
