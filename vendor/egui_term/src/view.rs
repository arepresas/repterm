use alacritty_terminal::index::Point as TerminalGridPoint;
use alacritty_terminal::term::cell;
use alacritty_terminal::term::TermMode;
use alacritty_terminal::vte::ansi::{Color, NamedColor};
use egui::epaint::RectShape;
use egui::{CornerRadius, Key};
use egui::Modifiers;
use egui::MouseWheelUnit;
use egui::Shape;
use egui::Widget;
use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Response, Stroke, Vec2};
use egui::{Id, PointerButton};

use crate::backend::BackendCommand;
use crate::backend::TerminalBackend;
use crate::backend::{LinkAction, MouseButton, SelectionType};
use crate::bindings::Binding;
use crate::bindings::{BindingAction, BindingsLayout, InputKind};
use crate::font::TerminalFont;
use crate::theme::TerminalTheme;
use crate::types::Size;

const EGUI_TERM_WIDGET_ID_PREFIX: &str = "egui_term::instance::";

#[derive(Debug, Clone)]
enum InputAction {
    BackendCall(BackendCommand),
    WriteToClipboard(String),
    Ignore,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PointerRoute {
    Select,
    MouseReport,
    Ignore,
}

#[derive(Clone)]
pub struct TerminalViewState {
    active_route: Option<PointerRoute>,
    scroll_pixels: f32,
    current_mouse_position_on_grid: TerminalGridPoint,
    line_input: Option<String>,
    bang_draft: Option<String>,
    bang_cursor: usize,
    bang_committed: Option<String>,
}

impl Default for TerminalViewState {
    fn default() -> Self {
        Self {
            active_route: None,
            scroll_pixels: 0.0,
            current_mouse_position_on_grid: TerminalGridPoint::default(),
            line_input: Some(String::new()),
            bang_draft: None,
            bang_cursor: 0,
            bang_committed: None,
        }
    }
}

pub struct TerminalView<'a> {
    widget_id: Id,
    has_focus: bool,
    size: Vec2,
    backend: &'a mut TerminalBackend,
    font: TerminalFont,
    theme: TerminalTheme,
    bindings_layout: BindingsLayout,
    bang_submit: Option<&'a mut Option<String>>,
    bang_draft: Option<&'a mut Option<String>>,
    agent_history: Option<&'a [String]>,
    inline_lines: Option<&'a [String]>,
    agent_conversation: bool,
    release_overlay: bool,
    clear_draft: bool,
    selection_requires_shift: bool,
}

impl Widget for TerminalView<'_> {
    fn ui(self, ui: &mut egui::Ui) -> Response {
        let (layout, painter) =
            ui.allocate_painter(self.size, egui::Sense::click());

        let widget_id = self.widget_id;
        let mut state = ui.memory(|m| {
            m.data
                .get_temp::<TerminalViewState>(widget_id)
                .unwrap_or_default()
        });

        self.focus(&layout)
            .resize(&layout)
            .process_input(&layout, &mut state)
            .show(&mut state, &layout, &painter);

        ui.memory_mut(|m| m.data.insert_temp(widget_id, state));
        layout
    }
}

impl<'a> TerminalView<'a> {
    pub fn new(ui: &mut egui::Ui, backend: &'a mut TerminalBackend) -> Self {
        let widget_id = ui.make_persistent_id(format!(
            "{}{}",
            EGUI_TERM_WIDGET_ID_PREFIX, backend.id
        ));

        Self {
            widget_id,
            has_focus: false,
            size: ui.available_size(),
            backend,
            font: TerminalFont::default(),
            theme: TerminalTheme::default(),
            bindings_layout: BindingsLayout::new(),
            bang_submit: None,
            bang_draft: None,
            agent_history: None,
            inline_lines: None,
            agent_conversation: false,
            release_overlay: false,
            clear_draft: false,
            selection_requires_shift: false,
        }
    }

    #[inline]
    pub fn set_bang_submit(mut self, target: &'a mut Option<String>) -> Self {
        self.bang_submit = Some(target);
        self
    }

    #[inline]
    pub fn set_bang_draft(mut self, target: &'a mut Option<String>) -> Self {
        self.bang_draft = Some(target);
        self
    }

    /// Entries repterm added to the shell history. Used to tell a recalled
    /// `!question` apart from an ordinary shell command containing `!`.
    #[inline]
    pub fn set_agent_history(mut self, entries: &'a [String]) -> Self {
        self.agent_history = Some(entries);
        self
    }

    #[inline]
    pub fn set_inline_lines(mut self, lines: &'a [String]) -> Self {
        self.inline_lines = Some(lines);
        self
    }

    #[inline]
    pub fn set_agent_conversation(mut self, enabled: bool) -> Self {
        self.agent_conversation = enabled;
        self
    }

    #[inline]
    pub fn set_release_overlay(mut self, release: bool) -> Self {
        self.release_overlay = release;
        self
    }

    #[inline]
    pub fn set_clear_draft(mut self, clear: bool) -> Self {
        self.clear_draft = clear;
        self
    }

    /// Require Shift for repterm selection. Without it, plain drag goes to
    /// apps that capture the mouse instead of selecting terminal text.
    #[inline]
    pub fn set_selection_requires_shift(mut self, enabled: bool) -> Self {
        self.selection_requires_shift = enabled;
        self
    }

    #[inline]
    pub fn set_theme(mut self, theme: TerminalTheme) -> Self {
        self.theme = theme;
        self
    }

    #[inline]
    pub fn set_font(mut self, font: TerminalFont) -> Self {
        self.font = font;
        self
    }

    #[inline]
    pub fn set_focus(mut self, has_focus: bool) -> Self {
        self.has_focus = has_focus;
        self
    }

