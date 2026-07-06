use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::TermMode;
use alacritty_terminal::index::Side;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use tokio::sync::mpsc::UnboundedSender;
use crate::app::App;
use crate::events::AppEvent;
use crate::keybindings::Action;
use crate::state::types::{AddRepoDialog, DeleteWorktreeDialog, PanelFocus, NewWorktreeDialog};

pub fn handle_event(app: &mut App, event: Event, tx: &UnboundedSender<AppEvent>) {
    match event {
        Event::Key(key) => handle_key(app, key, tx),
        Event::Resize(cols, rows) => handle_resize(app, cols, rows),
        Event::Mouse(mouse) => handle_mouse(app, mouse, tx),
        Event::Paste(ref text) => {
            tracing::debug!("Paste event: len={} bytes={:?}", text.len(), &text.as_bytes()[..text.len().min(100)]);
            handle_paste(app, text);
        }
        _ => {
            tracing::debug!("Other crossterm event: {:?}", event);
        }
    }
}

fn handle_paste(app: &mut App, text: &str) {
    if app.state.focus != PanelFocus::Terminal {
        return;
    }

    if let Some(path) = app.state.selected_worktree_path().cloned() {
        if let Some(session) = app.pty_manager.get_mut(&path) {
            let bracketed = session.term.lock().mode().contains(TermMode::BRACKETED_PASTE);
            session.reset_scroll();
            if bracketed {
                session.write_input(b"\x1b[200~");
                session.write_input(text.as_bytes());
                session.write_input(b"\x1b[201~");
            } else {
                session.write_input(text.as_bytes());
            }
        }
    }
}

