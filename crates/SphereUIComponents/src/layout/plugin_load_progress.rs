//! Indeterminate "Loading Plug-in" dialog.
//!
//! Loading a VST3 / VST2 / CLAP / AU instance happens in the plugin-host
//! process and can take anywhere from a blink to tens of seconds (scanning,
//! sample streaming, a licence check). Until it lands the studio looks like it
//! did nothing to the click, so this puts a named, self-animating progress
//! dialog in front of the user for the whole load — Effect and Instrument
//! alike, since both go through the same bridge load.
//!
//! There is no percentage to report: the host does not publish load progress,
//! and inventing one would be a fake. The bar is
//! [`ProgressBarValue::Indeterminate`] and the dialog is not cancellable — the
//! load is already in flight in another process. It closes as soon as the
//! instance reaches Ready or Failed.

use super::StudioLayout;
use crate::components::progress_dialog::{
    open_progress_dialog_window, ProgressBarValue, ProgressDialogOptions,
};
use gpui::Context;

impl StudioLayout {
    /// Mark `plugin_instance_id` as loading and show (or refresh) the dialog.
    pub(crate) fn begin_plugin_load_progress(
        &mut self,
        plugin_instance_id: &str,
        display_name: &str,
        cx: &mut Context<Self>,
    ) {
        if self
            .plugin_editors
            .loading
            .iter()
            .any(|(id, _)| id == plugin_instance_id)
        {
            return;
        }
        self.plugin_editors
            .loading
            .push((plugin_instance_id.to_string(), display_name.to_string()));
        self.refresh_plugin_load_progress(cx);
    }

    /// The instance reached a terminal state (Ready or Failed) — drop it and
    /// close the dialog once nothing is left loading.
    pub(crate) fn end_plugin_load_progress(
        &mut self,
        plugin_instance_id: &str,
        cx: &mut Context<Self>,
    ) {
        let before = self.plugin_editors.loading.len();
        self.plugin_editors
            .loading
            .retain(|(id, _)| id != plugin_instance_id);
        if self.plugin_editors.loading.len() != before {
            self.refresh_plugin_load_progress(cx);
        }
    }

    /// Drop every in-flight load and close the dialog. Used when the host
    /// disconnects: those loads will never report back.
    pub(crate) fn cancel_all_plugin_load_progress(&mut self, cx: &mut Context<Self>) {
        if self.plugin_editors.loading.is_empty() {
            return;
        }
        self.plugin_editors.loading.clear();
        self.refresh_plugin_load_progress(cx);
    }

    fn refresh_plugin_load_progress(&mut self, cx: &mut Context<Self>) {
        let Some(options) = plugin_load_dialog_options(&self.plugin_editors.loading) else {
            self.close_plugin_load_progress(cx);
            return;
        };

        // Already open: re-label it in place rather than stacking a second
        // window when a chain of inserts is added back to back.
        if let Some(handle) = self.external_windows.plugin_loading.clone() {
            let updated = handle
                .update(cx, |dialog, _window, cx| {
                    dialog.set_options(options.clone(), cx);
                })
                .is_ok();
            if updated {
                return;
            }
            self.external_windows.plugin_loading = None;
        }

        let owner_bounds = crate::window_position::resolve_owner_bounds_with_preferred(
            None,
            self.studio_window_bounds(cx),
            cx,
        );
        match open_progress_dialog_window(owner_bounds, options, None, cx) {
            Ok(handle) => self.external_windows.plugin_loading = Some(handle),
            // A dialog that will not open must never hold up the load itself.
            Err(err) => eprintln!("[PluginAdd] failed to open loading dialog: {err}"),
        }
    }

    fn close_plugin_load_progress(&mut self, cx: &mut Context<Self>) {
        let Some(handle) = self.external_windows.plugin_loading.take() else {
            return;
        };
        let _ = handle.update(cx, |_dialog, window, _cx| window.remove_window());
    }
}

/// Dialog copy for the current in-flight set, or `None` when nothing is
/// loading. Pure so the "one plug-in names itself, several are counted"
/// wording is testable without a window.
fn plugin_load_dialog_options(loading: &[(String, String)]) -> Option<ProgressDialogOptions> {
    let (heading, detail) = match loading {
        [] => return None,
        [(_, name)] => (
            "Loading plug-in".to_string(),
            if name.trim().is_empty() {
                "Preparing the plug-in in the plug-in host...".to_string()
            } else {
                name.clone()
            },
        ),
        many => (
            format!("Loading {} plug-ins", many.len()),
            many.iter()
                .map(|(_, name)| name.as_str())
                .filter(|name| !name.trim().is_empty())
                .collect::<Vec<_>>()
                .join(", "),
        ),
    };
    Some(
        ProgressDialogOptions::default()
            .title("Loading Plug-in")
            .heading(heading)
            .detail(detail)
            .progress(ProgressBarValue::Indeterminate)
            .footer("Some plug-ins take a moment to scan their content.")
            .hide_percent(),
    )
}

#[cfg(test)]
mod tests {
    use super::plugin_load_dialog_options;

    #[test]
    fn nothing_loading_means_no_dialog() {
        assert!(plugin_load_dialog_options(&[]).is_none());
    }

    #[test]
    fn one_plugin_is_named_and_the_bar_stays_indeterminate() {
        let options =
            plugin_load_dialog_options(&[("i1".to_string(), "Serum".to_string())]).unwrap();
        assert_eq!(options.heading, "Loading plug-in");
        assert_eq!(options.detail.as_deref(), Some("Serum"));
        assert!(options.progress.fraction().is_none());
        assert!(!options.show_percent);
        assert!(options.cancel_label.is_none());
    }

    #[test]
    fn several_plugins_are_counted_and_listed() {
        let options = plugin_load_dialog_options(&[
            ("i1".to_string(), "Serum".to_string()),
            ("i2".to_string(), "Pro-Q".to_string()),
        ])
        .unwrap();
        assert_eq!(options.heading, "Loading 2 plug-ins");
        assert_eq!(options.detail.as_deref(), Some("Serum, Pro-Q"));
    }

    #[test]
    fn an_unnamed_plugin_still_says_something_useful() {
        let options = plugin_load_dialog_options(&[("i1".to_string(), "  ".to_string())]).unwrap();
        assert_eq!(
            options.detail.as_deref(),
            Some("Preparing the plug-in in the plug-in host...")
        );
    }
}
