pub mod add_repo;
pub mod delete_worktree;
pub mod new_worktree;
pub mod terminal_panel;
pub mod theme;
pub mod worktree_panel;

use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph},
    Frame,
};
use crate::app::App;
use crate::keybindings::Action;
use crate::state::types::PanelFocus;
use crate::ui::add_repo::render_add_repo_dialog;
use crate::ui::new_worktree::render_new_worktree_dialog;
use crate::ui::terminal_panel::render_terminal_panel;
use crate::ui::worktree_panel::render_worktree_panel;

/// Draw the full UI and return the exact inner dimensions of the terminal
/// panel so callers can keep PTY sizes pixel-perfect.
pub fn draw(frame: &mut Frame, app: &App) -> (u16, u16) {
    let area = frame.area();

    let [main_area, status_area] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area);

    let [left, right] = Layout::horizontal([
        Constraint::Percentage(40),
        Constraint::Percentage(60),
    ])
    .areas(main_area);

    render_worktree_panel(frame, left, &app.state, &app.pty_manager);

    let active_session = app
        .state
        .selected_worktree_path()
        .and_then(|p| app.pty_manager.get(p));

    let terminal_inner = render_terminal_panel(
        frame,
        right,
        active_session,
        app.state.focus == PanelFocus::Terminal,
        &app.state.keybindings,
    );

    render_status_bar(frame, status_area, app);

    if let Some(dialog) = &app.state.new_worktree_dialog {
        render_new_worktree_dialog(frame, area, dialog);
    }

    if let Some(dialog) = &app.state.add_repo_dialog {
        render_add_repo_dialog(frame, area, dialog);
    }

    if let Some(dialog) = &app.state.delete_worktree_dialog {
        delete_worktree::render_delete_worktree_dialog(frame, area, dialog);
    }

    (terminal_inner.width, terminal_inner.height)
}

fn build_status_hints(kb: &crate::keybindings::Keybindings) -> Vec<String> {
    let mut hints = Vec::new();

    if let (Some(down), Some(up)) = (kb.hint_for(Action::MoveDown), kb.hint_for(Action::MoveUp)) {
        hints.push(format!("{}/{}: navigate", down, up));
    }

    let action_labels = [
        (Action::FocusTerminal, "terminal"),
        (Action::UnfocusTerminal, "unfocus"),
        (Action::OpenEditor, "smerge"),
        (Action::NewWorktree, "new"),
        (Action::DeleteWorktree, "delete"),
        (Action::AddRepo, "add repo"),
        (Action::Refresh, "refresh"),
        (Action::Quit, "quit"),
    ];

    for (action, label) in &action_labels {
        if let Some(key) = kb.hint_for(*action) {
            hints.push(format!("{}: {}", key, label));
        }
    }

    hints
}

fn render_status_bar(frame: &mut Frame, area: Rect, app: &App) {
    let mut parts = vec![];

    if app.state.is_refreshing {
        parts.push(Span::styled(
            " \u{27f3} refreshing ",
            Style::default().fg(Color::Yellow),
        ));
    }

    let hint_str = format!(" {}", build_status_hints(&app.state.keybindings).join("  "));
    parts.push(Span::styled(hint_str, Style::default().fg(Color::DarkGray)));

    let status = Paragraph::new(Line::from(parts))
        .block(Block::default().style(Style::default().bg(theme::COLOR_STATUS_BAR)));

    frame.render_widget(status, area);
}
