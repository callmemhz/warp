//! Left-panel tool that lists claude sessions (running + recent history), marks
//! the ones currently open in a Warp pane, and on click jumps to that pane.
//!
//! Read-only navigator for left-click: clicking a session that isn't hosted by a
//! Warp pane does nothing (by design — see the session-navigator design spec).
//! Session data is read from `~/.claude` via [`session_index`]; "open in Warp" is
//! recomputed at render time from the live [`CLIAgentSessionsModel`] so a closed
//! pane is never stale-marked.
//!
//! Right-click opens a context menu on *any* row (open or not) with a "Rename"
//! option (plus "Clear name" when a custom name already exists). Custom names are
//! persisted Warp-side via [`session_names`] and override the displayed title.

use std::collections::HashMap;
use std::ops::Range;

use chrono::{TimeZone, Utc};
use pathfinder_geometry::vector::Vector2F;
use warp_core::ui::Icon;
use warpui::elements::{
    ChildAnchor, ChildView, ConstrainedBox, Container, CornerRadius, CrossAxisAlignment, Element,
    Empty, Fill as ElementFill, Flex, Hoverable, MainAxisAlignment, MainAxisSize, MouseStateHandle,
    OffsetPositioning, ParentAnchor, ParentElement, ParentOffsetBounds, Radius, SavePosition,
    Shrinkable, Stack, Text, UniformList, UniformListState,
};
use warpui::platform::Cursor;
use warpui::ui_components::components::{UiComponent, UiComponentStyles};
use warpui::ui_components::text_input::TextInput;
use warpui::{
    AppContext, Entity, EntityId, SingletonEntity, TypedActionView, View, ViewContext, ViewHandle,
};

use crate::appearance::Appearance;
use crate::editor::{
    EditorView, Event as EditorEvent, SingleLineEditorOptions, TextOptions,
};
use crate::menu::{Event as MenuEvent, Menu, MenuItemFields};
use crate::terminal::cli_agent_sessions::session_index::{self, ClaudeSessionEntry};
use crate::terminal::cli_agent_sessions::session_names;
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
/// Width of the right-click context menu.
const CONTEXT_MENU_WIDTH: f32 = 160.;

/// Stable id for the list's saved position, used to translate a right-click's
/// window coordinates into an offset for the menu overlay.
fn list_position_id(view_id: EntityId) -> String {
    format!("claude_sessions_list_{view_id}")
}

/// Tracks the open context menu: which session it's for and where the right
/// click took place (offset from the list's origin).
#[derive(Clone)]
struct MenuState {
    session_id: String,
    position: Vector2F,
}

#[derive(Clone, Debug)]
pub enum ClaudeSessionsAction {
    /// Jump focus to the Warp pane currently hosting a session.
    FocusPane { terminal_view_id: EntityId },
    /// Open the right-click context menu for a session at a position relative to
    /// the list's origin.
    OpenContextMenu {
        session_id: String,
        position: Vector2F,
    },
    /// Begin renaming a session: opens the inline editor seeded with the current
    /// display title.
    StartRename { session_id: String },
    /// Clear a session's custom name (revert to claude's title).
    ClearName { session_id: String },
}

pub struct ClaudeSessionsView {
    view_id: EntityId,
    entries: Vec<ClaudeSessionEntry>,
    /// `session_id` → custom Warp-side name. Reloaded alongside `entries` and
    /// used to override the displayed title.
    names: HashMap<String, String>,
    list_state: UniformListState,
    /// Per-session hover state, keyed by session id and rebuilt on reload so it
    /// stays in sync with `entries`.
    row_states: HashMap<String, MouseStateHandle>,
    /// Right-click context menu, rendered as a positioned overlay when open.
    context_menu: ViewHandle<Menu<ClaudeSessionsAction>>,
    /// `Some` while a context menu is open.
    menu_state: Option<MenuState>,
    /// Inline single-line editor reused for renaming any row.
    rename_editor: ViewHandle<EditorView>,
    /// `Some(session_id)` while that row is being renamed inline.
    renaming: Option<String>,
}

