//! Sidebar rows, scroll sync, and status glyphs (port of pi-harness
//! `app::sidebar`, with status derived from `SessionStatus` instead of the
//! sidecar runtime flags).

use crate::status::SessionStatus;
use crate::workspace::{MuxProject, MuxSession};
use crate::grid::color::Color;
use crate::grid::theme::{HEADING, MUTED, TEXT};

pub const SIDEBAR_ANIMATION_FRAME_MS: u64 = 40;
pub const SIDEBAR_SPINNER_FRAME_TICKS: u64 = 2;
pub const SIDEBAR_SPINNER_FRAME_MS: u64 = SIDEBAR_ANIMATION_FRAME_MS * SIDEBAR_SPINNER_FRAME_TICKS;
pub const SIDEBAR_SPINNER_FRAMES: &[&str] = &[
    "⠋", "⠙", "⠹", "⠸", "⢸", "⢰", "⣰", "⣠", "⣤", "⣄", "⣆", "⡆", "⡇", "⠇", "⠏",
];
pub const SIDEBAR_NOTIFICATION_GLYPH: &str = "⣿";

const DRAFT_SESSION_LABEL: &str = "new session";

#[derive(Clone, Debug)]
pub enum SidebarRowKind {
    Project(usize),
    Session {
        project_index: usize,
        session_index: usize,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarStatusKind {
    Active,
    Interrupted,
    Notification,
}

#[derive(Clone, Debug)]
pub struct SidebarRow {
    pub kind: SidebarRowKind,
    pub text: String,
    pub fg: Color,
    pub bg: Option<Color>,
    pub inverted: bool,
    pub selector: bool,
    pub current: bool,
    pub status: Option<SidebarStatusKind>,
}

/// anchor_row = selected project's header; selected_row = active row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SidebarSelectionSpan {
    pub anchor_row: usize,
    pub selected_row: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SidebarViewportItem {
    pub row_index: usize,
    pub visible_row: usize,
    pub sticky: bool,
}

fn clamp_scroll_into_range(current_scroll: usize, min_scroll: usize, max_scroll: usize) -> usize {
    if min_scroll <= max_scroll {
        current_scroll.clamp(min_scroll, max_scroll)
    } else {
        max_scroll
    }
}

fn sync_sidebar_scroll_to_row(
    current_scroll: usize,
    row_count: usize,
    visible_rows: usize,
    row_index: usize,
) -> usize {
    let max_scroll = row_count.saturating_sub(visible_rows);
    if visible_rows == 0 {
        return current_scroll.min(max_scroll);
    }

    let min_scroll = row_index.saturating_add(1).saturating_sub(visible_rows);
    clamp_scroll_into_range(
        current_scroll.min(max_scroll),
        min_scroll,
        row_index.min(max_scroll),
    )
}

fn sync_sidebar_scroll_to_selection_span(
    current_scroll: usize,
    row_count: usize,
    visible_rows: usize,
    span: SidebarSelectionSpan,
) -> usize {
    // Keep the selected project's header visible alongside the selected row
    // when possible; otherwise reserve a sticky header row for it.
    let max_scroll = row_count.saturating_sub(visible_rows);
    let current_scroll = current_scroll.min(max_scroll);
    if visible_rows == 0 {
        return current_scroll;
    }

    if span.anchor_row >= span.selected_row {
        return sync_sidebar_scroll_to_row(
            current_scroll,
            row_count,
            visible_rows,
            span.selected_row,
        );
    }

    let min_scroll = span
        .selected_row
        .saturating_add(1)
        .saturating_sub(visible_rows);
    let max_scroll_with_anchor = span.anchor_row.min(max_scroll);
    if min_scroll <= max_scroll_with_anchor {
        return clamp_scroll_into_range(current_scroll, min_scroll, max_scroll_with_anchor);
    }

    if visible_rows < 2 {
        return sync_sidebar_scroll_to_row(
            current_scroll,
            row_count,
            visible_rows,
            span.selected_row,
        );
    }

    let sticky_min_scroll = span
        .selected_row
        .saturating_add(2)
        .saturating_sub(visible_rows)
        .max(span.anchor_row.saturating_add(1))
        .min(max_scroll);
    clamp_scroll_into_range(
        current_scroll,
        sticky_min_scroll,
        span.selected_row.min(max_scroll),
    )
}

pub fn sticky_sidebar_anchor_row(
    scroll: usize,
    visible_rows: usize,
    span: SidebarSelectionSpan,
) -> Option<usize> {
    if visible_rows < 2 || span.anchor_row >= span.selected_row || span.anchor_row >= scroll {
        return None;
    }

    let body_visible_rows = visible_rows - 1;
    let body_last_visible = scroll.saturating_add(body_visible_rows.saturating_sub(1));
    (span.selected_row >= scroll && span.selected_row <= body_last_visible)
        .then_some(span.anchor_row)
}

pub fn sidebar_viewport_items(
    rows: &[SidebarRow],
    scroll: usize,
    visible_rows: usize,
    sticky_row_index: Option<usize>,
) -> Vec<SidebarViewportItem> {
    if visible_rows == 0 {
        return Vec::new();
    }

    let mut items = Vec::new();
    let body_start = usize::from(sticky_row_index.is_some());
    if let Some(row_index) = sticky_row_index.filter(|row_index| rows.get(*row_index).is_some()) {
        items.push(SidebarViewportItem {
            row_index,
            visible_row: 0,
            sticky: true,
        });
    }

    let body_rows = visible_rows.saturating_sub(body_start);
    items.extend(
        rows.iter()
            .enumerate()
            .skip(scroll)
            .take(body_rows)
            .enumerate()
            .map(|(offset, (row_index, _))| SidebarViewportItem {
                row_index,
                visible_row: body_start + offset,
                sticky: false,
            }),
    );
    items
}

pub fn session_sidebar_status(session: &MuxSession) -> Option<SidebarStatusKind> {
    match &session.status {
        status if status.is_busy() => Some(SidebarStatusKind::Active),
        SessionStatus::Crashed => Some(SidebarStatusKind::Interrupted),
        SessionStatus::Done => Some(SidebarStatusKind::Notification),
        _ => None,
    }
}

pub fn sidebar_status_glyph(status: SidebarStatusKind, now_ms: u64) -> &'static str {
    match status {
        SidebarStatusKind::Active => {
            let frame =
                ((now_ms / SIDEBAR_SPINNER_FRAME_MS) as usize) % SIDEBAR_SPINNER_FRAMES.len();
            SIDEBAR_SPINNER_FRAMES[frame]
        }
        SidebarStatusKind::Interrupted | SidebarStatusKind::Notification => {
            SIDEBAR_NOTIFICATION_GLYPH
        }
    }
}

fn project_sidebar_status(project: &MuxProject) -> Option<SidebarStatusKind> {
    let mut active = false;

    for session in &project.sessions {
        match session_sidebar_status(session) {
            Some(SidebarStatusKind::Interrupted) => return Some(SidebarStatusKind::Interrupted),
            Some(SidebarStatusKind::Notification) => return Some(SidebarStatusKind::Notification),
            Some(SidebarStatusKind::Active) => active = true,
            None => {}
        }
    }

    active.then_some(SidebarStatusKind::Active)
}

pub fn sidebar_has_spinner(projects: &[MuxProject]) -> bool {
    projects
        .iter()
        .flat_map(|project| project.sessions.iter())
        .any(|session| session.status.is_busy())
}

pub fn session_sidebar_title(session: &MuxSession) -> String {
    let title = session
        .first_message
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if title.is_empty() {
        DRAFT_SESSION_LABEL.to_owned()
    } else {
        title
    }
}

pub fn project_sidebar_title(project: &MuxProject) -> String {
    project
        .cwd
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| project.cwd.display().to_string())
}

