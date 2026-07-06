use ratatui::{
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};
use crate::state::types::NewWorktreeDialog;
use crate::vcs::VcsBackend;

pub fn render_new_worktree_dialog(
    frame: &mut Frame,
    area: Rect,
    dialog: &NewWorktreeDialog,
) {
    let dialog_width = 60u16.min(area.width.saturating_sub(4));
    let dialog_height = 7u16;

    let x = area.x + (area.width.saturating_sub(dialog_width)) / 2;
    let y = area.y + (area.height.saturating_sub(dialog_height)) / 2;

    let dialog_area = Rect {
        x,
        y,
        width: dialog_width,
        height: dialog_height,
    };

    // Clear background
    frame.render_widget(Clear, dialog_area);

    let title = match dialog.backend {
        VcsBackend::Jj => " New Workspace ",
        VcsBackend::Git => " New Worktree ",
    };
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let inner = block.inner(dialog_area);
    frame.render_widget(block, dialog_area);

    let [label_area, input_area, error_area, hint_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(inner)[..] else {
        return;
    };

    // Label
    let label_text = match dialog.backend {
        VcsBackend::Jj => "Workspace name:",
        VcsBackend::Git => "Branch name:",
    };
    let label = Paragraph::new(label_text);
    frame.render_widget(label, label_area);

    // Input field
    let input_text = if dialog.is_creating {
        format!("Creating {}...", dialog.branch_name)
    } else {
        let mut s = dialog.branch_name.clone();
        // Insert cursor indicator
        if dialog.cursor_pos <= s.len() {
            s.insert(dialog.cursor_pos, '│');
        }
        s
    };

    let input_style = if dialog.is_creating {
        Style::default().fg(Color::DarkGray)
    } else {
        Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
    };

    let input = Paragraph::new(Line::from(vec![
        Span::styled("> ", Style::default().fg(Color::Cyan)),
        Span::styled(input_text, input_style),
    ]));
    frame.render_widget(input, input_area);

    // Error
    if let Some(err) = &dialog.error {
        let error = Paragraph::new(Span::styled(
            err.as_str(),
            Style::default().fg(Color::Red),
        ));
        frame.render_widget(error, error_area);
    }

    // Hint
    let hint = Paragraph::new(Span::styled(
        "Enter: create  Esc: cancel",
        Style::default().fg(Color::DarkGray),
    ))
    .alignment(Alignment::Right);
    frame.render_widget(hint, hint_area);
}
