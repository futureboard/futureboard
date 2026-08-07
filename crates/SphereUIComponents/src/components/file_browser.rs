//! File browser state + lazy directory index for the left sidebar.
//!
//! Mirrors the Electron browser's IPC model: a long-lived process owns the
//! filesystem access and the UI reads from a cache. Here:
//!
//! * `FileBrowserState` holds **state only** — expand/select sets, drive
//!   roots, and an [`IndexCache`] of previously-loaded directory listings.
//! * `visible_nodes()` never touches the filesystem. It walks the cache
//!   and emits "Loading…" / "Error" placeholder rows for paths the
//!   indexer has not finished (or failed to) load.
//! * The actual `std::fs::read_dir` work runs on `gpui::BackgroundExecutor`
//!   from [`crate::layout`] — the UI thread is never blocked.
//!
//! Realtime / audio rules:
//! * filesystem reads never happen in render/layout.
//! * audio paths must never touch this module.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileEntryKind {
    Folder,
    File,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileBrowserEntry {
    pub name: String,
    pub path: PathBuf,
    pub kind: FileEntryKind,
    /// Lowercased extension (without the dot), or empty for folders.
    pub extension: String,
}

impl FileBrowserEntry {
    pub fn is_audio(&self) -> bool {
        matches!(
            self.extension.as_str(),
            "wav" | "wave" | "mp3" | "flac" | "ogg" | "oga" | "m4a" | "aiff" | "aif"
        )
    }

    pub fn is_midi(&self) -> bool {
        matches!(self.extension.as_str(), "mid" | "midi")
    }

    pub fn is_plugin_preset(&self) -> bool {
        self.extension == "pst"
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserNodeKind {
    /// Subtle grouping header (COLLECTIONS / LIBRARY / PLACES). Collapsible,
    /// never selectable, carries no path.
    GroupHeader,
    Section,
    Folder,
    File,
    /// Non-interactive hint row — e.g. an honest "No favorites yet" empty state
    /// for a category whose data provider does not exist yet.
    Info,
}

/// Presentation-neutral icon hint resolved by the view layer into a concrete
/// SVG glyph. Keeping this in the model (instead of matching label strings in
/// the renderer) is what lets the same navigation model drive future sources
/// without the view guessing icons from text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserIcon {
    None,
    Favorites,
    Recent,
    Samples,
    Instruments,
    Plugins,
    AudioFiles,
    Projects,
    Templates,
    UserLibrary,
    Downloads,
    Desktop,
    Documents,
    Music,
    Videos,
    Drive,
    Folder,
    FolderOpen,
    AudioFile,
    MidiFile,
    PresetFile,
    ProjectFile,
    GenericFile,
}

#[derive(Debug, Clone)]
pub struct BrowserRootSection {
    pub id: String,
    pub label: String,
    pub root_path: Option<PathBuf>,
    pub kind: BrowserNodeKind,
}

#[derive(Debug, Clone)]
pub struct BrowserVisibleNode {
    pub id: String,
    pub label: String,
    pub path: Option<PathBuf>,
    pub kind: BrowserNodeKind,
    pub depth: usize,
    pub extension: String,
    pub expandable: bool,
    pub expanded: bool,
    pub selected: bool,
    pub error: Option<String>,
    /// Semantic icon hint, resolved to a glyph by the view layer.
    pub icon: BrowserIcon,
}

impl BrowserVisibleNode {
    /// Rows the user can land on with click / arrow keys. Group headers and
    /// info/empty-state rows are skipped by selection and keyboard navigation.
    pub fn is_selectable(&self) -> bool {
        !matches!(
            self.kind,
            BrowserNodeKind::GroupHeader | BrowserNodeKind::Info
        )
    }
}

impl BrowserVisibleNode {
    pub fn is_audio(&self) -> bool {
        matches!(
            self.extension.as_str(),
            "wav" | "wave" | "mp3" | "flac" | "ogg" | "oga" | "m4a" | "aiff" | "aif"
        )
    }

    pub fn is_midi(&self) -> bool {
        matches!(self.extension.as_str(), "mid" | "midi")
    }

