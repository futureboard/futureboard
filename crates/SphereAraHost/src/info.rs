//! Plug-in-side descriptors, copied out of the ARA factory into owned data.

use crate::model::AraPlaybackTransform;

/// Everything the host needs to know about an ARA plug-in before it opens a
/// document, copied out of `ARAFactory` so no foreign pointer escapes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AraFactoryInfo {
    /// Globally unique factory identifier.
    pub factory_id: String,
    /// Display name.
    pub plug_in_name: String,
    /// Vendor name.
    pub manufacturer_name: String,
    /// Version string.
    pub version: String,
    /// Informational URL.
    pub information_url: String,
    /// Identifier written into archives this plug-in produces.
    ///
    /// A stored archive may only be handed back to a plug-in whose
    /// [`Self::document_archive_id`] matches, or that lists the stored id in
    /// [`Self::compatible_archive_ids`].
    pub document_archive_id: String,
    /// Older archive identifiers this plug-in can still read.
    pub compatible_archive_ids: Vec<String>,
    /// Playback transformations the plug-in can perform.
    pub supported_transforms: AraPlaybackTransform,
    /// Whether the plug-in stores its analysis in audio-file chunks.
    pub stores_audio_file_chunks: bool,
    /// Negotiated ARA API generation, as the raw ARA value.
    pub api_generation: i32,
}

impl AraFactoryInfo {
    /// Whether an archive written under `archive_id` can be restored into this
    /// plug-in.
    pub fn can_restore_archive(&self, archive_id: &str) -> bool {
        self.document_archive_id == archive_id
            || self
                .compatible_archive_ids
                .iter()
                .any(|candidate| candidate == archive_id)
    }
}

/// Roles a bound plug-in instance takes on for one ARA document.
///
/// A clip editor binds all three; a pure playback voice binds only
/// [`AraRoles::PLAYBACK_RENDERER`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AraRoles {
    /// Renders assigned playback regions during transport playback.
    pub playback_renderer: bool,
    /// Renders preview audio for the plug-in's own editor.
    pub editor_renderer: bool,
    /// Receives selection and visibility hints for the editor.
    pub editor_view: bool,
}

impl AraRoles {
    /// Playback rendering only.
    pub const PLAYBACK_RENDERER: Self = Self {
        playback_renderer: true,
        editor_renderer: false,
        editor_view: false,
    };

    /// Every role, which is what a clip editor instance needs.
    pub const ALL: Self = Self {
        playback_renderer: true,
        editor_renderer: true,
        editor_view: true,
    };

    /// Whether no role at all is selected.
    pub fn is_empty(self) -> bool {
        !self.playback_renderer && !self.editor_renderer && !self.editor_view
    }
}

/// Identifies one bound plug-in instance inside a session.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AraRendererId(pub(crate) u64);

impl AraRendererId {
    /// Raw value, for logging and for carrying the id through project state.
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_compatibility_accepts_own_and_listed_ids() {
        let info = AraFactoryInfo {
            document_archive_id: "com.example.v2".into(),
            compatible_archive_ids: vec!["com.example.v1".into()],
            ..AraFactoryInfo::default()
        };
        assert!(info.can_restore_archive("com.example.v2"));
        assert!(info.can_restore_archive("com.example.v1"));
        assert!(!info.can_restore_archive("com.other.v1"));
    }
}