pub fn build_sidebar_rows(projects: &[MuxProject], selected_project: usize) -> Vec<SidebarRow> {
    let mut rows = Vec::new();

    for (project_index, project) in projects.iter().enumerate() {
        let project_selected = project_index == selected_project;
        let selected_visible = project
            .sessions
            .get(project.selected)
            .is_some_and(MuxSession::is_sidebar_visible);
        let project_focused = project_selected && !selected_visible;
        let status = project_sidebar_status(project);
        rows.push(SidebarRow {
            kind: SidebarRowKind::Project(project_index),
            text: project_sidebar_title(project),
            fg: if project_focused { TEXT } else { HEADING },
            bg: None,
            inverted: project_focused,
            selector: project_focused,
            current: project_selected,
            status,
        });

        for (session_index, session) in project.sessions.iter().enumerate() {
            if !session.is_sidebar_visible() {
                continue;
            }
            let selected = project_selected && session_index == project.selected;
            let status = session_sidebar_status(session);
            rows.push(SidebarRow {
                kind: SidebarRowKind::Session {
                    project_index,
                    session_index,
                },
                text: session_sidebar_title(session),
                fg: if selected { TEXT } else { MUTED },
                bg: None,
                inverted: selected,
                selector: selected,
                current: selected,
                status,
            });
        }
    }

    rows
}