    #[inline]
    pub fn set_size(mut self, size: Vec2) -> Self {
        self.size = size;
        self
    }

    #[inline]
    pub fn add_bindings(
        mut self,
        bindings: Vec<(Binding<InputKind>, BindingAction)>,
    ) -> Self {
        self.bindings_layout.add_bindings(bindings);
        self
    }

    fn focus(self, layout: &Response) -> Self {
        if self.has_focus {
            layout.request_focus();
        } else {
            layout.surrender_focus();
        }

        self
    }

    fn resize(self, layout: &Response) -> Self {
        self.backend.process_command(BackendCommand::Resize(
            Size::from(layout.rect.size()),
            self.font.font_measure(&layout.ctx),
        ));

        self
    }

    fn process_input(
        mut self,
        layout: &Response,
        state: &mut TerminalViewState,
    ) -> Self {
        // repterm: keyboard must work after tab switches even if the pointer
        // is over the tab bar. Focus is requested explicitly by the host.
        if !layout.has_focus() {
            return self;
        }

        if self.release_overlay || self.clear_draft {
            state.bang_committed = None;
        }
        if self.clear_draft {
            state.bang_draft = None;
            state.bang_cursor = 0;
        }

        let recalled_bang = match self.agent_history {
            Some(entries) => recalled_agent_line(self.backend.sync(), entries),
            None => None,
        };

        let modifiers = layout.ctx.input(|i| i.modifiers);
        let events = layout.ctx.input(|i| i.events.clone());
        for event in events {
            match track_bang_submit(
                &event,
                state,
                self.agent_conversation,
                recalled_bang.as_deref(),
            ) {
                BangAction::Consume => continue,
                BangAction::Submit(question) => {
                    if let Some(target) = self.bang_submit.as_deref_mut() {
                        *target = Some(question);
                    }
                    continue;
                }
                BangAction::SubmitRecalled(question) => {
                    self.backend
                        .process_command(BackendCommand::Write(vec![0x15]));
                    if let Some(target) = self.bang_submit.as_deref_mut() {
                        *target = Some(question);
                    }
                    continue;
                }
                BangAction::PassThrough => {}
            }

            let mut input_actions = vec![];

            match event {
                egui::Event::Text(_)
                | egui::Event::Key { .. }
                | egui::Event::Copy
                | egui::Event::Paste(_) => {
                    input_actions.push(process_keyboard_event(
                        event,
                        self.backend,
                        &self.bindings_layout,
                        modifiers,
                    ))
                },
                egui::Event::MouseWheel {
                    unit,
                    delta,
                    modifiers: wheel_modifiers,
                    ..
                } => input_actions.extend(process_mouse_wheel(
                    state,
                    self.backend,
                    layout,
                    self.font.font_type().size,
                    unit,
                    delta,
                    wheel_modifiers,
                )),
                egui::Event::PointerButton {
                    button,
                    pressed,
                    modifiers,
                    pos,
                    ..
                } => input_actions.push(process_button_click(
                    state,
                    layout,
                    self.backend,
                    &self.bindings_layout,
                    button,
                    pos,
                    &modifiers,
                    pressed,
                    self.selection_requires_shift,
                )),
                egui::Event::PointerMoved(pos) => {
                    input_actions = process_mouse_move(
                        state,
                        layout,
                        self.backend,
                        pos,
                        &modifiers,
                    )
                },
                _ => {},
            };

            for action in input_actions {
                match action {
                    InputAction::BackendCall(cmd) => {
                        self.backend.process_command(cmd);
                    },
                    InputAction::WriteToClipboard(data) => {
                        if !data.trim().is_empty() {
                            // repterm: the app listens for this command to show
                            // a "Copied to clipboard" confirmation.
                            layout
                                .ctx
                                .send_cmd(egui::OutputCommand::TextSelectionSettled(data.clone()));
                        }
                        layout.ctx.copy_text(data);
                    },
                    InputAction::Ignore => {},
                }
            }
        }

        if let Some(target) = self.bang_draft.as_deref_mut() {
            *target = state.bang_draft.clone();
        }

        self
    }