fn handle_mouse(app: &mut App, event: MouseEvent, tx: &UnboundedSender<AppEvent>) {
    let (total_cols, total_rows) = app.terminal_size;
    let left_width = (total_cols as u32 * 40 / 100) as u16;
    let in_left_panel = event.column < left_width;
    let in_status_bar = event.row >= total_rows.saturating_sub(1);

    let term_inner_left = left_width + 1;
    let term_inner_top: u16 = 1;
    let term_inner_right = total_cols.saturating_sub(1);
    let term_inner_bottom = total_rows.saturating_sub(2);

    let in_terminal_content = event.column >= term_inner_left
        && event.column < term_inner_right
        && event.row >= term_inner_top
        && event.row < term_inner_bottom;

    match event.kind {
        MouseEventKind::Down(MouseButton::Left) if !in_status_bar => {
            if in_left_panel {
                clear_selection(app);
                app.state.focus = PanelFocus::WorktreeList;
            } else if in_terminal_content {
                let row = event.row - term_inner_top;
                let col = event.column - term_inner_left;

                if let Some(path) = app.state.selected_worktree_path().cloned() {
                    let (r, c) = exact_or_approx_pty_size(app);
                    if let Err(e) = app.pty_manager.get_or_create(&path, r, c, tx.clone()) {
                        tracing::error!("Failed to create PTY session on click: {}", e);
                    } else {
                        app.state.focus = PanelFocus::Terminal;
                    }

                    if let Some(session) = app.pty_manager.get_mut(&path) {
                        let point = Point::new(Line(row as i32), Column(col as usize));
                        let mut term = session.term.lock();
                        term.selection = Some(Selection::new(SelectionType::Simple, point, Side::Left));
                        session.selection_dragging = true;
                    }
                } else {
                    app.state.focus = PanelFocus::Terminal;
                }
            } else {
                clear_selection(app);
                app.state.focus = PanelFocus::Terminal;
            }
        }

        MouseEventKind::Down(_) if !in_status_bar => {
            clear_selection(app);
            if in_left_panel {
                app.state.focus = PanelFocus::WorktreeList;
            } else {
                app.state.focus = PanelFocus::Terminal;
            }
        }

        MouseEventKind::Drag(MouseButton::Left) => {
            if let Some(path) = app.state.selected_worktree_path().cloned() {
                if let Some(session) = app.pty_manager.get_mut(&path) {
                    if session.selection_dragging {
                        let row = event.row.saturating_sub(term_inner_top);
                        let col = event.column.saturating_sub(term_inner_left);
                        let max_row = term_inner_bottom.saturating_sub(term_inner_top).saturating_sub(1);
                        let max_col = term_inner_right.saturating_sub(term_inner_left).saturating_sub(1);
                        let point = Point::new(
                            Line(row.min(max_row) as i32),
                            Column(col.min(max_col) as usize),
                        );
                        let mut term = session.term.lock();
                        if let Some(ref mut sel) = term.selection {
                            sel.update(point, Side::Right);
                        }
                    }
                }
            }
        }

        MouseEventKind::Up(MouseButton::Left) => {
            if let Some(path) = app.state.selected_worktree_path().cloned() {
                if let Some(session) = app.pty_manager.get_mut(&path) {
                    session.selection_dragging = false;
                    let term = session.term.lock();
                    if let Some(text) = term.selection_to_string() {
                        if text.is_empty() {
                            drop(term);
                            clear_selection_for_path(app, &path);
                        } else {
                            match arboard::Clipboard::new() {
                                Ok(mut clipboard) => {
                                    if let Err(e) = clipboard.set_text(&text) {
                                        tracing::warn!("Failed to copy to clipboard: {}", e);
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!("Failed to access clipboard: {}", e);
                                }
                            }
                        }
                    } else {
                        drop(term);
                        clear_selection_for_path(app, &path);
                    }
                }
            }
        }

        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
            if app.state.focus == PanelFocus::Terminal =>
        {
            clear_selection(app);
            let is_up = matches!(event.kind, MouseEventKind::ScrollUp);

            let col_1 = event.column.saturating_sub(term_inner_left) + 1;
            let row_1 = event.row.saturating_sub(term_inner_top) + 1;

            if let Some(path) = app.state.selected_worktree_path().cloned() {
                if let Some(session) = app.pty_manager.get_mut(&path) {
                    let mode = *session.term.lock().mode();
                    let mouse_reporting = mode.intersects(TermMode::MOUSE_MODE);
                    let sgr_mouse = mode.contains(TermMode::SGR_MOUSE);
                    let alt_screen = mode.contains(TermMode::ALT_SCREEN);

                    if mouse_reporting {
                        session.reset_scroll();
                        let button = if is_up { 64u8 } else { 65u8 };
                        let bytes = if sgr_mouse {
                            format!("\x1b[<{};{};{}M", button, col_1, row_1)
                                .into_bytes()
                        } else {
                            vec![0x1b, b'[', b'M',
                                 32 + button,
                                 32 + (col_1 as u8).min(223),
                                 32 + (row_1 as u8).min(223)]
                        };
                        session.write_input(&bytes);
                    } else if alt_screen {
                        session.reset_scroll();
                        let arrow: &[u8] = if is_up { b"\x1b[A" } else { b"\x1b[B" };
                        for _ in 0..3 {
                            session.write_input(arrow);
                        }
                    } else if is_up {
                        session.scroll_up(3);
                    } else {
                        session.scroll_down(3);
                    }
                }
            }
        }

        _ => {}
    }
}

fn clear_selection(app: &mut App) {
    if let Some(path) = app.state.selected_worktree_path().cloned() {
        clear_selection_for_path(app, &path);
    }
}

fn clear_selection_for_path(app: &mut App, path: &std::path::PathBuf) {
    if let Some(session) = app.pty_manager.get_mut(path) {
        session.selection_dragging = false;
        session.term.lock().selection = None;
    }
}

fn handle_key(app: &mut App, key: KeyEvent, tx: &UnboundedSender<AppEvent>) {
    tracing::debug!("handle_key: code={:?} modifiers={:?} kind={:?} focus={:?}", key.code, key.modifiers, key.kind, app.state.focus);
    if app.state.delete_worktree_dialog.is_some() {
        handle_delete_dialog_key(app, key, tx);
        return;
    }

    if app.state.add_repo_dialog.is_some() {
        handle_add_repo_dialog_key(app, key, tx);
        return;
    }

    if app.state.new_worktree_dialog.is_some() {
        handle_dialog_key(app, key, tx);
        return;
    }

    match app.state.focus {
        PanelFocus::WorktreeList => handle_list_key(app, key, tx),
        PanelFocus::Terminal => handle_terminal_key(app, key, tx),
    }
}

fn handle_add_repo_dialog_key(app: &mut App, key: KeyEvent, tx: &UnboundedSender<AppEvent>) {
    let dialog = match app.state.add_repo_dialog.as_mut() {
        Some(d) => d,
        None => return,
    };

    if dialog.is_adding {
        return;
    }

    match key.code {
        KeyCode::Esc => {
            app.state.add_repo_dialog = None;
        }
        KeyCode::Enter => {
            let path = dialog.expanded_path();

            if dialog.path_input.is_empty() {
                dialog.error = Some("Path cannot be empty".to_string());
                return;
            }

            let already_tracked = app.state.repos.iter().any(|r| r.config.path == path);
            if already_tracked {
                dialog.error = Some("This repository is already tracked".to_string());
                return;
            }

            dialog.is_adding = true;
            dialog.error = None;

            let tx = tx.clone();
            tokio::spawn(async move {
                validate_and_add_repo(path, tx).await;
            });
        }
        KeyCode::Backspace => {
            let dialog = app.state.add_repo_dialog.as_mut().unwrap();
            if dialog.cursor_pos > 0 {
                dialog.cursor_pos -= 1;
                dialog.path_input.remove(dialog.cursor_pos);
            }
        }
        KeyCode::Delete => {
            let dialog = app.state.add_repo_dialog.as_mut().unwrap();
            if dialog.cursor_pos < dialog.path_input.len() {
                dialog.path_input.remove(dialog.cursor_pos);
            }
        }
        KeyCode::Left => {
            let dialog = app.state.add_repo_dialog.as_mut().unwrap();
            if dialog.cursor_pos > 0 {
                dialog.cursor_pos -= 1;
            }
        }
        KeyCode::Right => {
            let dialog = app.state.add_repo_dialog.as_mut().unwrap();
            if dialog.cursor_pos < dialog.path_input.len() {
                dialog.cursor_pos += 1;
            }
        }
        KeyCode::Home => {
            app.state.add_repo_dialog.as_mut().unwrap().cursor_pos = 0;
        }
        KeyCode::End => {
            let len = app.state.add_repo_dialog.as_ref().unwrap().path_input.len();
            app.state.add_repo_dialog.as_mut().unwrap().cursor_pos = len;
        }
        KeyCode::Char(c) => {
            let dialog = app.state.add_repo_dialog.as_mut().unwrap();
            dialog.path_input.insert(dialog.cursor_pos, c);
            dialog.cursor_pos += 1;
        }
        _ => {}
    }
}

async fn validate_and_add_repo(path: std::path::PathBuf, tx: UnboundedSender<AppEvent>) {
    let result = tokio::task::spawn_blocking(move || -> Result<std::path::PathBuf, String> {
        if !path.exists() {
            return Err(format!("Path does not exist: {}", path.display()));
        }
        git2::Repository::open(&path)
            .map_err(|e| format!("Not a git repository: {}", e))?;
        Ok(path)
    })
    .await;

    match result {
        Ok(Ok(path)) => { tx.send(AppEvent::RepoAdded(path)).ok(); }
        Ok(Err(msg)) => { tx.send(AppEvent::RepoAddError(msg)).ok(); }
        Err(e) => { tx.send(AppEvent::RepoAddError(e.to_string())).ok(); }
    }
}


fn handle_delete_dialog_key(app: &mut App, key: KeyEvent, tx: &UnboundedSender<AppEvent>) {
    let dialog = match app.state.delete_worktree_dialog.as_ref() {
        Some(d) => d,
        None => return,
    };

    if dialog.is_deleting {
        return;
    }

    match key.code {
        KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
            let repo_path = dialog.repo_path.clone();
            let worktree_path = dialog.worktree_path.clone();
            let backend = dialog.backend;
            let workspace_name = dialog.workspace_name.clone();

            if let Some(d) = app.state.delete_worktree_dialog.as_mut() {
                d.is_deleting = true;
                d.error = None;
            }

            app.pty_manager.remove(&worktree_path);

            let tx = tx.clone();
            tokio::spawn(async move {
                let result = crate::vcs::remove_workspace(backend, &repo_path, &worktree_path, &workspace_name).await;
                match result {
                    Ok(()) => {
                        tx.send(AppEvent::WorktreeDeleted { repo_path, worktree_path }).ok();
                    }
                    Err(e) => {
                        tx.send(AppEvent::WorktreeDeleteError(e.to_string())).ok();
                    }
                }
            });
        }
        _ => {
            app.state.delete_worktree_dialog = None;
        }
    }
}