pub fn selected_sidebar_row_index(rows: &[SidebarRow]) -> Option<usize> {
    rows.iter().position(|row| row.selector)
}

pub fn selected_sidebar_selection_span(
    selected_project: usize,
    rows: &[SidebarRow],
) -> Option<SidebarSelectionSpan> {
    let selected_row = selected_sidebar_row_index(rows)?;
    let anchor_row = rows
        .iter()
        .position(
            |row| matches!(row.kind, SidebarRowKind::Project(index) if index == selected_project),
        )
        .unwrap_or(selected_row);
    Some(SidebarSelectionSpan {
        anchor_row,
        selected_row,
    })
}

pub fn clamp_sidebar_scroll_value(
    current_scroll: usize,
    row_count: usize,
    visible_rows: usize,
) -> usize {
    current_scroll.min(row_count.saturating_sub(visible_rows))
}

pub fn ensure_sidebar_selection_visible(
    current_scroll: usize,
    selected_project: usize,
    rows: &[SidebarRow],
    visible_rows: usize,
) -> usize {
    let current_scroll = clamp_sidebar_scroll_value(current_scroll, rows.len(), visible_rows);
    let Some(span) = selected_sidebar_selection_span(selected_project, rows) else {
        return current_scroll;
    };
    sync_sidebar_scroll_to_selection_span(current_scroll, rows.len(), visible_rows, span)
}