    fn show(
        self,
        state: &mut TerminalViewState,
        layout: &Response,
        painter: &Painter,
    ) {
        let content = self.backend.sync();
        let layout_min = layout.rect.min;
        let layout_max = layout.rect.max;
        let cell_height = content.terminal_size.cell_height as f32;
        let cell_width = content.terminal_size.cell_width as f32;
        let global_bg =
            self.theme.get_color(Color::Named(NamedColor::Background));
        let cursor = content.grid.cursor.point;
        let cursor_line = cursor.line.0 + content.grid.display_offset() as i32;
        let indicator_active = agent_indicator_active(
            self.agent_conversation,
            state.bang_draft.is_some(),
            state.bang_committed.is_some(),
        );
        let cursor_visible = cursor_row_visible(cursor_line, cell_height, layout.rect.height());
        let indicator_column = if indicator_active && cursor_visible {
            indicator_column(content.grid.display_iter().filter_map(|indexed| {
                (indexed.point.line == cursor.line
                    && indexed.point.column.0 < cursor.column.0)
                    .then_some((
                        indexed.point.column.0,
                        indexed.c,
                        !indexed.cell.flags.intersects(
                            cell::Flags::WIDE_CHAR | cell::Flags::WIDE_CHAR_SPACER,
                        ),
                    ))
            }))
        } else {
            None
        };
        let mut indicator_color = None;

        let mut shapes = vec![Shape::Rect(RectShape::filled(
            Rect::from_min_max(layout_min, layout_max),
            CornerRadius::ZERO,
            global_bg,
        ))];

        for indexed in content.grid.display_iter() {
            let flags = indexed.cell.flags;
            let is_wide_char_spacer =
                flags.contains(cell::Flags::WIDE_CHAR_SPACER);
            if is_wide_char_spacer {
                continue;
            }

            let is_app_cursor_mode =
                content.terminal_mode.contains(TermMode::APP_CURSOR);
            let is_wide_char = flags.contains(cell::Flags::WIDE_CHAR);
            let is_inverse = flags.contains(cell::Flags::INVERSE);
            let is_dim =
                flags.intersects(cell::Flags::DIM | cell::Flags::DIM_BOLD);
            let is_selected = content
                .selectable_range
                .is_some_and(|r| r.contains(indexed.point));
            let is_hovered_hyperling =
                content.hovered_hyperlink.as_ref().is_some_and(|r| {
                    r.contains(&indexed.point)
                        && r.contains(&state.current_mouse_position_on_grid)
                });

            let x = layout_min.x + (cell_width * indexed.point.column.0 as f32);
            let line_num =
                indexed.point.line.0 + content.grid.display_offset() as i32;
            let y = layout_min.y + (cell_height * line_num as f32);

            let mut fg = self.theme.get_color(indexed.fg);
            let mut bg = self.theme.get_color(indexed.bg);
            let cell_width = if is_wide_char {
                cell_width * 2.0
            } else {
                cell_width
            };

            if is_dim {
                fg = fg.linear_multiply(0.7);
            }

            if is_inverse || is_selected {
                std::mem::swap(&mut fg, &mut bg);
            }
            let is_indicator = indexed.point.line == cursor.line
                && indicator_column == Some(indexed.point.column.0);
            if is_indicator {
                indicator_color = Some((fg, bg));
            }

            if global_bg != bg {
                shapes.push(Shape::Rect(RectShape::filled(
                    Rect::from_min_size(
                        Pos2::new(x, y),
                        // + 1.0 is to fill grid border
                        Vec2::new(cell_width + 1., cell_height + 1.),
                    ),
                    CornerRadius::ZERO,
                    bg,
                )));
            }

            // Handle hovered hyperlink underline
            if is_hovered_hyperling {
                let underline_height = y + cell_height;
                shapes.push(Shape::LineSegment {
                    points: [
                        Pos2::new(x, underline_height),
                        Pos2::new(x + cell_width, underline_height),
                    ],
                    stroke: Stroke::new(cell_height * 0.15, fg).into(),
                });
            }

            // Handle cursor rendering
            if content.grid.cursor.point == indexed.point && state.bang_draft.is_none() {
                let cursor_color = self.theme.get_color(content.cursor.fg);
                shapes.push(Shape::Rect(RectShape::filled(
                    Rect::from_min_size(
                        Pos2::new(x, y),
                        Vec2::new(cell_width, cell_height),
                    ),
                    CornerRadius::default(),
                    cursor_color,
                )));
            }

            // Draw text content
            if !is_indicator && indexed.c != ' ' && indexed.c != '\t' {
                if content.grid.cursor.point == indexed.point
                    && is_app_cursor_mode
                {
                    std::mem::swap(&mut fg, &mut bg);
                }

                shapes.push(painter.fonts_mut(|fonts| Shape::text(
                    fonts,
                    Pos2 {
                        x: x + (cell_width / 2.0),
                        y,
                    },
                    Align2::CENTER_TOP,
                    indexed.c,
                    self.font.font_type(),
                    fg,
                )));
            }
        }

        let cursor_x = layout_min.x + cell_width * cursor.column.0 as f32;
        let cursor_y = layout_min.y + cell_height * cursor_line as f32;
        let text_color = self.theme.get_color(Color::Named(NamedColor::Foreground));
        if let Some(draft) = state.bang_draft.as_deref() {
            push_terminal_line(
                &mut shapes,
                painter,
                self.font.font_type(),
                text_color,
                cursor_x,
                cursor_y,
                cell_width,
                draft,
            );
            let cursor_color = self.theme.get_color(content.cursor.fg);
            shapes.push(Shape::Rect(RectShape::filled(
                Rect::from_min_size(
                    Pos2::new(
                        cursor_x + state.bang_cursor.min(draft.chars().count()) as f32 * cell_width,
                        cursor_y,
                    ),
                    Vec2::new(cell_width, cell_height),
                ),
                CornerRadius::default(),
                cursor_color,
            )));
        }
        if let Some(committed) = state.bang_committed.as_deref() {
            let response = self.inline_lines.unwrap_or(&[]);
            let rows = 1 + response.len();
            shapes.push(Shape::Rect(RectShape::filled(
                Rect::from_min_size(
                    Pos2::new(cursor_x, cursor_y),
                    Vec2::new((layout_max.x - cursor_x).max(0.0), cell_height),
                ),
                CornerRadius::ZERO,
                global_bg,
            )));
            if rows > 1 {
                shapes.push(Shape::Rect(RectShape::filled(
                    Rect::from_min_size(
                        Pos2::new(layout_min.x, cursor_y + cell_height),
                        Vec2::new(layout.rect.width(), (rows - 1) as f32 * cell_height),
                    ),
                    CornerRadius::ZERO,
                    global_bg,
                )));
            }
            push_terminal_line(
                &mut shapes,
                painter,
                self.font.font_type(),
                text_color,
                cursor_x,
                cursor_y,
                cell_width,
                committed,
            );
            for (row, line) in response.iter().enumerate() {
                push_terminal_line(
                    &mut shapes,
                    painter,
                    self.font.font_type(),
                    text_color,
                    layout_min.x,
                    cursor_y + (row as f32 + 1.0) * cell_height,
                    cell_width,
                    line,
                );
            }
        }

        painter.with_clip_rect(layout.rect).extend(shapes);
        if let (Some(column), Some((color, background))) = (indicator_column, indicator_color) {
            let rect = Rect::from_min_size(
                Pos2::new(layout_min.x + column as f32 * cell_width, cursor_y),
                Vec2::new(cell_width, cell_height),
            );
            paint_robot(&painter.with_clip_rect(rect.intersect(layout.rect)), rect, color, background);
        }
    }
}

