//! Left-panel tool that lists claude sessions (running + recent history), marks
//! the ones currently open in a Warp pane, and on click jumps to that pane.
//!
//! Read-only navigator: clicking a session that isn't hosted by a Warp pane does
//! nothing (by design — see the session-navigator design spec). Session data is
//! read from `~/.claude` via [`session_index`]; "open in Warp" is recomputed at
//! render time from the live [`CLIAgentSessionsModel`] so a closed pane is never
//! stale-marked.

use std::collections::HashMap;
use std::ops::Range;

use chrono::{TimeZone, Utc};
use warp_core::ui::Icon;
use warpui::elements::{
    ConstrainedBox, Container, CornerRadius, CrossAxisAlignment, Element, Empty, Flex, Hoverable,
    MainAxisAlignment, MainAxisSize, MouseStateHandle, ParentElement, Radius, Shrinkable, Text,
    UniformList, UniformListState,
};
use warpui::platform::Cursor;
use warpui::{AppContext, Entity, SingletonEntity, View, ViewContext};

use crate::appearance::Appearance;
use crate::terminal::cli_agent_sessions::session_index::{
    self, ClaudeSessionEntry,
};
use crate::terminal::cli_agent_sessions::CLIAgentSessionsModel;
use crate::util::time_format::format_approx_duration_from_now_utc;
use crate::workspace::WorkspaceAction;

/// Recent sessions to surface. The data layer reads newest-first; truncation
/// past this is logged there.
const SESSION_CAP: usize = 50;

/// Fixed height per row so [`UniformList`] can virtualize the list.
const ROW_HEIGHT: f32 = 48.;

const ROW_HORIZONTAL_PADDING: f32 = 12.;
const ICON_SPACING: f32 = 8.;
/// Diameter of the "open in Warp" accent dot.
const MARKER_SIZE: f32 = 8.;

pub struct ClaudeSessionsView {
    entries: Vec<ClaudeSessionEntry>,
    list_state: UniformListState,
    /// Per-session hover state, keyed by session id and rebuilt on reload so it
    /// stays in sync with `entries`.
    row_states: HashMap<String, MouseStateHandle>,
}

impl ClaudeSessionsView {
    pub fn new(_ctx: &mut ViewContext<Self>) -> Self {
        let mut view = Self {
            entries: Vec::new(),
            list_state: UniformListState::new(),
            row_states: HashMap::new(),
        };
        view.reload();
        view
    }

    /// Re-reads `~/.claude` and rebuilds per-row hover state. Cheap (a handful of
    /// small files); safe to call on panel focus.
    fn reload(&mut self) {
        self.entries = match session_index::claude_home() {
            Some(home) => session_index::load_claude_sessions(&home, SESSION_CAP),
            None => Vec::new(),
        };
        // Rebuild hover state keyed by session id, preserving existing handles so
        // an in-progress hover survives a refresh.
        let mut row_states = HashMap::with_capacity(self.entries.len());
        for entry in &self.entries {
            let handle = self
                .row_states
                .remove(&entry.session_id)
                .unwrap_or_default();
            row_states.insert(entry.session_id.clone(), handle);
        }
        self.row_states = row_states;
    }

    /// Called when the left panel becomes focused/visible so the list reflects
    /// the latest on-disk state.
    pub fn on_left_panel_focused(&mut self, ctx: &mut ViewContext<Self>) {
        self.reload();
        ctx.notify();
    }
}

impl Entity for ClaudeSessionsView {
    type Event = ();
}

impl View for ClaudeSessionsView {
    fn ui_name() -> &'static str {
        "ClaudeSessionsView"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();

        if self.entries.is_empty() {
            return Container::new(
                Text::new_inline(
                    "No claude sessions found",
                    appearance.ui_font_family(),
                    appearance.ui_font_size(),
                )
                .with_color(theme.sub_text_color(theme.background()).into())
                .finish(),
            )
            .with_horizontal_padding(ROW_HORIZONTAL_PADDING)
            .with_vertical_padding(8.)
            .finish();
        }

        let entries = self.entries.clone();
        let row_states = self.row_states.clone();

        let list = UniformList::new(
            self.list_state.clone(),
            entries.len(),
            move |range: Range<usize>, app: &AppContext| {
                let entries = entries.clone();
                let row_states = row_states.clone();
                range
                    .filter_map(move |index| {
                        let entry = entries.get(index)?;
                        let mouse_state =
                            row_states.get(&entry.session_id).cloned().unwrap_or_default();
                        Some(render_row(entry, mouse_state, app))
                    })
                    .collect::<Vec<_>>()
                    .into_iter()
            },
        );

        Shrinkable::new(1.0, list.finish()).finish()
    }
}