    pub fn is_plugin_preset(&self) -> bool {
        self.extension == "pst"
    }
}

/// Lazy directory cache populated by the background indexer.
///
/// Each known path is in exactly one of three states:
///   * `loaded` — entries cached, render shows children.
///   * `loading` — request in flight, render shows a Loading row.
///   * `errors` — last load failed, render shows an Error row.
/// Paths in none of these maps are treated as "never asked" and the
/// layout will dispatch a load when the user expands them.
#[derive(Debug, Clone, Default)]
pub struct IndexCache {
    pub loaded: HashMap<PathBuf, IndexedDir>,
    pub loading: HashSet<PathBuf>,
    pub errors: HashMap<PathBuf, String>,
}

#[derive(Debug, Clone)]
pub struct IndexedDir {
    pub entries: Vec<FileBrowserEntry>,
    pub loaded_at: Instant,
}

#[derive(Debug, Clone)]
pub struct FileBrowserState {
    pub selected: Option<PathBuf>,
    /// Expand state, keyed exclusively by path. Drive roots and folders
    /// alike live here so toggle / lookup never disagree.
    pub expanded_paths: HashSet<PathBuf>,
    /// Top-level filesystem roots — drive letters on Windows, `/` and
    /// per-volume mounts on Unix-like systems. Enumerated cheaply at
    /// startup (Win32 `GetLogicalDrives` bitmask on Windows).
    pub root_drives: Vec<BrowserRootSection>,
    /// Lazy index of expanded directories. Render reads from here; the
    /// layout owns the background loader that populates it.
    pub index: IndexCache,
    /// Active project folder. Pinned to the top of the browser roots when set.
    pub project_folder: Option<PathBuf>,
    /// Current search filter query.
    pub filter: String,
    /// Whether selecting an audio file immediately auditions it through the
    /// engine. This is session-only and defaults on for DAW-style browsing.
    pub preview_enabled: bool,
    /// Audio files whose waveform peaks are being decoded in the background for
    /// the mini preview pane — guards against re-spawning a decode while one is
    /// in flight (e.g. arrowing quickly through a folder).
    pub waveform_inflight: HashSet<PathBuf>,
    /// File the engine is currently auditioning, plus its playhead in seconds of
    /// that file. Both are polled from the engine (never guessed from a UI
    /// timer), and the pair is dropped together the moment the preview voice
    /// retires — so the pane can never draw a stale playhead, nor one belonging
    /// to a different file than the waveform on screen.
    pub preview_playing: Option<PathBuf>,
    pub preview_position_seconds: Option<f32>,
    /// Group headers (`group:*` ids) the user has collapsed. Collapsed groups
    /// hide all their child rows. Default = all groups expanded.
    pub collapsed_groups: HashSet<String>,
    /// Expand state for path-less category rows (e.g. `collections:favorites`)
    /// whose contents are not filesystem paths. Filesystem folders use
    /// `expanded_paths` instead.
    pub expanded_virtual: HashSet<String>,
    /// Cached flattened visible nodes representing the current state.
    pub visible_nodes: Vec<BrowserVisibleNode>,
}

impl Default for FileBrowserState {
    fn default() -> Self {
        let mut state = Self {
            selected: None,
            expanded_paths: HashSet::new(),
            root_drives: default_root_drives(),
            index: IndexCache::default(),
            project_folder: None,
            filter: String::new(),
            preview_enabled: true,
            waveform_inflight: HashSet::new(),
            preview_playing: None,
            preview_position_seconds: None,
            collapsed_groups: {
                // "Filesystem" (raw drives / `/` / mounted volumes) is advanced,
                // beginner-unfriendly territory — start collapsed. Every other
                // group defaults to expanded.
                let mut collapsed = HashSet::new();
                collapsed.insert("group:filesystem".to_string());
                collapsed
            },
            expanded_virtual: HashSet::new(),
            visible_nodes: Vec::new(),
        };
        state.update_visible_nodes();
        state
    }
}

impl FileBrowserState {
    pub fn select(&mut self, path: PathBuf) {
        self.selected = Some(path);
        self.update_visible_nodes();
    }

    /// Toggle expand state for a node. Path is the source of truth.
    /// Returns `true` if the path was just expanded (caller should ensure
    /// it is indexed); `false` if it was collapsed.
    pub fn toggle_node(&mut self, node_id: &str, path: Option<&Path>) -> bool {
        // Group headers have no path; their open/closed state lives in
        // `collapsed_groups` keyed by the `group:*` id.
        if path.is_none() {
            if node_id.starts_with("group:") {
                let expanded = if self.collapsed_groups.remove(node_id) {
                    true
                } else {
                    self.collapsed_groups.insert(node_id.to_string());
                    false
                };
                self.update_visible_nodes();
                return expanded;
            }
            // Path-less category (Favorites / Recent …) — expand state keyed
            // by id since there is no filesystem path to track.
            let expanded = if self.expanded_virtual.remove(node_id) {
                false
            } else {
                self.expanded_virtual.insert(node_id.to_string());
                true
            };
            self.update_visible_nodes();
            return expanded;
        }
        let Some(path) = path else {
            return false;
        };
        let path = path.to_path_buf();
        let expanded = if self.expanded_paths.contains(&path) {
            self.expanded_paths.remove(&path);
            false
        } else {
            self.expanded_paths.insert(path);
            true
        };
        self.update_visible_nodes();
        expanded
    }

    pub fn is_expanded_node(&self, _node_id: &str, path: Option<&Path>) -> bool {
        path.is_some_and(|p| self.expanded_paths.contains(p))
    }

    /// Apply a finished directory listing from the background indexer.
    pub fn apply_loaded(&mut self, path: PathBuf, entries: Vec<FileBrowserEntry>) {
        self.index.loading.remove(&path);
        self.index.errors.remove(&path);
        self.index.loaded.insert(
            path,
            IndexedDir {
                entries,
                loaded_at: Instant::now(),
            },
        );
        self.update_visible_nodes();
    }