impl ClaudeSessionsView {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        let context_menu = ctx.add_typed_action_view(|_| {
            Menu::new()
                .prevent_interaction_with_other_elements()
                .with_width(CONTEXT_MENU_WIDTH)
        });
        ctx.subscribe_to_view(&context_menu, |me, _, event, ctx| match event {
            MenuEvent::Close { .. } => {
                me.menu_state = None;
                ctx.notify();
            }
            MenuEvent::ItemSelected | MenuEvent::ItemHovered => {}
        });

        let rename_editor = ctx.add_typed_action_view(|ctx| {
            let appearance = Appearance::as_ref(ctx);
            let options = SingleLineEditorOptions {
                text: TextOptions::ui_text(Some(appearance.ui_font_size() + 1.), appearance),
                select_all_on_focus: true,
                clear_selections_on_blur: true,
                ..Default::default()
            };
            EditorView::single_line(options, ctx)
        });
        ctx.subscribe_to_view(&rename_editor, |me, _, event, ctx| {
            me.handle_rename_editor_event(event, ctx);
        });

        // Re-render whenever a CLI agent session changes (starts, stops, becomes
        // active) so the "open in Warp" marker reflects live state. The marker is
        // computed at render time from `CLIAgentSessionsModel`; without this the
        // panel would stay stale until the user refocused it — e.g. after a
        // restart, when the auto-resumed `claude --continue` only reports its
        // session a few seconds later.
        ctx.subscribe_to_model(&CLIAgentSessionsModel::handle(ctx), |_, _, _, ctx| {
            ctx.notify();
        });

        let mut view = Self {
            view_id: ctx.view_id(),
            entries: Vec::new(),
            names: HashMap::new(),
            list_state: UniformListState::new(),
            row_states: HashMap::new(),
            context_menu,
            menu_state: None,
            rename_editor,
            renaming: None,
        };
        view.reload();
        view
    }

    /// Re-reads `~/.claude` and the custom names map, and rebuilds per-row hover
    /// state. Cheap (a handful of small files); safe to call on panel focus.
    fn reload(&mut self) {
        self.entries = match session_index::claude_home() {
            Some(home) => session_index::load_claude_sessions(&home, SESSION_CAP),
            None => Vec::new(),
        };
        self.names = session_names::load();
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

    /// Displayed title for a session: the custom Warp-side name when present,
    /// otherwise claude's own title.
    fn display_title<'a>(&'a self, entry: &'a ClaudeSessionEntry) -> &'a str {
        self.names
            .get(&entry.session_id)
            .map(String::as_str)
            .unwrap_or(entry.title.as_str())
    }

    /// Called when the left panel becomes focused/visible so the list reflects
    /// the latest on-disk state.
    pub fn on_left_panel_focused(&mut self, ctx: &mut ViewContext<Self>) {
        self.reload();
        ctx.notify();
    }

    fn begin_rename(&mut self, session_id: &str, ctx: &mut ViewContext<Self>) {
        let Some(entry) = self
            .entries
            .iter()
            .find(|e| e.session_id == session_id)
            .cloned()
        else {
            return;
        };
        // Close the menu and seed the editor with the current display title.
        self.menu_state = None;
        self.renaming = Some(session_id.to_string());
        let seed = self.display_title(&entry).to_string();
        self.rename_editor.update(ctx, |editor, ctx| {
            editor.clear_buffer_and_reset_undo_stack(ctx);
            editor.insert_selected_text(&seed, ctx);
        });
        ctx.focus(&self.rename_editor);
        ctx.notify();
    }

    fn handle_rename_editor_event(&mut self, event: &EditorEvent, ctx: &mut ViewContext<Self>) {
        if self.renaming.is_none() {
            return;
        }
        match event {
            EditorEvent::Blurred | EditorEvent::Enter => self.finish_rename(ctx),
            EditorEvent::Escape => self.cancel_rename(ctx),
            _ => {}
        }
    }

    fn finish_rename(&mut self, ctx: &mut ViewContext<Self>) {
        let Some(session_id) = self.renaming.take() else {
            return;
        };
        // An empty/whitespace name clears the custom name (handled by `set`).
        let text = self.rename_editor.as_ref(ctx).buffer_text(ctx);
        session_names::set(&session_id, &text);
        self.rename_editor.update(ctx, |editor, ctx| {
            editor.clear_buffer_and_reset_undo_stack(ctx);
        });
        self.reload();
        ctx.notify();
    }

    fn cancel_rename(&mut self, ctx: &mut ViewContext<Self>) {
        if self.renaming.take().is_some() {
            self.rename_editor.update(ctx, |editor, ctx| {
                editor.clear_buffer_and_reset_undo_stack(ctx);
            });
            ctx.notify();
        }
    }
}

