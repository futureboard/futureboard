//! In-window Multiple Document Interface primitives.
//!
//! GPUI windows are operating-system windows. This module implements the DAW
//! style MDI layer used inside a single GPUI window: document rectangles,
//! z-order, focus, minimize/restore, tile, cascade, and compact document chrome.

use std::sync::Arc;

use gpui::{
    div, px, svg, App, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement,
    Styled, Window,
};

use crate::assets;
use crate::theme::Colors;

pub type MdiDocumentId = String;
pub type MdiDocumentCb = Arc<dyn Fn(&MdiDocumentId, &mut Window, &mut App) + 'static>;

const DEFAULT_W: f32 = 420.0;
const DEFAULT_H: f32 = 280.0;
const MIN_W: f32 = 260.0;
const MIN_H: f32 = 160.0;
const TITLEBAR_H: f32 = 28.0;
const TASKBAR_H: f32 = 30.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MdiDocumentKind {
    SoundfontPlayer,
    Generic,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MdiDocumentState {
    pub id: MdiDocumentId,
    pub title: String,
    pub kind: MdiDocumentKind,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub z_order: u64,
    pub minimized: bool,
    pub maximized: bool,
}

impl MdiDocumentState {
    pub fn new(
        id: impl Into<MdiDocumentId>,
        title: impl Into<String>,
        kind: MdiDocumentKind,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        z_order: u64,
    ) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            kind,
            x,
            y,
            width: width.max(MIN_W),
            height: height.max(MIN_H),
            z_order,
            minimized: false,
            maximized: false,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MdiWorkspaceState {
    pub documents: Vec<MdiDocumentState>,
    pub active_document_id: Option<MdiDocumentId>,
    next_document_seq: u64,
    next_z_order: u64,
}

impl MdiWorkspaceState {
    pub fn open_document(
        &mut self,
        kind: MdiDocumentKind,
        title: impl Into<String>,
    ) -> MdiDocumentId {
        self.next_document_seq = self.next_document_seq.saturating_add(1);
        self.next_z_order = self.next_z_order.saturating_add(1);
        let id = format!("mdi-doc-{}", self.next_document_seq);
        let offset = ((self.documents.len() % 7) as f32) * 24.0;
        self.documents.push(MdiDocumentState::new(
            id.clone(),
            title,
            kind,
            24.0 + offset,
            22.0 + offset,
            DEFAULT_W,
            DEFAULT_H,
            self.next_z_order,
        ));
        self.active_document_id = Some(id.clone());
        id
    }

    pub fn focus_document(&mut self, id: &str) -> bool {
        let Some(doc) = self.documents.iter_mut().find(|doc| doc.id == id) else {
            return false;
        };
        self.next_z_order = self.next_z_order.saturating_add(1);
        doc.z_order = self.next_z_order;
        doc.minimized = false;
        self.active_document_id = Some(id.to_string());
        true
    }

    pub fn close_document(&mut self, id: &str) -> bool {
        let Some(index) = self.documents.iter().position(|doc| doc.id == id) else {
            return false;
        };
        self.documents.remove(index);
        if self.active_document_id.as_deref() == Some(id) {
            self.active_document_id = self
                .documents
                .iter()
                .max_by_key(|doc| doc.z_order)
                .map(|doc| doc.id.clone());
        }
        true
    }

    pub fn minimize_document(&mut self, id: &str) -> bool {
        let Some(doc) = self.documents.iter_mut().find(|doc| doc.id == id) else {
            return false;
        };
        doc.minimized = true;
        if self.active_document_id.as_deref() == Some(id) {
            self.active_document_id = self
                .documents
                .iter()
                .filter(|doc| !doc.minimized)
                .max_by_key(|doc| doc.z_order)
                .map(|doc| doc.id.clone());
        }
        true
    }

    pub fn restore_document(&mut self, id: &str) -> bool {
        self.focus_document(id)
    }

    pub fn cascade(&mut self) {
        let mut z = self.next_z_order;
        for (index, doc) in self.documents.iter_mut().enumerate() {
            let offset = (index as f32) * 24.0;
            doc.x = 24.0 + offset;
            doc.y = 22.0 + offset;
            doc.width = DEFAULT_W;
            doc.height = DEFAULT_H;
            doc.maximized = false;
            doc.minimized = false;
            z = z.saturating_add(1);
            doc.z_order = z;
        }
        self.next_z_order = z;
        self.active_document_id = self.documents.last().map(|doc| doc.id.clone());
    }

    pub fn tile(&mut self, workspace_width: f32, workspace_height: f32) {
        let visible_count = self.documents.iter().filter(|doc| !doc.minimized).count();
        if visible_count == 0 {
            return;
        }
        let columns = (visible_count as f32).sqrt().ceil().max(1.0) as usize;
        let rows = visible_count.div_ceil(columns).max(1);
        let width = (workspace_width / columns as f32).max(MIN_W);
        let height = ((workspace_height - TASKBAR_H) / rows as f32).max(MIN_H);
        let mut visible_index = 0usize;
        for doc in &mut self.documents {
            if doc.minimized {
                continue;
            }
            let col = visible_index % columns;
            let row = visible_index / columns;
            doc.x = col as f32 * width;
            doc.y = row as f32 * height;
            doc.width = width;
            doc.height = height;
            doc.maximized = false;
            visible_index += 1;
        }
    }

    pub fn document_count(&self) -> usize {
        self.documents.len()
    }
}

#[derive(Clone)]
pub struct MdiWorkspaceCallbacks {
    pub on_focus: MdiDocumentCb,
    pub on_close: MdiDocumentCb,
    pub on_minimize: MdiDocumentCb,
    pub on_restore: MdiDocumentCb,
}

pub fn mdi_workspace(
    state: &MdiWorkspaceState,
    callbacks: MdiWorkspaceCallbacks,
    content_for: impl Fn(&MdiDocumentState) -> gpui::AnyElement,
) -> gpui::AnyElement {
    let mut documents = state.documents.clone();
    documents.sort_by_key(|doc| doc.z_order);

    let mut desktop = div()
        .relative()
        .size_full()
        .overflow_hidden()
        .bg(Colors::surface_muted());

    for doc in documents.iter().filter(|doc| !doc.minimized) {
        desktop = desktop.child(mdi_document_window(
            state,
            doc,
            callbacks.clone(),
            content_for(doc),
        ));
    }

    desktop
        .child(mdi_taskbar(state, callbacks))
        .into_any_element()
}

fn mdi_document_window(
    state: &MdiWorkspaceState,
    doc: &MdiDocumentState,
    callbacks: MdiWorkspaceCallbacks,
    content: gpui::AnyElement,
) -> gpui::AnyElement {
    let active = state.active_document_id.as_deref() == Some(doc.id.as_str());
    let id_for_focus = doc.id.clone();
    div()
        .id(("mdi-document", doc.z_order))
        .absolute()
        .left(px(doc.x))
        .top(px(doc.y))
        .w(px(doc.width))
        .h(px(doc.height))
        .flex()
        .flex_col()
        .overflow_hidden()
        .rounded(px(crate::theme::radius::CONTROL))
        .border(px(1.0))
        .border_color(if active {
            Colors::border_accent()
        } else {
            Colors::border_subtle()
        })
        .bg(Colors::surface_panel())
        .on_mouse_down(gpui::MouseButton::Left, {
            let focus = callbacks.on_focus.clone();
            move |_, window, cx| focus(&id_for_focus, window, cx)
        })
        .child(mdi_titlebar(doc, active, callbacks))
        .child(div().flex_1().min_h_0().overflow_hidden().child(content))
        .into_any_element()
}

fn mdi_titlebar(
    doc: &MdiDocumentState,
    active: bool,
    callbacks: MdiWorkspaceCallbacks,
) -> gpui::AnyElement {
    let minimize_id = doc.id.clone();
    let close_id = doc.id.clone();
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(7.0))
        .h(px(TITLEBAR_H))
        .px(px(8.0))
        .flex_shrink_0()
        .bg(if active {
            Colors::surface_raised()
        } else {
            Colors::surface_panel()
        })
        .border_b(px(1.0))
        .border_color(Colors::border_subtle())
        .child(
            svg()
                .path(assets::ICON_MUSIC_PATH)
                .w(px(13.0))
                .h(px(13.0))
                .text_color(if active {
                    Colors::accent_primary()
                } else {
                    Colors::text_muted()
                }),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_size(px(11.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(Colors::text_primary())
                .child(doc.title.clone()),
        )
        .child(mdi_titlebar_button(
            "mdi-minimize",
            assets::ICON_MINUS_PATH,
            move |_, window, cx| (callbacks.on_minimize)(&minimize_id, window, cx),
        ))
        .child(mdi_titlebar_button(
            "mdi-close",
            assets::ICON_X_PATH,
            move |_, window, cx| (callbacks.on_close)(&close_id, window, cx),
        ))
        .into_any_element()
}

fn mdi_titlebar_button(
    id: &'static str,
    icon_path: &'static str,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> gpui::AnyElement {
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .w(px(20.0))
        .h(px(20.0))
        .rounded(px(crate::theme::radius::CONTROL))
        .text_color(Colors::text_muted())
        .cursor(gpui::CursorStyle::PointingHand)
        .hover(|s| s.bg(Colors::surface_control_hover()))
        .on_click(on_click)
        .child(
            svg()
                .path(icon_path)
                .w(px(10.0))
                .h(px(10.0))
                .text_color(Colors::text_muted()),
        )
        .into_any_element()
}

fn mdi_taskbar(state: &MdiWorkspaceState, callbacks: MdiWorkspaceCallbacks) -> gpui::AnyElement {
    let mut bar = div()
        .absolute()
        .left(px(0.0))
        .right(px(0.0))
        .bottom(px(0.0))
        .h(px(TASKBAR_H))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.0))
        .px(px(8.0))
        .border_t(px(1.0))
        .border_color(Colors::border_subtle())
        .bg(Colors::surface_base());

    for doc in &state.documents {
        if !doc.minimized {
            continue;
        }
        let id = doc.id.clone();
        bar = bar.child(
            div()
                .id(("mdi-task", doc.z_order))
                .h(px(22.0))
                .min_w(px(128.0))
                .max_w(px(220.0))
                .px(px(8.0))
                .flex()
                .items_center()
                .rounded(px(crate::theme::radius::CONTROL))
                .border(px(1.0))
                .border_color(Colors::border_subtle())
                .bg(Colors::surface_input())
                .text_size(px(10.5))
                .text_color(Colors::text_secondary())
                .cursor(gpui::CursorStyle::PointingHand)
                .hover(|s| s.bg(Colors::surface_hover()))
                .on_click({
                    let restore = callbacks.on_restore.clone();
                    move |_, window, cx| restore(&id, window, cx)
                })
                .child(doc.title.clone()),
        );
    }

    bar.into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_focus_and_close_document() {
        let mut state = MdiWorkspaceState::default();
        let a = state.open_document(MdiDocumentKind::Generic, "A");
        let b = state.open_document(MdiDocumentKind::SoundfontPlayer, "B");
        assert_eq!(state.document_count(), 2);
        assert_eq!(state.active_document_id.as_deref(), Some(b.as_str()));
        assert!(state.focus_document(&a));
        assert_eq!(state.active_document_id.as_deref(), Some(a.as_str()));
        assert!(state.close_document(&a));
        assert_eq!(state.document_count(), 1);
    }

    #[test]
    fn minimize_and_restore_updates_active_document() {
        let mut state = MdiWorkspaceState::default();
        let a = state.open_document(MdiDocumentKind::Generic, "A");
        assert!(state.minimize_document(&a));
        assert!(state.active_document_id.is_none());
        assert!(state.restore_document(&a));
        assert_eq!(state.active_document_id.as_deref(), Some(a.as_str()));
        assert!(!state.documents[0].minimized);
    }

    #[test]
    fn tile_assigns_visible_rects() {
        let mut state = MdiWorkspaceState::default();
        state.open_document(MdiDocumentKind::Generic, "A");
        state.open_document(MdiDocumentKind::Generic, "B");
        state.tile(800.0, 600.0);
        assert!(state.documents.iter().all(|doc| doc.width >= MIN_W));
        assert!(state.documents.iter().all(|doc| doc.height >= MIN_H));
    }
}