fn agent_indicator_active(conversation: bool, draft: bool, committed: bool) -> bool {
    conversation || draft || committed
}

fn cursor_row_visible(line: i32, cell_height: f32, viewport_height: f32) -> bool {
    line >= 0 && (line as f32 + 1.0) * cell_height <= viewport_height
}

// Replace only the leftmost prompt symbol, never a later cell or prompt text.
fn indicator_column(cells: impl Iterator<Item = (usize, char, bool)>) -> Option<usize> {
    let (column, ch, safe) = cells
        .filter(|(_, ch, _)| !ch.is_whitespace())
        .min_by_key(|(column, _, _)| *column)?;
    (safe && matches!(ch, '❯' | '➜' | '>' | '→' | '▶' | '➤')).then_some(column)
}

fn paint_robot(painter: &Painter, cell: Rect, color: Color32, eye_color: Color32) {
    let point = |x: f32, y: f32| Pos2::new(
        cell.min.x + cell.width() * x,
        cell.min.y + cell.height() * y,
    );
    let stroke = Stroke::new((cell.width() * 0.13).max(1.0), color);
    painter.line_segment([point(0.5, 0.13), point(0.5, 0.3)], stroke);
    painter.circle_filled(point(0.5, 0.13), (cell.width() * 0.1).max(0.7), color);
    painter.rect_filled(Rect::from_min_max(point(0.06, 0.3), point(0.94, 0.83)), 1, color);
    painter.circle_filled(point(0.32, 0.5), (cell.width() * 0.1).max(0.7), eye_color);
    painter.circle_filled(point(0.68, 0.5), (cell.width() * 0.1).max(0.7), eye_color);
    painter.line_segment([point(0.3, 0.69), point(0.7, 0.69)], Stroke::new(1.0_f32, eye_color));
}

fn push_terminal_line(
    shapes: &mut Vec<Shape>,
    painter: &Painter,
    font: FontId,
    color: Color32,
    x: f32,
    y: f32,
    cell_width: f32,
    text: &str,
) {
    for (index, ch) in text.chars().enumerate() {
        if ch == ' ' || ch == '\t' {
            continue;
        }
        shapes.push(painter.fonts_mut(|fonts| Shape::text(
            fonts,
            Pos2::new(x + index as f32 * cell_width + cell_width / 2.0, y),
            Align2::CENTER_TOP,
            ch,
            font.clone(),
            color,
        )));
    }
}

fn edit_bang_draft(event: &egui::Event, state: &mut TerminalViewState) {
    let Some(draft) = state.bang_draft.as_deref() else {
        return;
    };
    let cursor = state.bang_cursor.min(draft.chars().count());
    let edited = match event {
        egui::Event::Text(text) | egui::Event::Paste(text)
            if !text.chars().all(|ch| ch == '\n' || ch == '\r') =>
        {
            Some(insert_at(draft, cursor, text))
        }
        egui::Event::Key {
            key: Key::Backspace,
            pressed: true,
            modifiers,
            ..
        } if modifiers.command || modifiers.ctrl => Some(kill_word_before(draft, cursor)),
        egui::Event::Key {
            key: Key::Backspace,
            pressed: true,
            ..
        } => Some(delete_before(draft, cursor)),
        egui::Event::Key {
            key: Key::Delete,
            pressed: true,
            ..
        } => Some(delete_after(draft, cursor)),
        egui::Event::Key {
            key: Key::ArrowLeft,
            pressed: true,
            modifiers,
            ..
        } if modifiers.command || modifiers.ctrl => Some((draft.to_owned(), word_left(draft, cursor))),
        egui::Event::Key {
            key: Key::ArrowRight,
            pressed: true,
            modifiers,
            ..
        } if modifiers.command || modifiers.ctrl => Some((draft.to_owned(), word_right(draft, cursor))),
        egui::Event::Key {
            key: Key::ArrowLeft,
            pressed: true,
            ..
        } => Some((draft.to_owned(), cursor.saturating_sub(1))),
        egui::Event::Key {
            key: Key::ArrowRight,
            pressed: true,
            ..
        } => Some((draft.to_owned(), (cursor + 1).min(draft.chars().count()))),
        egui::Event::Key {
            key: Key::Home,
            pressed: true,
            ..
        } => Some((draft.to_owned(), 0)),
        egui::Event::Key {
            key: Key::End,
            pressed: true,
            ..
        } => Some((draft.to_owned(), draft.chars().count())),
        egui::Event::Key {
            key: Key::Escape,
            pressed: true,
            ..
        } => Some((String::new(), 0)),
        _ => None,
    };
    if let Some((text, cursor)) = edited {
        state.bang_cursor = cursor;
        state.bang_draft = Some(text);
    }
}

fn insert_at(text: &str, cursor: usize, insert: &str) -> (String, usize) {
    let at = char_byte(text, cursor);
    let mut out = String::new();
    out.push_str(&text[..at]);
    out.push_str(insert);
    out.push_str(&text[at..]);
    (out, cursor + insert.chars().count())
}

fn delete_before(text: &str, cursor: usize) -> (String, usize) {
    if cursor == 0 {
        return (text.to_owned(), 0);
    }
    let end = char_byte(text, cursor);
    let start = char_byte(text, cursor - 1);
    let mut out = String::new();
    out.push_str(&text[..start]);
    out.push_str(&text[end..]);
    (out, cursor - 1)
}