impl Entity for ClaudeSessionsView {
    type Event = ();
}

impl TypedActionView for ClaudeSessionsView {
    type Action = ClaudeSessionsAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            ClaudeSessionsAction::FocusPane { terminal_view_id } => {
                ctx.dispatch_typed_action(&WorkspaceAction::FocusTerminalViewInWorkspace {
                    terminal_view_id: *terminal_view_id,
                });
            }
            ClaudeSessionsAction::OpenContextMenu {
                session_id,
                position,
            } => {
                // Toggle off if it's already open for this row.
                let already_open = self
                    .menu_state
                    .as_ref()
                    .is_some_and(|s| &s.session_id == session_id);
                if already_open {
                    self.menu_state = None;
                    ctx.notify();
                    return;
                }

                let has_custom_name = self.names.contains_key(session_id);
                let mut items = vec![MenuItemFields::new("Rename")
                    .with_on_select_action(ClaudeSessionsAction::StartRename {
                        session_id: session_id.clone(),
                    })
                    .into_item()];
                if has_custom_name {
                    items.push(
                        MenuItemFields::new("Clear name")
                            .with_on_select_action(ClaudeSessionsAction::ClearName {
                                session_id: session_id.clone(),
                            })
                            .into_item(),
                    );
                }
                self.context_menu.update(ctx, |menu, ctx| {
                    menu.set_items(items, ctx);
                });
                self.menu_state = Some(MenuState {
                    session_id: session_id.clone(),
                    position: *position,
                });
                ctx.notify();
            }
            ClaudeSessionsAction::StartRename { session_id } => {
                self.begin_rename(session_id, ctx);
            }
            ClaudeSessionsAction::ClearName { session_id } => {
                self.menu_state = None;
                session_names::set(session_id, "");
                self.reload();
                ctx.notify();
            }
        }
    }
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
        let names = self.names.clone();
        let row_states = self.row_states.clone();
        let renaming = self.renaming.clone();
        let rename_editor = self.rename_editor.clone();
        let position_id = list_position_id(self.view_id);
        let list_position_id_for_rows = position_id.clone();

        let list = UniformList::new(
            self.list_state.clone(),
            entries.len(),
            move |range: Range<usize>, app: &AppContext| {
                let entries = entries.clone();
                let names = names.clone();
                let row_states = row_states.clone();
                let renaming = renaming.clone();
                let rename_editor = rename_editor.clone();
                let list_position_id_for_rows = list_position_id_for_rows.clone();
                range
                    .filter_map(move |index| {
                        let entry = entries.get(index)?;
                        let mouse_state =
                            row_states.get(&entry.session_id).cloned().unwrap_or_default();
                        let display_title = names
                            .get(&entry.session_id)
                            .map(String::as_str)
                            .unwrap_or(entry.title.as_str());
                        let is_renaming =
                            renaming.as_deref() == Some(entry.session_id.as_str());
                        Some(render_row(RowProps {
                            entry,
                            display_title,
                            is_renaming,
                            rename_editor: &rename_editor,
                            list_position_id: &list_position_id_for_rows,
                            mouse_state,
                            app,
                        }))
                    })
                    .collect::<Vec<_>>()
                    .into_iter()
            },
        );

        let positioned_list = SavePosition::new(
            Shrinkable::new(1.0, list.finish()).finish(),
            &position_id,
        )
        .finish();

        let mut stack = Stack::new().with_child(positioned_list);

        if let Some(menu_state) = &self.menu_state {
            stack.add_positioned_overlay_child(
                ChildView::new(&self.context_menu).finish(),
                OffsetPositioning::offset_from_parent(
                    menu_state.position,
                    ParentOffsetBounds::WindowByPosition,
                    ParentAnchor::TopLeft,
                    ChildAnchor::TopLeft,
                ),
            );
        }

        stack.finish()
    }
}