fn handle_dialog_key(app: &mut App, key: KeyEvent, tx: &UnboundedSender<AppEvent>) {
    let dialog = match app.state.new_worktree_dialog.as_mut() {
        Some(d) => d,
        None => return,
    };

    if dialog.is_creating {
        return;
    }

    match key.code {
        KeyCode::Esc => {
            app.state.new_worktree_dialog = None;
        }
        KeyCode::Enter => {
            let branch_name = dialog.branch_name.clone();
            let repo_idx = dialog.repo_idx;

            if branch_name.is_empty() {
                if let Some(d) = app.state.new_worktree_dialog.as_mut() {
                    d.error = Some("Branch name cannot be empty".to_string());
                }
                return;
            }

            let repo_path = match app.state.repos.get(repo_idx) {
                Some(r) => r.config.path.clone(),
                None => {
                    app.state.new_worktree_dialog = None;
                    return;
                }
            };

            if let Some(d) = app.state.new_worktree_dialog.as_mut() {
                d.is_creating = true;
                d.error = None;
            }

            let tx = tx.clone();
            tokio::spawn(async move {
                crate::state::refresh::create_worktree(repo_path, branch_name, tx).await;
            });
        }
        KeyCode::Backspace => {
            let dialog = app.state.new_worktree_dialog.as_mut().unwrap();
            if dialog.cursor_pos > 0 {
                dialog.cursor_pos -= 1;
                dialog.branch_name.remove(dialog.cursor_pos);
            }
        }
        KeyCode::Delete => {
            let dialog = app.state.new_worktree_dialog.as_mut().unwrap();
            if dialog.cursor_pos < dialog.branch_name.len() {
                dialog.branch_name.remove(dialog.cursor_pos);
            }
        }
        KeyCode::Left => {
            let dialog = app.state.new_worktree_dialog.as_mut().unwrap();
            if dialog.cursor_pos > 0 {
                dialog.cursor_pos -= 1;
            }
        }
        KeyCode::Right => {
            let dialog = app.state.new_worktree_dialog.as_mut().unwrap();
            if dialog.cursor_pos < dialog.branch_name.len() {
                dialog.cursor_pos += 1;
            }
        }
        KeyCode::Char(c) => {
            let dialog = app.state.new_worktree_dialog.as_mut().unwrap();
            dialog.branch_name.insert(dialog.cursor_pos, c);
            dialog.cursor_pos += 1;
        }
        _ => {}
    }
}