fn delete_after(text: &str, cursor: usize) -> (String, usize) {
    let start = char_byte(text, cursor);
    let Some(next) = text[start..].chars().next() else {
        return (text.to_owned(), cursor);
    };
    let mut out = String::new();
    out.push_str(&text[..start]);
    out.push_str(&text[start + next.len_utf8()..]);
    (out, cursor)
}

fn kill_word_before(text: &str, cursor: usize) -> (String, usize) {
    let start = word_start_left(text, cursor);
    let from = char_byte(text, start);
    let to = char_byte(text, cursor);
    let mut out = String::new();
    out.push_str(&text[..from]);
    out.push_str(&text[to..]);
    (out, start)
}

fn word_start_left(text: &str, cursor: usize) -> usize {
    let chars: Vec<char> = text.chars().collect();
    let mut index = cursor.min(chars.len());
    while index > 0 && chars[index - 1].is_whitespace() {
        index -= 1;
    }
    while index > 0 && !chars[index - 1].is_whitespace() {
        index -= 1;
    }
    index
}

fn word_left(text: &str, cursor: usize) -> usize {
    let chars: Vec<char> = text.chars().collect();
    let mut index = cursor.min(chars.len());
    if index == 0 {
        return 0;
    }
    if !chars[index - 1].is_whitespace() {
        while index > 0 && !chars[index - 1].is_whitespace() {
            index -= 1;
        }
    }
    while index > 0 && chars[index - 1].is_whitespace() {
        index -= 1;
    }
    index
}

fn word_right(text: &str, cursor: usize) -> usize {
    let chars: Vec<char> = text.chars().collect();
    let mut index = cursor.min(chars.len());
    if index < chars.len() && !chars[index].is_whitespace() {
        while index < chars.len() && !chars[index].is_whitespace() {
            index += 1;
        }
        return index;
    }
    while index < chars.len() && chars[index].is_whitespace() {
        index += 1;
    }
    while index < chars.len() && !chars[index].is_whitespace() {
        index += 1;
    }
    index
}

fn current_input_line(content: &crate::backend::RenderableContent) -> String {
    let cursor = content.grid.cursor.point;
    content
        .grid
        .display_iter()
        .filter(|indexed| {
            indexed.point.line == cursor.line
                && indexed.point.column.0 <= cursor.column.0
                && !indexed
                    .cell
                    .flags
                    .intersects(cell::Flags::WIDE_CHAR_SPACER)
        })
        .map(|indexed| indexed.c)
        .collect()
}

fn recalled_agent_line(
    content: &crate::backend::RenderableContent,
    entries: &[String],
) -> Option<String> {
    let line = current_input_line(content);
    if let Some(entry) = entries
        .iter()
        .rev()
        .find(|entry| entry.starts_with('!') && line.ends_with(entry.as_str()))
    {
        return Some(entry.clone());
    }
    // Entries from earlier repterm sessions are not in `entries`, but still
    // carry the `!` prefix in the shell history.
    let start = line.find('!')?;
    let text = line[start..].trim_end().to_owned();
    (text.len() > 1).then_some(text)
}

fn is_bang_submit(event: &egui::Event) -> bool {
    match event {
        egui::Event::Key {
            key: Key::Enter,
            pressed: true,
            modifiers,
            ..
        } => !modifiers.alt,
        egui::Event::Text(text) | egui::Event::Paste(text) => {
            !text.is_empty() && text.chars().all(|ch| ch == '\n' || ch == '\r')
        }
        _ => false,
    }
}

fn submit_bang(state: &mut TerminalViewState) -> BangAction {
    let typed = state
        .bang_draft
        .take()
        .unwrap_or_default()
        .replace(['\n', '\r'], "");
    let typed = typed.trim().to_owned();
    state.bang_cursor = 0;
    state.bang_committed = Some(typed.clone());
    state.line_input = Some(String::new());
    if typed.is_empty() || typed == "!" {
        BangAction::Consume
    } else {
        BangAction::Submit(typed)
    }
}

fn char_byte(text: &str, cursor: usize) -> usize {
    text.char_indices()
        .nth(cursor)
        .map(|(index, _)| index)
        .unwrap_or(text.len())
}

enum BangAction {
    PassThrough,
    Consume,
    Submit(String),
    SubmitRecalled(String),
}

fn track_bang_submit(
    event: &egui::Event,
    state: &mut TerminalViewState,
    agent_conversation: bool,
    recalled_bang: Option<&str>,
) -> BangAction {
    if state.bang_draft.is_none()
        && state.bang_committed.is_some()
        && matches!(
            event,
            egui::Event::Text(_)
                | egui::Event::Paste(_)
                | egui::Event::Key {
                    key: Key::Escape | Key::Enter,
                    pressed: true,
                    ..
                }
        )
    {
        state.bang_committed = None;
        if matches!(event, egui::Event::Key { key: Key::Escape, .. }) {
            return BangAction::Consume;
        }
    }

    if state.bang_draft.is_none() {
        if let Some(question) = recalled_bang {
            if is_bang_submit(event) {
                state.bang_committed = Some(question.to_owned());
                state.line_input = Some(String::new());
                return BangAction::SubmitRecalled(question.to_owned());
            }
        }
    }

    if state.bang_draft.is_some() {
        if is_bang_submit(event) {
            return submit_bang(state);
        }
        edit_bang_draft(event, state);
        if state.bang_draft.as_deref().is_some_and(str::is_empty) {
            state.bang_draft = None;
            state.bang_cursor = 0;
        }
        return BangAction::Consume;
    }

    match event {
        egui::Event::Text(text) | egui::Event::Paste(text) => {
            let line_empty = state.line_input.as_deref().is_none_or(str::is_empty);
            if line_empty
                && !text.contains(['\n', '\r'])
                && (text.starts_with('!') || agent_conversation)
            {
                state.bang_cursor = text.chars().count();
                state.bang_draft = Some(text.clone());
                state.line_input = Some(String::new());
                return BangAction::Consume;
            }
            if let Some(input) = &mut state.line_input {
                input.push_str(text);
            }
        }
        egui::Event::Key {
            key: Key::Backspace,
            pressed: true,
            modifiers,
            ..
        } if modifiers.is_none() => {
            if let Some(input) = &mut state.line_input {
                input.pop();
            }
        }
        egui::Event::Key {
            key: Key::U,
            pressed: true,
            modifiers,
            ..
        } if modifiers.command_only() => state.line_input = Some(String::new()),
        egui::Event::Key {
            key: Key::Enter,
            pressed: true,
            modifiers,
            ..
        } if modifiers.is_none() => state.line_input = Some(String::new()),
        egui::Event::Key {
            key:
                Key::ArrowUp
                | Key::ArrowDown
                | Key::ArrowLeft
                | Key::ArrowRight
                | Key::Home
                | Key::End
                | Key::PageUp
                | Key::PageDown
                | Key::Tab
                | Key::Delete
                | Key::Escape,
            pressed: true,
            ..
        } => state.line_input = None,
        egui::Event::Key {
            pressed: true,
            modifiers,
            ..
        } if !modifiers.is_none() && !modifiers.shift_only() => state.line_input = None,
        _ => {}
    }

    BangAction::PassThrough
}

