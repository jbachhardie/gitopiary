use std::path::PathBuf;
use crate::config::RepoConfig;
use crate::keybindings::Keybindings;
use crate::vcs::VcsBackend;

#[derive(Debug, Clone)]
pub struct AppState {
    pub repos: Vec<Repository>,
    pub selected_repo_idx: usize,
    pub selected_worktree_idx: usize,
    pub focus: PanelFocus,
    pub new_worktree_dialog: Option<NewWorktreeDialog>,
    pub add_repo_dialog: Option<AddRepoDialog>,
    pub is_refreshing: bool,
    pub should_quit: bool,
    pub delete_worktree_dialog: Option<DeleteWorktreeDialog>,
    pub keybindings: Keybindings,
}

impl AppState {
    pub fn new(repos: Vec<Repository>, keybindings: Keybindings) -> Self {
        Self {
            repos,
            selected_repo_idx: 0,
            selected_worktree_idx: 0,
            focus: PanelFocus::WorktreeList,
            new_worktree_dialog: None,
            add_repo_dialog: None,
            is_refreshing: false,
            should_quit: false,
            delete_worktree_dialog: None,
            keybindings,
        }
    }

    pub fn selected_worktree(&self) -> Option<&Worktree> {
        self.repos
            .get(self.selected_repo_idx)
            .and_then(|r| r.worktrees.get(self.selected_worktree_idx))
    }

    pub fn selected_worktree_path(&self) -> Option<&PathBuf> {
        self.selected_worktree().map(|w| &w.path)
    }

    pub fn flat_list_items(&self) -> Vec<FlatListItem> {
        let mut items = vec![];
        for (repo_idx, repo) in self.repos.iter().enumerate() {
            items.push(FlatListItem::Repo {
                idx: repo_idx,
                is_selected: repo_idx == self.selected_repo_idx,
            });
            if repo.is_expanded {
                for (wt_idx, _wt) in repo.worktrees.iter().enumerate() {
                    items.push(FlatListItem::Worktree {
                        repo_idx,
                        worktree_idx: wt_idx,
                        is_selected: repo_idx == self.selected_repo_idx
                            && wt_idx == self.selected_worktree_idx,
                    });
                }
            }
        }
        items
    }

    pub fn selected_flat_idx(&self) -> usize {
        let items = self.flat_list_items();
        items.iter().position(|item| match item {
            FlatListItem::Worktree { repo_idx, worktree_idx, .. } => {
                *repo_idx == self.selected_repo_idx
                    && *worktree_idx == self.selected_worktree_idx
            }
            _ => false,
        }).unwrap_or(0)
    }

    pub fn move_selection_down(&mut self) {
        let items = self.flat_list_items();
        let current = self.selected_flat_idx();
        for i in (current + 1)..items.len() {
            if let FlatListItem::Worktree { repo_idx, worktree_idx, .. } = &items[i] {
                self.selected_repo_idx = *repo_idx;
                self.selected_worktree_idx = *worktree_idx;
                return;
            }
        }
        // wrap around
        for item in &items {
            if let FlatListItem::Worktree { repo_idx, worktree_idx, .. } = item {
                self.selected_repo_idx = *repo_idx;
                self.selected_worktree_idx = *worktree_idx;
                return;
            }
        }
    }

    pub fn move_selection_up(&mut self) {
        let items = self.flat_list_items();
        let current = self.selected_flat_idx();
        for i in (0..current).rev() {
            if let FlatListItem::Worktree { repo_idx, worktree_idx, .. } = &items[i] {
                self.selected_repo_idx = *repo_idx;
                self.selected_worktree_idx = *worktree_idx;
                return;
            }
        }
        // wrap around
        for item in items.iter().rev() {
            if let FlatListItem::Worktree { repo_idx, worktree_idx, .. } = item {
                self.selected_repo_idx = *repo_idx;
                self.selected_worktree_idx = *worktree_idx;
                return;
            }
        }
    }
}

#[derive(Debug, Clone)]
pub enum FlatListItem {
    Repo {
        idx: usize,
        is_selected: bool,
    },
    Worktree {
        repo_idx: usize,
        worktree_idx: usize,
        is_selected: bool,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum PanelFocus {
    WorktreeList,
    Terminal,
}

#[derive(Debug, Clone)]
pub struct Repository {
    pub config: RepoConfig,
    pub display_name: String,
    pub worktrees: Vec<Worktree>,
    pub is_expanded: bool,
    pub backend: VcsBackend,
}

impl Repository {
    pub fn new(config: RepoConfig) -> Self {
        let display_name = config
            .name
            .clone()
            .unwrap_or_else(|| {
                config
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| config.path.to_string_lossy().to_string())
            });
        Self {
            config,
            display_name,
            worktrees: vec![],
            is_expanded: true,
            backend: VcsBackend::Git,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Worktree {
    pub name: String,
    pub path: PathBuf,
    pub branch: String,
    pub is_main: bool,
    pub status: WorktreeStatus,
    pub pr: Option<PullRequest>,
}

#[derive(Debug, Clone, Default)]
pub struct WorktreeStatus {
    pub uncommitted_changes: u32,
    pub ahead: u32,
    pub behind: u32,
    pub is_dirty: bool,
}

#[derive(Debug, Clone)]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    pub state: PrState,
    pub is_draft: bool,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PrState {
    Open,
    Closed,
    Merged,
}

#[derive(Debug, Clone)]
pub struct NewWorktreeDialog {
    pub repo_idx: usize,
    pub backend: VcsBackend,
    pub branch_name: String,
    pub cursor_pos: usize,
    pub error: Option<String>,
    pub is_creating: bool,
}

impl NewWorktreeDialog {
    pub fn new(repo_idx: usize, backend: VcsBackend) -> Self {
        Self {
            repo_idx,
            backend,
            branch_name: String::new(),
            cursor_pos: 0,
            error: None,
            is_creating: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AddRepoDialog {
    pub path_input: String,
    pub cursor_pos: usize,
    pub error: Option<String>,
    pub is_adding: bool,
}

impl AddRepoDialog {
    pub fn new() -> Self {
        Self {
            path_input: String::new(),
            cursor_pos: 0,
            error: None,
            is_adding: false,
        }
    }

    /// Expand a leading `~` to the home directory.
    pub fn expanded_path(&self) -> std::path::PathBuf {
        if self.path_input.starts_with('~') {
            let home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("/"));
            home.join(self.path_input.trim_start_matches("~/").trim_start_matches('~'))
        } else {
            std::path::PathBuf::from(&self.path_input)
        }
    }
}

#[derive(Debug, Clone)]
pub struct DeleteWorktreeDialog {
    pub repo_idx: usize,
    pub worktree_idx: usize,
    pub repo_path: PathBuf,
    pub worktree_path: PathBuf,
    pub branch_name: String,
    pub backend: VcsBackend,
    pub workspace_name: String,
    pub is_deleting: bool,
    pub error: Option<String>,
}