    /// Apply a finished directory listing failure from the background indexer.
    pub fn apply_error(&mut self, path: PathBuf, error: String) {
        self.index.loading.remove(&path);
        self.index.loaded.remove(&path);
        self.index.errors.insert(path, error);
        self.update_visible_nodes();
    }

    /// Mark a path as having an in-flight load request.
    pub fn mark_loading(&mut self, path: PathBuf) {
        self.index.errors.remove(&path);
        self.index.loading.insert(path);
        self.update_visible_nodes();
    }

    /// Set active project folder and refresh roots.
    pub fn set_project_folder(&mut self, folder: Option<PathBuf>) {
        self.project_folder = folder;
        self.update_visible_nodes();
    }

    /// Set search filter query.
    pub fn set_filter(&mut self, filter: &str) {
        self.filter = filter.to_string();
        self.update_visible_nodes();
    }

    /// Flip the auto-preview toggle. Returns the new state.
    pub fn toggle_preview_enabled(&mut self) -> bool {
        self.preview_enabled = !self.preview_enabled;
        self.preview_enabled
    }

    /// The current selection if (and only if) it is an audio file — drives the
    /// mini waveform preview pane.
    pub fn selected_audio_path(&self) -> Option<&Path> {
        let path = self.selected.as_deref()?;
        is_audio_path(path).then_some(path)
    }

    /// Record which file the engine was asked to audition. The playhead stays
    /// empty until the engine reports a real position, so a decode that never
    /// produces sound never draws one.
    pub fn set_preview_playing(&mut self, path: PathBuf) {
        self.preview_playing = Some(path);
        self.preview_position_seconds = None;
    }

    /// Apply the engine's preview playhead. `None` means nothing is auditioning,
    /// which also clears the file the playhead belonged to. Returns whether the
    /// pane needs a repaint.
    pub fn apply_preview_position(&mut self, position_seconds: Option<f32>) -> bool {
        match position_seconds {
            Some(seconds) => {
                let changed = self.preview_position_seconds != Some(seconds);
                self.preview_position_seconds = Some(seconds);
                changed
            }
            None => {
                let changed =
                    self.preview_position_seconds.is_some() || self.preview_playing.is_some();
                self.preview_position_seconds = None;
                self.preview_playing = None;
                changed
            }
        }
    }

    /// Playhead for `path` in seconds, if that is the file currently being
    /// auditioned.
    pub fn preview_position_for(&self, path: &Path) -> Option<f32> {
        (self.preview_playing.as_deref() == Some(path)).then_some(self.preview_position_seconds)?
    }

    /// Mark `path` as having an in-flight waveform decode. Returns `true` if it
    /// was newly inserted (caller should spawn the decode), `false` if one is
    /// already running.
    pub fn begin_waveform_load(&mut self, path: PathBuf) -> bool {
        self.waveform_inflight.insert(path)
    }

    /// Clear the in-flight marker once a waveform decode finishes (or fails).
    pub fn end_waveform_load(&mut self, path: &Path) {
        self.waveform_inflight.remove(path);
    }

    /// Returns the list of currently-expanded paths whose contents have
    /// neither been loaded nor are loading. The caller dispatches
    /// background loads for each.
    pub fn paths_needing_load(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for path in &self.expanded_paths {
            if self.index.loaded.contains_key(path) || self.index.loading.contains(path) {
                continue;
            }
            out.push(path.clone());
        }
        out
    }

    pub fn visible_node_count(&self) -> usize {
        self.visible_nodes.len()
    }

    /// Flatten the tree into one row per visible node, driven entirely by
    /// the in-memory cache. No filesystem access happens here.
    pub fn visible_nodes(&self) -> Vec<BrowserVisibleNode> {
        self.visible_nodes.clone()
    }