fn process_keyboard_event(
    event: egui::Event,
    backend: &TerminalBackend,
    bindings_layout: &BindingsLayout,
    modifiers: Modifiers,
) -> InputAction {
    match event {
        egui::Event::Text(text) => {
            process_text_event(&text, modifiers, backend, bindings_layout)
        },
        egui::Event::Paste(text) => InputAction::BackendCall(
            #[cfg(not(any(target_os = "ios", target_os = "macos")))]
            if modifiers.contains(Modifiers::COMMAND | Modifiers::SHIFT) {
                BackendCommand::Write(text.as_bytes().to_vec())
            } else {
                // Hotfix - Send ^V when there's not selection on view.
                BackendCommand::Write([0x16].to_vec())
            },
            #[cfg(any(target_os = "ios", target_os = "macos"))]
            {
                BackendCommand::Write(text.as_bytes().to_vec())
            },
        ),
        egui::Event::Copy => {
            #[cfg(not(any(target_os = "ios", target_os = "macos")))]
            if modifiers.contains(Modifiers::COMMAND | Modifiers::SHIFT) {
                let content = backend.selectable_content();
                InputAction::WriteToClipboard(content)
            } else {
                // Hotfix - Send ^C when there's not selection on view.
                InputAction::BackendCall(BackendCommand::Write([0x3].to_vec()))
            }
            #[cfg(any(target_os = "ios", target_os = "macos"))]
            {
                let content = backend.selectable_content();
                InputAction::WriteToClipboard(content)
            }
        },
        egui::Event::Key {
            key,
            pressed,
            modifiers,
            ..
        } => process_keyboard_key(
            backend,
            bindings_layout,
            key,
            modifiers,
            pressed,
        ),
        _ => InputAction::Ignore,
    }
}

fn process_text_event(
    text: &str,
    modifiers: Modifiers,
    backend: &TerminalBackend,
    bindings_layout: &BindingsLayout,
) -> InputAction {
    if let Some(key) = Key::from_name(text) {
        if bindings_layout.get_action(
            InputKind::KeyCode(key),
            modifiers,
            backend.last_content().terminal_mode,
        ) == BindingAction::Ignore
        {
            InputAction::BackendCall(BackendCommand::Write(
                text.as_bytes().to_vec(),
            ))
        } else {
            InputAction::Ignore
        }
    } else {
        InputAction::BackendCall(BackendCommand::Write(
            text.as_bytes().to_vec(),
        ))
    }
}

fn process_keyboard_key(
    backend: &TerminalBackend,
    bindings_layout: &BindingsLayout,
    key: Key,
    modifiers: Modifiers,
    pressed: bool,
) -> InputAction {
    if !pressed {
        return InputAction::Ignore;
    }

    let terminal_mode = backend.last_content().terminal_mode;
    let binding_action = bindings_layout.get_action(
        InputKind::KeyCode(key),
        modifiers,
        terminal_mode,
    );

    match binding_action {
        BindingAction::Char(c) => {
            let mut buf = [0, 0, 0, 0];
            let str = c.encode_utf8(&mut buf);
            InputAction::BackendCall(BackendCommand::Write(
                str.as_bytes().to_vec(),
            ))
        },
        BindingAction::Esc(seq) => InputAction::BackendCall(
            BackendCommand::Write(seq.as_bytes().to_vec()),
        ),
        _ => InputAction::Ignore,
    }
}

fn process_mouse_wheel(
    state: &mut TerminalViewState,
    backend: &TerminalBackend,
    layout: &Response,
    font_size: f32,
    unit: MouseWheelUnit,
    delta: Vec2,
    modifiers: Modifiers,
) -> Vec<InputAction> {
    let (scroll_delta, wheel_lines) = match unit {
        MouseWheelUnit::Line => {
            let lines = delta.y.signum() * delta.y.abs().ceil();
            (lines as i32, lines.abs() as usize)
        },
        MouseWheelUnit::Point => {
            state.scroll_pixels -= delta.y;
            let lines = (state.scroll_pixels / font_size).trunc();
            state.scroll_pixels %= font_size;
            (-lines as i32, lines.abs() as usize)
        },
        MouseWheelUnit::Page => (0, if delta.y == 0.0 { 0 } else { 1 }),
    };

    if scroll_delta == 0 && wheel_lines == 0 {
        return vec![InputAction::Ignore];
    }

    if backend
        .last_content()
        .terminal_mode
        .intersects(TermMode::MOUSE_MODE)
        && !modifiers.contains(Modifiers::SHIFT)
    {
        if let Some(position) = layout.ctx.input(|input| input.pointer.latest_pos()) {
            let content = backend.last_content();
            state.current_mouse_position_on_grid = TerminalBackend::selection_point(
                position.x - layout.rect.min.x,
                position.y - layout.rect.min.y,
                &content.terminal_size,
                content.grid.display_offset(),
            );
        }

        let button = if delta.y > 0.0 {
            MouseButton::ScrollUp
        } else {
            MouseButton::ScrollDown
        };
        return (0..wheel_lines)
            .map(|_| {
                InputAction::BackendCall(BackendCommand::MouseReport(
                    button,
                    modifiers,
                    state.current_mouse_position_on_grid,
                    true,
                ))
            })
            .collect();
    }

    if scroll_delta == 0 {
        vec![InputAction::Ignore]
    } else {
        vec![InputAction::BackendCall(BackendCommand::Scroll(
            scroll_delta,
        ))]
    }
}