struct RowProps<'a> {
    entry: &'a ClaudeSessionEntry,
    display_title: &'a str,
    is_renaming: bool,
    rename_editor: &'a ViewHandle<EditorView>,
    list_position_id: &'a str,
    mouse_state: MouseStateHandle,
    app: &'a AppContext,
}

fn render_row(props: RowProps<'_>) -> Box<dyn Element> {
    let RowProps {
        entry,
        display_title,
        is_renaming,
        rename_editor,
        list_position_id,
        mouse_state,
        app,
    } = props;

    let appearance = Appearance::as_ref(app);
    let theme = appearance.theme();
    let font_family = appearance.ui_font_family();
    let font_size = appearance.ui_font_size();

    // "Open in Warp" is live state, recomputed every render so a closed pane is
    // never stale-marked. Match by exact session id first, then fall back to
    // cwd: `claude --continue`/`--resume` spawns a new session id but keeps the
    // same directory, so the original conversation still resolves to its pane.
    let sessions_model = CLIAgentSessionsModel::as_ref(app);
    let open_view_id = sessions_model
        .find_view_for_session_id(&entry.session_id)
        .or_else(|| sessions_model.find_view_for_cwd(&entry.project_path));
    let is_open_in_warp = open_view_id.is_some();

    // While renaming, the title is replaced by the inline editor.
    let title_element: Box<dyn Element> = if is_renaming {
        let editor_line_height = rename_editor
            .as_ref(app)
            .line_height(app.font_cache(), appearance);
        TextInput::new(
            rename_editor.clone(),
            UiComponentStyles::default()
                .set_height(editor_line_height)
                .set_background(ElementFill::None)
                .set_border_radius(CornerRadius::with_all(Radius::Pixels(0.)))
                .set_border_width(0.),
        )
        .build()
        .finish()
    } else {
        Text::new_inline(display_title.to_string(), font_family, font_size + 1.)
            .with_color(theme.main_text_color(theme.background()).into())
            .finish()
    };

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
        .with_child(Shrinkable::new(1.0, title_element).finish());

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

    // Sessions not open in Warp are inert on left-click (no jump target); render
    // them without a pointer cursor or hover affordance. Right-click works on all
    // rows regardless.
    let can_jump = open_view_id.is_some();
    let session_id = entry.session_id.clone();
    let menu_list_position_id = list_position_id.to_string();
    let hoverable = Hoverable::new(mouse_state, move |_| {
        let mut container = Container::new(column)
            .with_horizontal_padding(ROW_HORIZONTAL_PADDING)
            .with_vertical_padding(6.);
        if can_jump {
            container = container.with_background(theme.surface_overlay_1());
        }
        container.finish()
    })
    .on_right_click(move |ctx, _, position| {
        let Some(parent_bounds) = ctx.element_position_by_id(&menu_list_position_id) else {
            log::warn!(
                "Could not retrieve claude sessions list position for context menu display."
            );
            return;
        };
        let offset = position - parent_bounds.origin();
        ctx.dispatch_typed_action(ClaudeSessionsAction::OpenContextMenu {
            session_id: session_id.clone(),
            position: offset,
        });
    });

    let hoverable = if let Some(view_id) = open_view_id {
        hoverable
            .with_cursor(Cursor::PointingHand)
            .on_click(move |ctx, _, _| {
                ctx.dispatch_typed_action(ClaudeSessionsAction::FocusPane {
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