    /// Re-calculate the cached visible flattened nodes.
    ///
    /// The browser is organized into three subtle groups — **Collections**,
    /// **Library**, and **Places** — each a collapsible header followed by its
    /// items (depth 1) and their lazily-loaded filesystem children (depth 2+).
    /// Every Library/Places item is backed by a real path; Collections items
    /// (Favorites/Recent) have no provider yet and show an honest empty state.
    pub fn update_visible_nodes(&mut self) {
        let mut nodes = Vec::new();
        let dirs = resolve_standard_dirs();
        let templates_dir = crate::paths::FutureboardPaths::resolve().templates;

        // ── Collections ───────────────────────────────────────────────
        if self.push_group_header("group:collections", "Collections", &mut nodes) {
            // TODO: wire to real favorites/recent providers. Until then these
            // are honest empty categories, never fabricated content.
            self.push_virtual_category(
                "collections:favorites",
                "Favorites",
                BrowserIcon::Favorites,
                "No favorites yet",
                &mut nodes,
            );
            self.push_virtual_category(
                "collections:recent",
                "Recent",
                BrowserIcon::Recent,
                "No recent items",
                &mut nodes,
            );
        }

        // ── Library ───────────────────────────────────────────────────
        if self.push_group_header("group:library", "Library", &mut nodes) {
            if let Some(p) = dirs.get("samples") {
                self.push_root(
                    "lib:samples",
                    "Samples",
                    p,
                    BrowserIcon::Samples,
                    &mut nodes,
                );
            }
            if let Some(p) = dirs.get("plugins") {
                // Instrument presets are stored below format-specific roots
                // (`VST3/Instruments`, `CLAP/Instruments`). Do not point this
                // category at the nonexistent `Audio Plug-ins/Instruments`.
                // A registry-backed aggregate can replace this honest virtual
                // state when cross-format browser providers are introduced.
                self.push_virtual_category(
                    "lib:instruments",
                    "Instruments",
                    BrowserIcon::Instruments,
                    "Available after a plug-in scan",
                    &mut nodes,
                );
                self.push_root(
                    "lib:plugins",
                    "Plug-ins",
                    p,
                    BrowserIcon::Plugins,
                    &mut nodes,
                );
            }
            if let Some(p) = dirs.get("audio_files") {
                self.push_root(
                    "lib:audio_files",
                    "Audio Files",
                    p,
                    BrowserIcon::AudioFiles,
                    &mut nodes,
                );
            }
            if let Some(p) = dirs.get("projects") {
                self.push_root(
                    "lib:projects",
                    "Projects",
                    p,
                    BrowserIcon::Projects,
                    &mut nodes,
                );
            }
            self.push_root(
                "lib:templates",
                "Templates",
                &templates_dir,
                BrowserIcon::Templates,
                &mut nodes,
            );
        }

        // ── Places ────────────────────────────────────────────────────
        if self.push_group_header("group:places", "Places", &mut nodes) {
            if let Some(proj) = self.project_folder.clone() {
                let label = proj
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "Current Project".to_string());
                self.push_root(
                    "places:project",
                    &format!("Project — {label}"),
                    &proj,
                    BrowserIcon::Projects,
                    &mut nodes,
                );
            }
            if let Some(p) = dirs.get("user_data") {
                self.push_root(
                    "places:user_data",
                    "User Data",
                    p,
                    BrowserIcon::UserLibrary,
                    &mut nodes,
                );
            }
            // Friendly, platform-provided user folders (Home/Documents/Desktop/
            // Downloads/Music) — the normal beginner-facing entries. Raw
            // filesystem roots (drive letters, `/`, mounted volumes) live in the
            // separate "Filesystem" group below instead of being mixed in here.
            if let Some(p) = dirs::home_dir() {
                self.push_root(
                    "places:home",
                    "Home",
                    &p,
                    BrowserIcon::UserLibrary,
                    &mut nodes,
                );
            }
            if let Some(p) = dirs::document_dir() {
                self.push_root(
                    "places:documents",
                    "Documents",
                    &p,
                    BrowserIcon::Documents,
                    &mut nodes,
                );
            }
            if let Some(p) = dirs::desktop_dir() {
                self.push_root(
                    "places:desktop",
                    "Desktop",
                    &p,
                    BrowserIcon::Desktop,
                    &mut nodes,
                );
            }
            if let Some(p) = dirs::download_dir() {
                self.push_root(
                    "places:downloads",
                    "Downloads",
                    &p,
                    BrowserIcon::Downloads,
                    &mut nodes,
                );
            }
            if let Some(p) = dirs::audio_dir() {
                self.push_root("places:music", "Music", &p, BrowserIcon::Music, &mut nodes);
            }
        }

        // ── Filesystem (advanced) ────────────────────────────────────────
        // Raw drive letters / `/` / mounted volumes. Collapsed by default so
        // beginners see the friendly Places list first; full root access
        // remains one click away.
        if !self.root_drives.is_empty()
            && self.push_group_header("group:filesystem", "Filesystem", &mut nodes)
        {
            let drives = self.root_drives.clone();
            for drive in &drives {
                if let Some(drive_path) = drive.root_path.as_ref() {
                    self.push_root(
                        &drive.id,
                        &drive.label,
                        drive_path,
                        BrowserIcon::Drive,
                        &mut nodes,
                    );
                }
            }
        }

        // Apply the active search filter across the whole flattened tree.
        if !self.filter.is_empty() {
            nodes = self.filter_flattened_nodes(nodes);
        }

        self.visible_nodes = nodes;
    }

    /// Push a collapsible group header. Returns `true` when the group is open
    /// (caller should emit its items).
    fn push_group_header(
        &self,
        id: &str,
        label: &str,
        nodes: &mut Vec<BrowserVisibleNode>,
    ) -> bool {
        // While searching, keep every group open so matches in collapsed groups
        // can still surface; the header chevron still reflects the saved state.
        let open = !self.filter.is_empty() || !self.collapsed_groups.contains(id);
        nodes.push(BrowserVisibleNode {
            id: id.to_string(),
            label: label.to_string(),
            path: None,
            kind: BrowserNodeKind::GroupHeader,
            depth: 0,
            extension: String::new(),
            expandable: true,
            expanded: open,
            selected: false,
            error: None,
            icon: BrowserIcon::None,
        });
        open
    }