fn process_button_click(
    state: &mut TerminalViewState,
    layout: &Response,
    backend: &TerminalBackend,
    bindings_layout: &BindingsLayout,
    button: PointerButton,
    position: Pos2,
    modifiers: &Modifiers,
    pressed: bool,
    selection_requires_shift: bool,
) -> InputAction {
    match button {
        PointerButton::Primary => process_left_button(
            state,
            layout,
            backend,
            bindings_layout,
            position,
            modifiers,
            pressed,
            selection_requires_shift,
        ),
        _ => InputAction::Ignore,
    }
}

fn primary_route(
    mouse_mode: bool,
    shift: bool,
    selection_requires_shift: bool,
) -> PointerRoute {
    if selection_requires_shift {
        if shift {
            PointerRoute::Select
        } else if mouse_mode {
            PointerRoute::MouseReport
        } else {
            PointerRoute::Ignore
        }
    } else if mouse_mode && !shift {
        PointerRoute::MouseReport
    } else {
        PointerRoute::Select
    }
}

fn move_route(active: Option<PointerRoute>, mouse_motion: bool) -> PointerRoute {
    match active {
        Some(PointerRoute::Select) => PointerRoute::Select,
        Some(PointerRoute::MouseReport) if mouse_motion => PointerRoute::MouseReport,
        _ => PointerRoute::Ignore,
    }
}

fn process_left_button(
    state: &mut TerminalViewState,
    layout: &Response,
    backend: &TerminalBackend,
    bindings_layout: &BindingsLayout,
    position: Pos2,
    modifiers: &Modifiers,
    pressed: bool,
    selection_requires_shift: bool,
) -> InputAction {
    let terminal_mode = backend.last_content().terminal_mode;
    let route = primary_route(
        terminal_mode.intersects(TermMode::MOUSE_MODE),
        modifiers.contains(Modifiers::SHIFT),
        selection_requires_shift,
    );

    if pressed {
        state.active_route = match route {
            PointerRoute::Ignore => None,
            active => Some(active),
        };
        return match route {
            PointerRoute::Select => {
                InputAction::BackendCall(build_start_select_command(layout, position))
            },
            PointerRoute::MouseReport => InputAction::BackendCall(
                BackendCommand::MouseReport(
                    MouseButton::LeftButton,
                    *modifiers,
                    state.current_mouse_position_on_grid,
                    true,
                ),
            ),
            PointerRoute::Ignore => InputAction::Ignore,
        };
    }

    match state.active_route.take() {
        Some(PointerRoute::Select) => finish_selection(
            state,
            layout,
            backend,
            bindings_layout,
            position,
            modifiers,
        ),
        Some(PointerRoute::MouseReport) => InputAction::BackendCall(
            BackendCommand::MouseReport(
                MouseButton::LeftButton,
                *modifiers,
                state.current_mouse_position_on_grid,
                false,
            ),
        ),
        Some(PointerRoute::Ignore) | None => InputAction::Ignore,
    }
}

fn finish_selection(
    state: &TerminalViewState,
    layout: &Response,
    backend: &TerminalBackend,
    bindings_layout: &BindingsLayout,
    position: Pos2,
    modifiers: &Modifiers,
) -> InputAction {
    if layout.double_clicked() || layout.triple_clicked() {
        return InputAction::BackendCall(build_start_select_command(layout, position));
    }

    let terminal_content = backend.last_content();
    let binding_action = bindings_layout.get_action(
        InputKind::Mouse(PointerButton::Primary),
        *modifiers,
        terminal_content.terminal_mode,
    );

    if binding_action == BindingAction::LinkOpen {
        InputAction::BackendCall(BackendCommand::ProcessLink(
            LinkAction::Open,
            state.current_mouse_position_on_grid,
        ))
    } else {
        // repterm: finish a drag selection by copying what the user
        // highlighted. The host turns this into a visible confirmation.
        let content = backend.selectable_content();
        if content.trim().is_empty() {
            InputAction::Ignore
        } else {
            InputAction::WriteToClipboard(content)
        }
    }
}

fn build_start_select_command(
    layout: &Response,
    cursor_position: Pos2,
) -> BackendCommand {
    let selection_type = if layout.double_clicked() {
        SelectionType::Semantic
    } else if layout.triple_clicked() {
        SelectionType::Lines
    } else {
        SelectionType::Simple
    };

    BackendCommand::SelectStart(
        selection_type,
        cursor_position.x - layout.rect.min.x,
        cursor_position.y - layout.rect.min.y,
    )
}

