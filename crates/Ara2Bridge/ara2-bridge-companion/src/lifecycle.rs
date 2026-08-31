//! Companion processor lifecycle boundary names.

/// Processor boundary observed by a companion adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LifecycleEvent {
    /// Companion state or preset loading.
    StateLoad,
    /// Processor activation or initialization.
    Activate,
    /// Processor deactivation or uninitialization.
    Deactivate,
    /// One processing-related companion operation.
    Process,
    /// Custom-view or GUI creation.
    CreateView,
    /// Entry into companion rendering state.
    BeginRendering,
    /// Exit from companion rendering state.
    EndRendering,
    /// One model-thread ARA graph mutation boundary.
    ModelMutation,
}