    /// Push a real, filesystem-backed navigation item (depth 1) and, when
    /// expanded, its lazily-loaded children (depth 2+).
    fn push_root(
        &self,
        id: &str,
        label: &str,
        path: &Path,
        icon: BrowserIcon,
        nodes: &mut Vec<BrowserVisibleNode>,
    ) {
        let expanded = self.expanded_paths.contains(path);
        let selected = self.selected.as_deref() == Some(path);
        nodes.push(BrowserVisibleNode {
            id: id.to_string(),
            label: label.to_string(),
            path: Some(path.to_path_buf()),
            kind: BrowserNodeKind::Folder,
            depth: 1,
            extension: String::new(),
            expandable: true,
            expanded,
            selected,
            error: None,
            icon,
        });
        if expanded {
            self.append_cached_dir(path, 2, nodes);
        }
    }

    /// Push a path-less category (e.g. Favorites) whose provider does not exist
    /// yet. Expanding it reveals a single honest empty-state row — never mock
    /// content.
    fn push_virtual_category(
        &self,
        id: &str,
        label: &str,
        icon: BrowserIcon,
        empty_hint: &str,
        nodes: &mut Vec<BrowserVisibleNode>,
    ) {
        let expanded = self.expanded_virtual.contains(id);
        nodes.push(BrowserVisibleNode {
            id: id.to_string(),
            label: label.to_string(),
            path: None,
            kind: BrowserNodeKind::Folder,
            depth: 1,
            extension: String::new(),
            expandable: true,
            expanded,
            selected: false,
            error: None,
            icon,
        });
        if expanded {
            nodes.push(BrowserVisibleNode {
                id: format!("{id}:empty"),
                label: empty_hint.to_string(),
                path: None,
                kind: BrowserNodeKind::Info,
                depth: 2,
                extension: String::new(),
                expandable: false,
                expanded: false,
                selected: false,
                error: None,
                icon: BrowserIcon::None,
            });
        }
    }

    /// Collapse every expanded folder/category. Group headers stay open.
    pub fn collapse_all(&mut self) {
        self.expanded_paths.clear();
        self.expanded_virtual.clear();
        self.update_visible_nodes();
    }

    /// Drop cached listings for currently-expanded folders and return them so
    /// the caller can re-dispatch background loads (Rescan).
    pub fn invalidate_expanded(&mut self) -> Vec<PathBuf> {
        let paths: Vec<PathBuf> = self.expanded_paths.iter().cloned().collect();
        for p in &paths {
            self.index.loaded.remove(p);
            self.index.errors.remove(p);
            self.index.loading.remove(p);
        }
        self.update_visible_nodes();
        paths
    }

    fn append_cached_dir(&self, dir: &Path, depth: usize, nodes: &mut Vec<BrowserVisibleNode>) {
        if let Some(err) = self.index.errors.get(dir) {
            nodes.push(placeholder_row(dir, depth, err.clone(), true));
            return;
        }
        if self.index.loading.contains(dir) {
            nodes.push(placeholder_row(dir, depth, "Loading…".to_string(), false));
            return;
        }
        let Some(indexed) = self.index.loaded.get(dir) else {
            nodes.push(placeholder_row(dir, depth, "Loading…".to_string(), false));
            return;
        };

        for entry in &indexed.entries {
            let is_folder = entry.kind == FileEntryKind::Folder;
            let expanded = is_folder && self.expanded_paths.contains(&entry.path);
            let selected = self.selected.as_deref() == Some(entry.path.as_path());
            nodes.push(BrowserVisibleNode {
                id: entry.path.to_string_lossy().to_string(),
                label: entry.name.clone(),
                path: Some(entry.path.clone()),
                kind: if is_folder {
                    BrowserNodeKind::Folder
                } else {
                    BrowserNodeKind::File
                },
                depth,
                extension: entry.extension.clone(),
                expandable: is_folder,
                expanded,
                selected,
                error: None,
                icon: entry_icon(is_folder, expanded, entry),
            });

            if expanded {
                self.append_cached_dir(&entry.path, depth + 1, nodes);
            }
        }
    }

    fn filter_flattened_nodes(&self, nodes: Vec<BrowserVisibleNode>) -> Vec<BrowserVisibleNode> {
        let query = self.filter.to_lowercase();
        let mut kept = vec![false; nodes.len()];

        for i in (0..nodes.len()).rev() {
            let node = &nodes[i];
            let matches_query = node.label.to_lowercase().contains(&query);
            if matches_query {
                kept[i] = true;
                continue;
            }

            if node.expandable {
                let mut has_kept_descendant = false;
                for j in (i + 1)..nodes.len() {
                    let child = &nodes[j];
                    if child.depth <= node.depth {
                        break;
                    }
                    if kept[j] {
                        has_kept_descendant = true;
                        break;
                    }
                }
                if has_kept_descendant {
                    kept[i] = true;
                }
            }
        }

        nodes
            .into_iter()
            .enumerate()
            .filter(|(i, _)| kept[*i])
            .map(|(_, n)| n)
            .collect()
    }

    // Keyboard Helpers