fn process_mouse_move(
    state: &mut TerminalViewState,
    layout: &Response,
    backend: &TerminalBackend,
    position: Pos2,
    modifiers: &Modifiers,
) -> Vec<InputAction> {
    let terminal_content = backend.last_content();
    let cursor_x = position.x - layout.rect.min.x;
    let cursor_y = position.y - layout.rect.min.y;
    state.current_mouse_position_on_grid = TerminalBackend::selection_point(
        cursor_x,
        cursor_y,
        &terminal_content.terminal_size,
        terminal_content.grid.display_offset(),
    );

    let mut actions = vec![];
    let route = move_route(
        state.active_route,
        terminal_content.terminal_mode.contains(TermMode::MOUSE_MOTION),
    );
    let cmd = match route {
        PointerRoute::Select => InputAction::BackendCall(
            BackendCommand::SelectUpdate(cursor_x, cursor_y),
        ),
        PointerRoute::MouseReport => InputAction::BackendCall(
            BackendCommand::MouseReport(
                MouseButton::LeftMove,
                *modifiers,
                state.current_mouse_position_on_grid,
                true,
            ),
        ),
        PointerRoute::Ignore => InputAction::Ignore,
    };
    actions.push(cmd);

    // Handle link hover if applicable
    if modifiers.command_only() {
        actions.push(InputAction::BackendCall(BackendCommand::ProcessLink(
            LinkAction::Hover,
            state.current_mouse_position_on_grid,
        )));
    }

    actions
}

#[cfg(test)]
mod pointer_route_tests {
    use super::{move_route, primary_route, PointerRoute};

    #[test]
    fn current_mode_plain_drag_in_mouse_mode_goes_to_app() {
        assert_eq!(
            primary_route(true, false, false),
            PointerRoute::MouseReport
        );
    }

    #[test]
    fn current_mode_plain_drag_without_mouse_mode_selects() {
        assert_eq!(primary_route(false, false, false), PointerRoute::Select);
    }

    #[test]
    fn shift_mode_plain_drag_in_mouse_mode_goes_to_app() {
        assert_eq!(primary_route(true, false, true), PointerRoute::MouseReport);
    }

    #[test]
    fn shift_mode_plain_drag_without_mouse_mode_is_ignored() {
        assert_eq!(primary_route(false, false, true), PointerRoute::Ignore);
    }

    #[test]
    fn shift_mode_shift_drag_selects_even_in_mouse_mode() {
        assert_eq!(primary_route(true, true, true), PointerRoute::Select);
    }

    #[test]
    fn mouse_move_is_forwarded_only_for_mouse_drag_with_motion() {
        assert_eq!(
            move_route(Some(PointerRoute::MouseReport), true),
            PointerRoute::MouseReport
        );
        assert_eq!(
            move_route(Some(PointerRoute::MouseReport), false),
            PointerRoute::Ignore
        );
        assert_eq!(move_route(None, true), PointerRoute::Ignore);
    }

    #[test]
    fn selection_drag_keeps_updating_after_shift_is_released() {
        assert_eq!(
            move_route(Some(PointerRoute::Select), true),
            PointerRoute::Select
        );
    }
}

#[cfg(test)]
mod bang_edit_tests {
    use super::{delete_before, insert_at, kill_word_before, word_left, word_right};

    #[test]
    fn insert_and_backspace_keep_middle_cursor() {
        let (text, cursor) = insert_at("!hola", 1, "X");
        assert_eq!(text, "!Xhola");
        assert_eq!(cursor, 2);
        let (text, cursor) = delete_before(&text, cursor);
        assert_eq!(text, "!hola");
        assert_eq!(cursor, 1);
    }

    #[test]
    fn ctrl_arrows_jump_words_like_zsh() {
        let text = "!dime que hora es";
        assert_eq!(word_right(text, 0), 5);
        assert_eq!(word_right(text, 5), 9);
        assert_eq!(word_left(text, text.chars().count()), 14);
        assert_eq!(word_left(text, 14), 9);
    }

    #[test]
    fn ctrl_backspace_deletes_previous_word() {
        let (text, cursor) = kill_word_before("!dime que hora", 14);
        assert_eq!(text, "!dime que ");
        assert_eq!(cursor, 10);
    }
}

#[cfg(test)]
mod agent_indicator_tests {
    use super::{agent_indicator_active, cursor_row_visible, indicator_column};

    #[test]
    fn indicator_tracks_draft_stream_and_conversation_but_not_shell() {
        assert!(!agent_indicator_active(false, false, false));
        assert!(agent_indicator_active(false, true, false));
        assert!(agent_indicator_active(false, false, true));
        assert!(agent_indicator_active(true, false, false));
    }

    #[test]
    fn leftmost_prompt_arrow_is_selected_not_later_text() {
        let prompt = [(0, '➜', true), (1, ' ', true), (2, 'r', true), (3, '❯', true)];
        assert_eq!(indicator_column(prompt.into_iter()), Some(0));
    }

    #[test]
    fn prompt_without_arrow_is_not_overlaid() {
        let prompt = [(0, '$', true), (1, ' ', true)];
        assert_eq!(indicator_column(prompt.into_iter()), None);
        assert_eq!(indicator_column([(0, '$', true)].into_iter()), None);
    }

    #[test]
    fn ignores_unsafe_wide_cells_and_arrows_embedded_in_prompt_text() {
        assert_eq!(indicator_column([(0, '➜', false), (1, ' ', true)].into_iter()), None);
        assert_eq!(indicator_column([(0, 'x', true), (1, '>', true)].into_iter()), None);
        assert_eq!(indicator_column([(0, ' ', true), (1, '➜', true)].into_iter()), Some(1));
    }

    #[test]
    fn hides_indicator_when_scrollback_moves_cursor_out_of_view() {
        assert!(cursor_row_visible(3, 10.0, 40.0));
        assert!(!cursor_row_visible(4, 10.0, 40.0));
        assert!(!cursor_row_visible(-1, 10.0, 40.0));
    }
}