pub fn scroll_sidebar_by_rows_value(
    current_scroll: usize,
    delta_rows: i32,
    visible_rows: usize,
    row_count: usize,
) -> (usize, bool) {
    if delta_rows == 0 || row_count == 0 {
        return (current_scroll, false);
    }

    let current_scroll = clamp_sidebar_scroll_value(current_scroll, row_count, visible_rows);
    let next = if delta_rows > 0 {
        current_scroll.saturating_add(delta_rows as usize)
    } else {
        current_scroll.saturating_sub(delta_rows.unsigned_abs() as usize)
    };
    let next = next.min(row_count.saturating_sub(visible_rows));
    (next, next != current_scroll)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn session(status: SessionStatus, first_message: &str) -> MuxSession {
        MuxSession {
            key: 0,
            store_path: None,
            first_message: first_message.to_owned(),
            modified: None,
            status,
            handle: None,
        }
    }

    fn project(sessions: Vec<MuxSession>) -> MuxProject {
        MuxProject {
            cwd: PathBuf::from("/tmp/project"),
            sessions,
            selected: 0,
        }
    }

    #[test]
    fn status_maps_from_session_status() {
        assert_eq!(
            session_sidebar_status(&session(SessionStatus::Thinking, "")),
            Some(SidebarStatusKind::Active)
        );
        assert_eq!(
            session_sidebar_status(&session(SessionStatus::Crashed, "")),
            Some(SidebarStatusKind::Interrupted)
        );
        assert_eq!(
            session_sidebar_status(&session(SessionStatus::Idle, "")),
            None
        );
    }

    #[test]
    fn spinner_glyph_wraps_after_last_frame() {
        let now_ms = SIDEBAR_SPINNER_FRAME_MS * SIDEBAR_SPINNER_FRAMES.len() as u64;
        assert_eq!(
            sidebar_status_glyph(SidebarStatusKind::Active, now_ms),
            SIDEBAR_SPINNER_FRAMES[0]
        );
        assert_eq!(
            sidebar_status_glyph(SidebarStatusKind::Notification, now_ms),
            SIDEBAR_NOTIFICATION_GLYPH
        );
    }

    #[test]
    fn crashed_session_marks_project_interrupted() {
        let p = project(vec![
            session(SessionStatus::Thinking, "a"),
            session(SessionStatus::Crashed, "b"),
        ]);
        let rows = build_sidebar_rows(&[p], 0);
        assert_eq!(rows[0].status, Some(SidebarStatusKind::Interrupted));
    }

    #[test]
    fn empty_selected_project_gets_selector_on_header() {
        let rows = build_sidebar_rows(&[project(vec![])], 0);
        assert!(matches!(rows[0].kind, SidebarRowKind::Project(0)));
        assert!(rows[0].selector);
    }

    #[test]
    fn selected_session_gets_selector_not_header() {
        let rows = build_sidebar_rows(&[project(vec![session(SessionStatus::Idle, "hello")])], 0);
        assert!(!rows[0].selector);
        assert!(rows[1].selector);
        assert_eq!(rows[1].text, "hello");
    }

    #[test]
    fn hidden_unmaterialized_draft_does_not_render_session_row() {
        let rows = build_sidebar_rows(&[project(vec![session(SessionStatus::Idle, "")])], 0);
        assert_eq!(rows.len(), 1);
        assert!(matches!(rows[0].kind, SidebarRowKind::Project(0)));
        assert!(rows[0].selector);
    }

    #[test]
    fn draft_session_uses_placeholder_title() {
        let draft = session(SessionStatus::Idle, "");
        assert_eq!(session_sidebar_title(&draft), "new session");
    }

    #[test]
    fn selection_sync_keeps_project_header_visible() {
        assert_eq!(
            sync_sidebar_scroll_to_selection_span(
                0,
                32,
                5,
                SidebarSelectionSpan {
                    anchor_row: 10,
                    selected_row: 12,
                },
            ),
            8
        );
    }

    #[test]
    fn selection_sync_uses_sticky_header_space_for_deep_session() {
        let span = SidebarSelectionSpan {
            anchor_row: 10,
            selected_row: 15,
        };
        let scroll = sync_sidebar_scroll_to_selection_span(0, 32, 5, span);
        assert_eq!(scroll, 12);
        assert_eq!(sticky_sidebar_anchor_row(scroll, 5, span), Some(10));
        assert_eq!(sticky_sidebar_anchor_row(16, 5, span), None);
    }

    #[test]
    fn viewport_items_reserve_first_row_for_sticky_header() {
        let rows = build_sidebar_rows(
            &[project(
                (0..19)
                    .map(|i| session(SessionStatus::Idle, &i.to_string()))
                    .collect(),
            )],
            0,
        );
        let items = sidebar_viewport_items(&rows, 12, 5, Some(10));
        assert_eq!(items.len(), 5);
        assert_eq!(items[0].row_index, 10);
        assert!(items[0].sticky);
        assert_eq!(items[1].row_index, 12);
        assert_eq!(items[1].visible_row, 1);
    }
}