    /// Indices of rows the user can actually land on (skips group headers,
    /// info/empty rows, and synthetic path-less placeholders).
    fn selectable_indices(&self) -> Vec<usize> {
        self.visible_nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| n.is_selectable() && n.path.is_some())
            .map(|(i, _)| i)
            .collect()
    }

    pub fn select_next(&mut self) {
        let selectable = self.selectable_indices();
        if selectable.is_empty() {
            return;
        }
        let current_pos = self.selected.as_ref().and_then(|sel| {
            selectable
                .iter()
                .position(|&i| self.visible_nodes[i].path.as_ref() == Some(sel))
        });
        let target = match current_pos {
            Some(pos) if pos + 1 < selectable.len() => selectable[pos + 1],
            Some(pos) => selectable[pos],
            None => selectable[0],
        };
        self.selected = self.visible_nodes[target].path.clone();
        self.update_visible_nodes();
    }

    pub fn select_previous(&mut self) {
        let selectable = self.selectable_indices();
        if selectable.is_empty() {
            return;
        }
        let current_pos = self.selected.as_ref().and_then(|sel| {
            selectable
                .iter()
                .position(|&i| self.visible_nodes[i].path.as_ref() == Some(sel))
        });
        let target = match current_pos {
            Some(pos) if pos > 0 => selectable[pos - 1],
            Some(pos) => selectable[pos],
            None => selectable[0],
        };
        self.selected = self.visible_nodes[target].path.clone();
        self.update_visible_nodes();
    }

    pub fn expand_selected(&mut self) {
        if let Some(current_selected) = &self.selected {
            if self.expanded_paths.contains(current_selected) {
                if let Some(idx) = self
                    .visible_nodes
                    .iter()
                    .position(|n| n.path.as_ref() == Some(current_selected))
                {
                    if idx + 1 < self.visible_nodes.len()
                        && self.visible_nodes[idx + 1].depth > self.visible_nodes[idx].depth
                    {
                        self.selected = self.visible_nodes[idx + 1].path.clone();
                    }
                }
            } else {
                self.expanded_paths.insert(current_selected.clone());
                self.update_visible_nodes();
            }
        }
    }

    pub fn collapse_selected_or_parent(&mut self) {
        if let Some(current_selected) = &self.selected {
            if self.expanded_paths.contains(current_selected) {
                self.expanded_paths.remove(current_selected);
                self.update_visible_nodes();
            } else {
                if let Some(idx) = self
                    .visible_nodes
                    .iter()
                    .position(|n| n.path.as_ref() == Some(current_selected))
                {
                    let current_depth = self.visible_nodes[idx].depth;
                    if current_depth > 0 {
                        for i in (0..idx).rev() {
                            // Only jump to a real (path-backed) parent — never a
                            // group header or empty-state row.
                            if self.visible_nodes[i].depth < current_depth
                                && self.visible_nodes[i].path.is_some()
                                && self.visible_nodes[i].is_selectable()
                            {
                                self.selected = self.visible_nodes[i].path.clone();
                                break;
                            }
                        }
                    }
                }
                self.update_visible_nodes();
            }
        }
    }
}

/// Resolve the semantic icon for a filesystem entry.
fn entry_icon(is_folder: bool, expanded: bool, entry: &FileBrowserEntry) -> BrowserIcon {
    if is_folder {
        if expanded {
            BrowserIcon::FolderOpen
        } else {
            BrowserIcon::Folder
        }
    } else if entry.is_audio() {
        BrowserIcon::AudioFile
    } else if entry.is_midi() {
        BrowserIcon::MidiFile
    } else {
        match entry.extension.as_str() {
            "vst3" | "pst" | "fxp" | "fxb" => BrowserIcon::PresetFile,
            "fbproj" | "fbs" => BrowserIcon::ProjectFile,
            _ => BrowserIcon::GenericFile,
        }
    }
}

fn placeholder_row(dir: &Path, depth: usize, label: String, is_error: bool) -> BrowserVisibleNode {
    BrowserVisibleNode {
        id: format!(
            "{}:{}",
            if is_error { "error" } else { "loading" },
            dir.display()
        ),
        label,
        path: None,
        kind: BrowserNodeKind::Info,
        depth,
        extension: String::new(),
        expandable: false,
        expanded: false,
        selected: false,
        error: if is_error {
            Some("Cannot read folder".to_string())
        } else {
            None
        },
        icon: BrowserIcon::None,
    }
}

/// Whether a path points at an audio file the browser can preview/import.
/// Shared by the auto-preview trigger and the mini waveform pane.
pub fn is_audio_path(path: &Path) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
        .map(|ext| {
            matches!(
                ext.as_str(),
                "wav" | "wave" | "mp3" | "flac" | "ogg" | "oga" | "m4a" | "aiff" | "aif"
            )
        })
        .unwrap_or(false)
}