fn render_row(
    entry: &ClaudeSessionEntry,
    mouse_state: MouseStateHandle,
    app: &AppContext,
) -> Box<dyn Element> {
    let appearance = Appearance::as_ref(app);
    let theme = appearance.theme();
    let font_family = appearance.ui_font_family();
    let font_size = appearance.ui_font_size();

    // "Open in Warp" is live state, recomputed every render so a closed pane is
    // never stale-marked.
    let open_view_id = CLIAgentSessionsModel::as_ref(app).find_view_for_session_id(&entry.session_id);
    let is_open_in_warp = open_view_id.is_some();

    let title_text = Text::new_inline(entry.title.clone(), font_family, font_size + 1.)
        .with_color(theme.main_text_color(theme.background()).into())
        .finish();

    // Leading icon + title, with an accent "open in Warp" dot when applicable.
    let icon_color = if is_open_in_warp {
        theme.accent()
    } else {
        theme.sub_text_color(theme.background())
    };
    let icon = ConstrainedBox::new(Icon::Terminal.to_warpui_icon(icon_color).finish())
        .with_width(font_size + 2.)
        .with_height(font_size + 2.)
        .finish();

    let mut title_row = Flex::row()
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_spacing(ICON_SPACING)
        .with_child(icon)
        .with_child(Shrinkable::new(1.0, title_text).finish());

    if is_open_in_warp {
        let dot = Container::new(Empty::new().finish())
            .with_background(theme.accent())
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(MARKER_SIZE / 2.)))
            .finish();
        let marker = ConstrainedBox::new(dot)
            .with_width(MARKER_SIZE)
            .with_height(MARKER_SIZE)
            .finish();
        title_row = title_row.with_child(marker);
    }

    let title_row = Shrinkable::new(1.0, title_row.finish()).finish();

    // Secondary line: friendly project path on the left, relative time on the right.
    let path_label = friendly_project_path(&entry.project_path);
    let path_text = Shrinkable::new(
        1.0,
        Text::new_inline(path_label, font_family, font_size - 1.)
            .with_color(theme.sub_text_color(theme.background()).into())
            .finish(),
    )
    .finish();

    let time_text = Text::new_inline(relative_time(entry.last_active_ms), font_family, font_size - 1.)
        .with_color(theme.sub_text_color(theme.background()).into())
        .finish();

    let secondary_row = Container::new(
        Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_child(path_text)
            .with_child(time_text)
            .finish(),
    )
    .with_padding_left(font_size + 2. + ICON_SPACING)
    .finish();

    let column = Flex::column()
        .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
        .with_child(title_row)
        .with_child(secondary_row)
        .finish();

    // Sessions not open in Warp are inert (no jump target); render them without a
    // pointer cursor or hover affordance.
    let can_jump = open_view_id.is_some();
    let hoverable = Hoverable::new(mouse_state, move |_| {
        let mut container = Container::new(column)
            .with_horizontal_padding(ROW_HORIZONTAL_PADDING)
            .with_vertical_padding(6.);
        if can_jump {
            container = container.with_background(theme.surface_overlay_1());
        }
        container.finish()
    });

    let hoverable = if let Some(view_id) = open_view_id {
        hoverable
            .with_cursor(Cursor::PointingHand)
            .on_click(move |ctx, _, _| {
                ctx.dispatch_typed_action(WorkspaceAction::FocusTerminalViewInWorkspace {
                    terminal_view_id: view_id,
                });
            })
            .finish()
    } else {
        hoverable.finish()
    };

    ConstrainedBox::new(hoverable)
        .with_min_height(ROW_HEIGHT)
        .with_height(ROW_HEIGHT)
        .finish()
}

/// Shows the last one or two components of a project path so worktrees stay
/// distinguishable without the full absolute path.
fn friendly_project_path(project_path: &str) -> String {
    if project_path.is_empty() {
        return String::new();
    }
    let trimmed = project_path.trim_end_matches('/');
    let components: Vec<&str> = trimmed.rsplit('/').filter(|s| !s.is_empty()).collect();
    match components.as_slice() {
        [] => trimmed.to_string(),
        [last] => last.to_string(),
        [last, parent, ..] => format!("{parent}/{last}"),
    }
}

/// Relative time ("5m ago"-style) from an epoch-ms timestamp. `0` (unknown) is
/// rendered blank rather than as a bogus "55 years ago".
fn relative_time(last_active_ms: u64) -> String {
    if last_active_ms == 0 {
        return String::new();
    }
    match Utc.timestamp_millis_opt(last_active_ms as i64).single() {
        Some(datetime) => format_approx_duration_from_now_utc(datetime),
        None => String::new(),
    }
}

#[cfg(test)]
#[path = "view_tests.rs"]
mod tests;