fn handle_list_key(app: &mut App, key: KeyEvent, tx: &UnboundedSender<AppEvent>) {
    tracing::debug!("list key: code={:?} modifiers={:?} kind={:?}", key.code, key.modifiers, key.kind);
    let action = match app.state.keybindings.get(key.code, key.modifiers) {
        Some(a) => *a,
        None => return,
    };

    match action {
        Action::Quit => {
            app.state.should_quit = true;
        }
        Action::MoveDown => {
            app.state.move_selection_down();
        }
        Action::MoveUp => {
            app.state.move_selection_up();
        }
        Action::FocusTerminal => {
            if let Some(path) = app.state.selected_worktree_path().cloned() {
                let (rows, cols) = exact_or_approx_pty_size(app);
                if let Err(e) = app.pty_manager.get_or_create(&path, rows, cols, tx.clone()) {
                    tracing::error!("Failed to create PTY session: {}", e);
                } else {
                    app.state.focus = PanelFocus::Terminal;
                }
            }
        }
        Action::NewWorktree => {
            let repo_idx = app.state.selected_repo_idx;
            if let Some(repo) = app.state.repos.get(repo_idx) {
                app.state.new_worktree_dialog = Some(NewWorktreeDialog::new(repo_idx, repo.backend));
            }
        }
        Action::AddRepo => {
            app.state.add_repo_dialog = Some(AddRepoDialog::new());
        }
        Action::OpenEditor => {
            if let Some(wt) = app.state.selected_worktree() {
                let path = wt.path.clone();
                std::process::Command::new("smerge")
                    .arg(&path)
                    .spawn()
                    .ok();
            }
        }
        Action::Refresh => {
            app.trigger_refresh(tx.clone());
        }
        Action::DeleteWorktree => {
            if let Some(repo) = app.state.repos.get(app.state.selected_repo_idx) {
                if let Some(wt) = repo.worktrees.get(app.state.selected_worktree_idx) {
                    if wt.is_main {
                        return;
                    }
                    app.state.delete_worktree_dialog = Some(DeleteWorktreeDialog {
                        repo_idx: app.state.selected_repo_idx,
                        worktree_idx: app.state.selected_worktree_idx,
                        repo_path: repo.config.path.clone(),
                        worktree_path: wt.path.clone(),
                        branch_name: wt.branch.clone(),
                        backend: repo.backend,
                        workspace_name: wt.name.clone(),
                        is_deleting: false,
                        error: None,
                    });
                }
            }
        }
        Action::UnfocusTerminal => {
            // UnfocusTerminal in the list context is a no-op (already unfocused)
        }
    }
}