/// Resolve sensible starting directory: user Music dir, then home, then cwd.
pub fn default_directory() -> PathBuf {
    if let Some(p) = dirs::audio_dir() {
        if p.is_dir() {
            return p;
        }
    }
    if let Some(p) = dirs::home_dir() {
        if p.is_dir() {
            return p;
        }
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// Enumerate logical drive letters on Windows, `/` plus mounted volumes on unix.
fn default_root_drives() -> Vec<BrowserRootSection> {
    let mut out = Vec::new();
    for path in enumerate_filesystem_roots() {
        let label = drive_label(&path);
        let id = format!("root:{}", path.display());
        out.push(BrowserRootSection {
            id,
            label,
            root_path: Some(path),
            kind: BrowserNodeKind::Folder,
        });
    }
    out
}

#[cfg(target_os = "windows")]
fn enumerate_filesystem_roots() -> Vec<PathBuf> {
    extern "system" {
        fn GetLogicalDrives() -> u32;
    }
    let mask = unsafe { GetLogicalDrives() };
    let mut drives = Vec::new();
    for i in 0u32..26 {
        if mask & (1 << i) != 0 {
            let letter = (b'A' + i as u8) as char;
            drives.push(PathBuf::from(format!("{}:\\", letter)));
        }
    }
    if drives.is_empty() {
        if let Some(home) = dirs::home_dir() {
            drives.push(home);
        }
    }
    drives
}

// Note: the user's home directory is surfaced separately as the friendly
// "Home" place (see `update_visible_nodes`), so it is intentionally not
// duplicated in the raw root list below.

#[cfg(target_os = "macos")]
fn enumerate_filesystem_roots() -> Vec<PathBuf> {
    let mut roots = vec![PathBuf::from("/")];
    let volumes = PathBuf::from("/Volumes");
    if let Ok(read) = std::fs::read_dir(&volumes) {
        for entry in read.flatten() {
            let p = entry.path();
            if p.is_dir() {
                roots.push(p);
            }
        }
    }
    roots
}

#[cfg(all(unix, not(target_os = "macos")))]
fn enumerate_filesystem_roots() -> Vec<PathBuf> {
    let mut roots = vec![PathBuf::from("/")];
    for parent in ["/media", "/mnt", "/run/media"] {
        if let Ok(read) = std::fs::read_dir(parent) {
            for entry in read.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    roots.push(p);
                }
            }
        }
    }
    roots
}

fn drive_label(path: &Path) -> String {
    #[cfg(target_os = "windows")]
    {
        let s = path.to_string_lossy();
        let trimmed = s.trim_end_matches(|c| c == '\\' || c == '/');
        if trimmed.is_empty() {
            return s.into_owned();
        }
        return trimmed.to_string();
    }
    #[cfg(not(target_os = "windows"))]
    {
        if path == Path::new("/") {
            return "/".to_string();
        }
        path.file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| path.to_string_lossy().into_owned())
    }
}

/// Resolves standard folder paths for the file browser sidebar.
///
/// Delegates to [`crate::paths::FutureboardPaths`] — the centralized path
/// system. Directory creation is handled once at app startup via
/// `FutureboardPaths::ensure_user_dirs()`, not on every browser update.
pub fn resolve_standard_dirs() -> HashMap<String, PathBuf> {
    crate::paths::FutureboardPaths::resolve().standard_dirs()
}

/// Read directory into sorted entry lists. Treat .vst3 folders as files/plugins.
pub fn read_directory(path: &Path) -> (Vec<FileBrowserEntry>, Option<String>) {
    let read = match std::fs::read_dir(path) {
        Ok(r) => r,
        Err(error) => {
            return (
                Vec::new(),
                Some(directory_error_message(error.kind()).to_string()),
            )
        }
    };

    let mut folders: Vec<FileBrowserEntry> = Vec::new();
    let mut files: Vec<FileBrowserEntry> = Vec::new();

    for ent in read.flatten() {
        let p = ent.path();
        let name = match p.file_name().and_then(|s| s.to_str()) {
            Some(n) if !n.starts_with('.') => n.to_string(),
            _ => continue,
        };
        let meta = match ent.file_type() {
            Ok(m) => m,
            Err(_) => continue,
        };

        if meta.is_dir() {
            let ext = p
                .extension()
                .and_then(|s| s.to_str())
                .map(|s| s.to_ascii_lowercase())
                .unwrap_or_default();
            if ext == "vst3" {
                files.push(FileBrowserEntry {
                    name,
                    path: p,
                    kind: FileEntryKind::File,
                    extension: ext,
                });
            } else {
                folders.push(FileBrowserEntry {
                    name,
                    path: p,
                    kind: FileEntryKind::Folder,
                    extension: String::new(),
                });
            }
        } else if meta.is_file() {
            let ext = p
                .extension()
                .and_then(|s| s.to_str())
                .map(|s| s.to_ascii_lowercase())
                .unwrap_or_default();
            if ext == "pst" {
                let display = read_pst_plugin_name(&p).unwrap_or_else(|| {
                    p.file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or(&name)
                        .to_string()
                });
                files.push(FileBrowserEntry {
                    name: display,
                    path: p,
                    kind: FileEntryKind::File,
                    extension: ext,
                });
                continue;
            }
            files.push(FileBrowserEntry {
                name,
                path: p,
                kind: FileEntryKind::File,
                extension: ext,
            });
        }
    }

    folders.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    files.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    folders.extend(files);
    (folders, None)
}

