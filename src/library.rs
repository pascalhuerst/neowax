use std::path::{Path, PathBuf};

const AUDIO_EXTENSIONS: &[&str] = &["mp3", "flac", "wav", "ogg", "aac", "m4a", "opus", "aiff", "aif"];

/// A node in the library tree: either a folder (with children) or an audio file.
pub enum LibraryNode {
    Folder {
        name: String,
        children: Vec<LibraryNode>,
        open: bool,
    },
    Track {
        path: PathBuf,
        display_name: String,
    },
}

/// A flattened view item for UI rendering and navigation.
pub struct FlatEntry {
    pub kind: FlatEntryKind,
    pub depth: usize,
}

pub enum FlatEntryKind {
    Folder {
        /// Index into the tree for toggling open/closed
        node_path: Vec<usize>,
        name: String,
        open: bool,
    },
    Track {
        path: PathBuf,
        display_name: String,
    },
}

impl FlatEntry {
    pub fn display_name(&self) -> &str {
        match &self.kind {
            FlatEntryKind::Folder { name, .. } => name,
            FlatEntryKind::Track { display_name, .. } => display_name,
        }
    }

    pub fn track_path(&self) -> Option<&Path> {
        match &self.kind {
            FlatEntryKind::Track { path, .. } => Some(path),
            _ => None,
        }
    }
}

/// Scan a directory and build a tree of folders and audio files.
/// Filters out dotfiles and empty folders.
pub fn scan_directory(dir: &Path) -> Vec<LibraryNode> {
    let mut nodes = scan_dir_nodes(dir);
    sort_nodes(&mut nodes);
    nodes
}

fn scan_dir_nodes(dir: &Path) -> Vec<LibraryNode> {
    let read_dir = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) => {
            tracing::warn!("Cannot read {:?}: {}", dir, e);
            return Vec::new();
        }
    };

    let mut nodes = Vec::new();

    for entry in read_dir.flatten() {
        let path = entry.path();
        let file_name = entry.file_name();
        let name_str = file_name.to_string_lossy();

        // Skip dotfiles/dotfolders (macOS metadata etc.)
        if name_str.starts_with('.') {
            continue;
        }

        if path.is_dir() {
            let children = scan_dir_nodes(&path);
            // Only include folders that contain audio files (directly or nested)
            if !children.is_empty() {
                nodes.push(LibraryNode::Folder {
                    name: name_str.into_owned(),
                    children,
                    open: false,
                });
            }
        } else if path.is_file() {
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if AUDIO_EXTENSIONS.contains(&ext.to_lowercase().as_str()) {
                    let display_name = path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("Unknown")
                        .to_string();
                    nodes.push(LibraryNode::Track { path, display_name });
                }
            }
        }
    }

    nodes
}

fn sort_nodes(nodes: &mut Vec<LibraryNode>) {
    nodes.sort_by(|a, b| {
        let a_name = match a {
            LibraryNode::Folder { name, .. } => name,
            LibraryNode::Track { display_name, .. } => display_name,
        };
        let b_name = match b {
            LibraryNode::Folder { name, .. } => name,
            LibraryNode::Track { display_name, .. } => display_name,
        };
        // Folders first, then tracks; alphabetical within each group
        let a_is_folder = matches!(a, LibraryNode::Folder { .. });
        let b_is_folder = matches!(b, LibraryNode::Folder { .. });
        b_is_folder
            .cmp(&a_is_folder)
            .then_with(|| a_name.to_lowercase().cmp(&b_name.to_lowercase()))
    });
    for node in nodes.iter_mut() {
        if let LibraryNode::Folder { children, .. } = node {
            sort_nodes(children);
        }
    }
}

/// Flatten the tree into a list of visible entries for UI rendering.
/// Only descends into open folders.
pub fn flatten_tree(nodes: &[LibraryNode]) -> Vec<FlatEntry> {
    let mut entries = Vec::new();
    flatten_recursive(nodes, 0, &mut vec![], &mut entries);
    entries
}

fn flatten_recursive(
    nodes: &[LibraryNode],
    depth: usize,
    node_path: &mut Vec<usize>,
    entries: &mut Vec<FlatEntry>,
) {
    for (i, node) in nodes.iter().enumerate() {
        node_path.push(i);
        match node {
            LibraryNode::Folder {
                name,
                children,
                open,
            } => {
                entries.push(FlatEntry {
                    kind: FlatEntryKind::Folder {
                        node_path: node_path.clone(),
                        name: name.clone(),
                        open: *open,
                    },
                    depth,
                });
                if *open {
                    flatten_recursive(children, depth + 1, node_path, entries);
                }
            }
            LibraryNode::Track { path, display_name } => {
                entries.push(FlatEntry {
                    kind: FlatEntryKind::Track {
                        path: path.clone(),
                        display_name: display_name.clone(),
                    },
                    depth,
                });
            }
        }
        node_path.pop();
    }
}

/// Toggle a folder open/closed given its path in the tree.
pub fn toggle_folder(nodes: &mut [LibraryNode], path: &[usize]) {
    if path.is_empty() {
        return;
    }
    let idx = path[0];
    if idx >= nodes.len() {
        return;
    }
    if path.len() == 1 {
        if let LibraryNode::Folder { open, .. } = &mut nodes[idx] {
            *open = !*open;
        }
    } else if let LibraryNode::Folder { children, .. } = &mut nodes[idx] {
        toggle_folder(children, &path[1..]);
    }
}
