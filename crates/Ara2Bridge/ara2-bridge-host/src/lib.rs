//! Safe authoring runtime for ARA hosts.
//!
//! # Role and boundaries
//!
//! This crate builds host-service vtables, loads ARA factories, owns document graphs, scopes edits,
//! and coordinates plug-in extension roles. It depends on core validation and the raw ABI but not on
//! the plug-in authoring runtime or testkit. Sessions, handles, and builders have **No direct C
//! counterpart**; they support ARA host interfaces and `ARADocumentControllerInterface` dispatch.
//!
//! # Lifecycle and threading
//!
//! [`HostServices`] outlives every controller using its callbacks. [`DocumentSession`] owns all
//! graph handles; mutation requires its [`EditSession`]. Close leaf objects before the controller
//! and handle [`CloseFailure`] explicitly. Model callbacks run under the model-thread contract;
//! audio reads follow the source/reader thread rules and may be realtime only when the provider is.
//!
//! # Features and platforms
//!
//! The runtime has no format feature or SDK dependency. Companion-format discovery belongs to
//! `ara2-bridge-companion`; target ABI rules determine available ARA generations.
//!
//! # Compatibility and licensing
//!
//! The crate targets Rust 1.82 and ARA through 2.3 Final. It is MIT OR Apache-2.0 and retains
//! Apache-2.0 provenance for SDK-derived dispatch behavior.
//!
//! # Example
//!
//! ```
//! use ara2_bridge_core::ApiGeneration;
//! use ara2_bridge_host::HostServicesBuilder;
//!
//! // Audio access and archiving are required, so an empty builder fails before FFI.
//! assert!(HostServicesBuilder::new().build(ApiGeneration::V23Final).is_err());
//! ```
//!
//! See the workspace host specification and the upstream
//! [ARA API](https://github.com/Celemony/ARA_API).

#![deny(missing_docs)]
#![deny(unsafe_op_in_unsafe_fn)]
#![deny(clippy::missing_safety_doc)]
#![deny(clippy::undocumented_unsafe_blocks)]

mod document;
mod extension;
mod plugin;
mod services;

pub use document::{
    AudioModificationHandle, AudioSourceHandle, CloseError, CloseFailure, DocumentSession,
    EditSession, MusicalContextHandle, PlaybackRegionHandle, PluginContentReaderBackend,
    RegionSequenceHandle, StoredAudioFileChunk,
};
pub use extension::{
    ExtensionController, ExtensionRoles, PlaybackRegionAssignment, RegionSequenceAssignment,
    RendererRole,
};
pub use plugin::{
    dispatch_manifest, DispatchMethod, DocumentController, FactoryMetadata, LoadedFactory,
};
pub use services::{
    ArchiveReaderId, ArchiveWriterId, ArchivingProvider, AudioAccessProvider, AudioModificationId,
    AudioSourceId, ContentAccessProvider, HostAudioReader, HostContentReaderSnapshot,
    HostContentSnapshot, HostServices, HostServicesBuilder, ModelUpdateProvider, MusicalContextId,
    PlaybackProvider, PlaybackRegionId,
};

/// Returns host callback names with implemented dispatch and contract coverage.
pub const fn host_callback_manifest() -> &'static [&'static str] {
    &[
        "createAudioReaderForSource",
        "readAudioSamples",
        "destroyAudioReader",
        "getArchiveSize",
        "readBytesFromArchive",
        "writeBytesToArchive",
        "notifyDocumentArchivingProgress",
        "notifyDocumentUnarchivingProgress",
        "getDocumentArchiveID",
        "notifyAudioSourceAnalysisProgress",
        "notifyAudioSourceContentChanged",
        "notifyAudioModificationContentChanged",
        "notifyPlaybackRegionContentChanged",
        "notifyDocumentDataChanged",
        "requestStartPlayback",
        "requestStopPlayback",
        "requestSetPlaybackPosition",
        "requestSetCycleRange",
        "requestEnableCycle",
        "isMusicalContextContentAvailable",
        "getMusicalContextContentGrade",
        "createMusicalContextContentReader",
        "isAudioSourceContentAvailable",
        "getAudioSourceContentGrade",
        "createAudioSourceContentReader",
        "getContentReaderEventCount",
        "getContentReaderDataForEvent",
        "destroyContentReader",
    ]
}