fn directory_error_message(kind: std::io::ErrorKind) -> &'static str {
    match kind {
        std::io::ErrorKind::NotFound => "Folder is not available",
        std::io::ErrorKind::PermissionDenied => "Permission denied",
        _ => "Unable to read this folder",
    }
}

fn read_pst_plugin_name(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    if bytes.len() < 24 {
        return None;
    }
    if &bytes[0..5] != b"FBPST" {
        return None;
    }
    let meta_len = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) as usize;
    if bytes.len() < 24 + meta_len {
        return None;
    }
    let meta = &bytes[24..24 + meta_len];
    let value: serde_json::Value = serde_json::from_slice(meta).ok()?;
    value
        .get("pluginMetadata")
        .and_then(|m| m.get("name"))
        .and_then(|n| n.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(state: &FileBrowserState) -> Vec<String> {
        state.visible_nodes.iter().map(|n| n.id.clone()).collect()
    }

    #[test]
    fn default_browser_has_grouped_navigation() {
        let state = FileBrowserState::default();
        let ids = ids(&state);
        // The three subtle groups are always present and open by default.
        for group in ["group:collections", "group:library", "group:places"] {
            assert!(ids.iter().any(|id| id == group), "missing {group}");
        }
        // Collections exposes the Favorites/Recent categories.
        assert!(ids.iter().any(|id| id == "collections:favorites"));
        assert!(ids.iter().any(|id| id == "collections:recent"));
        // Group headers are never selectable and carry no path.
        let header = state
            .visible_nodes
            .iter()
            .find(|n| n.id == "group:library")
            .unwrap();
        assert_eq!(header.kind, BrowserNodeKind::GroupHeader);
        assert!(!header.is_selectable());
        assert!(header.path.is_none());
    }

    #[test]
    fn preview_playhead_belongs_to_the_auditioned_file_only() {
        let mut state = FileBrowserState::default();
        let kick = PathBuf::from("C:/samples/kick.wav");
        let snare = PathBuf::from("C:/samples/snare.wav");

        state.set_preview_playing(kick.clone());
        assert_eq!(
            state.preview_position_for(&kick),
            None,
            "no playhead before the engine reports one"
        );

        assert!(state.apply_preview_position(Some(1.25)));
        assert_eq!(state.preview_position_for(&kick), Some(1.25));
        assert_eq!(
            state.preview_position_for(&snare),
            None,
            "another file must never inherit the playhead"
        );

        // Same position twice is not a repaint reason.
        assert!(!state.apply_preview_position(Some(1.25)));

        // The voice retiring clears both the playhead and its owner.
        assert!(state.apply_preview_position(None));
        assert_eq!(state.preview_position_for(&kick), None);
        assert!(!state.apply_preview_position(None), "idle stays quiet");
    }

    #[test]
    fn collapsing_a_group_hides_its_items() {
        let mut state = FileBrowserState::default();
        assert!(ids(&state).iter().any(|id| id.starts_with("lib:")));
        // Toggle the Library group closed.
        let expanded = state.toggle_node("group:library", None);
        assert!(!expanded, "group should collapse on first toggle");
        assert!(
            !ids(&state).iter().any(|id| id.starts_with("lib:")),
            "library items must be hidden while the group is collapsed"
        );
        // The header itself stays visible and reflects the collapsed state.
        let header = state
            .visible_nodes
            .iter()
            .find(|n| n.id == "group:library")
            .unwrap();
        assert!(!header.expanded);
    }

    #[test]
    fn favorites_expands_to_an_honest_empty_state() {
        let mut state = FileBrowserState::default();
        // No fabricated children before expansion.
        assert!(!ids(&state)
            .iter()
            .any(|id| id == "collections:favorites:empty"));
        let expanded = state.toggle_node("collections:favorites", None);
        assert!(expanded);
        let empty = state
            .visible_nodes
            .iter()
            .find(|n| n.id == "collections:favorites:empty")
            .expect("empty-state row should appear");
        assert_eq!(empty.kind, BrowserNodeKind::Info);
        assert!(!empty.is_selectable());
        assert!(empty.path.is_none());
    }

    #[test]
    fn instruments_is_not_backed_by_a_nonexistent_aggregate_path() {
        let state = FileBrowserState::default();
        let instruments = state
            .visible_nodes
            .iter()
            .find(|node| node.id == "lib:instruments")
            .expect("instruments category should exist");
        assert!(instruments.path.is_none());
    }

    #[test]
    fn directory_errors_are_safe_for_browser_display() {
        assert_eq!(
            directory_error_message(std::io::ErrorKind::NotFound),
            "Folder is not available"
        );
        assert_eq!(
            directory_error_message(std::io::ErrorKind::PermissionDenied),
            "Permission denied"
        );
    }

    #[test]
    fn collapse_all_clears_folder_and_category_expansion() {
        let mut state = FileBrowserState::default();
        state.expanded_paths.insert(PathBuf::from("/tmp/example"));
        state.toggle_node("collections:recent", None);
        assert!(!state.expanded_virtual.is_empty());
        state.collapse_all();
        assert!(state.expanded_paths.is_empty());
        assert!(state.expanded_virtual.is_empty());
    }
}