fn handle_terminal_key(app: &mut App, key: KeyEvent, tx: &UnboundedSender<AppEvent>) {
    clear_selection(app);

    tracing::debug!("terminal key: code={:?} modifiers={:?}", key.code, key.modifiers);

    if app.state.keybindings.get(key.code, key.modifiers) == Some(&Action::UnfocusTerminal) {
        app.state.focus = PanelFocus::WorktreeList;
        return;
    }

    if let Some(path) = app.state.selected_worktree_path() {
        if let Some(session) = app.pty_manager.get_mut(path) {
            session.reset_scroll();
        }
    }

    let app_cursor = app
        .state
        .selected_worktree_path()
        .and_then(|p| app.pty_manager.get(p))
        .map(|s| s.term.lock().mode().contains(TermMode::APP_CURSOR))
        .unwrap_or(false);

    let bytes = crossterm_key_to_pty_bytes(key, app_cursor);
    if let Some(path) = app.state.selected_worktree_path().cloned() {
        if let Some(session) = app.pty_manager.get_mut(&path) {
            session.write_input(&bytes);
        }
    }
    let _ = tx;
}

fn handle_resize(app: &mut App, cols: u16, rows: u16) {
    app.terminal_size = (cols, rows);
    let (r, c) = compute_terminal_pty_size(&(cols, rows));
    app.pty_manager.resize_all(r, c);
}

fn exact_or_approx_pty_size(app: &crate::app::App) -> (u16, u16) {
    let (cols, rows) = app.last_synced_inner;
    if cols > 0 && rows > 0 {
        return (rows, cols);
    }
    compute_terminal_pty_size(&app.terminal_size)
}

fn compute_terminal_pty_size(terminal_size: &(u16, u16)) -> (u16, u16) {
    let (cols, rows) = *terminal_size;
    let inner_cols = (cols as u32 * 60 / 100).saturating_sub(2) as u16;
    let inner_rows = rows.saturating_sub(3);
    (inner_rows.max(10), inner_cols.max(20))
}

pub fn crossterm_key_to_pty_bytes(key: KeyEvent, app_cursor: bool) -> Vec<u8> {
    match key.code {
        KeyCode::Char(c) => {
            if key.modifiers.contains(KeyModifiers::CONTROL) {
                let ctrl_byte = (c as u8) & 0x1f;
                vec![ctrl_byte]
            } else if key.modifiers.contains(KeyModifiers::ALT) {
                vec![0x1b, c as u8]
            } else {
                let mut buf = [0u8; 4];
                let s = c.encode_utf8(&mut buf);
                s.as_bytes().to_vec()
            }
        }
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Delete => vec![0x1b, b'[', b'3', b'~'],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::BackTab => vec![0x1b, b'[', b'Z'],
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up    => if app_cursor { vec![0x1b, b'O', b'A'] } else { vec![0x1b, b'[', b'A'] },
        KeyCode::Down  => if app_cursor { vec![0x1b, b'O', b'B'] } else { vec![0x1b, b'[', b'B'] },
        KeyCode::Right => if app_cursor { vec![0x1b, b'O', b'C'] } else { vec![0x1b, b'[', b'C'] },
        KeyCode::Left  => if app_cursor { vec![0x1b, b'O', b'D'] } else { vec![0x1b, b'[', b'D'] },
        KeyCode::Home => vec![0x1b, b'[', b'H'],
        KeyCode::End => vec![0x1b, b'[', b'F'],
        KeyCode::PageUp => vec![0x1b, b'[', b'5', b'~'],
        KeyCode::PageDown => vec![0x1b, b'[', b'6', b'~'],
        KeyCode::Insert => vec![0x1b, b'[', b'2', b'~'],
        KeyCode::F(1) => vec![0x1b, b'O', b'P'],
        KeyCode::F(2) => vec![0x1b, b'O', b'Q'],
        KeyCode::F(3) => vec![0x1b, b'O', b'R'],
        KeyCode::F(4) => vec![0x1b, b'O', b'S'],
        KeyCode::F(5) => vec![0x1b, b'[', b'1', b'5', b'~'],
        KeyCode::F(6) => vec![0x1b, b'[', b'1', b'7', b'~'],
        KeyCode::F(7) => vec![0x1b, b'[', b'1', b'8', b'~'],
        KeyCode::F(8) => vec![0x1b, b'[', b'1', b'9', b'~'],
        KeyCode::F(9) => vec![0x1b, b'[', b'2', b'0', b'~'],
        KeyCode::F(10) => vec![0x1b, b'[', b'2', b'1', b'~'],
        KeyCode::F(11) => vec![0x1b, b'[', b'2', b'3', b'~'],
        KeyCode::F(12) => vec![0x1b, b'[', b'2', b'4', b'~'],
        _ => vec![],
    }
}
