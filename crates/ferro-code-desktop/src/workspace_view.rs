use crate::{ChangeRow, FileRow};
use std::collections::{BTreeMap, HashSet};

pub(super) struct ParsedChangeRow {
    pub(super) row: ChangeRow,
    pub(super) path: String,
    pub(super) staged: bool,
    pub(super) unstaged: bool,
}

pub(super) fn change_rows(status: &str) -> Vec<ParsedChangeRow> {
    status
        .lines()
        .filter_map(|line| {
            let bytes = line.as_bytes();
            if bytes.len() < 4 {
                return None;
            }

            let index_status = bytes[0] as char;
            let worktree_status = bytes[1] as char;
            let raw_path = line[3..].trim();
            if raw_path.is_empty() || (index_status == '!' && worktree_status == '!') {
                return None;
            }

            let conflicted = matches!(
                (index_status, worktree_status),
                ('D', 'D')
                    | ('A', 'U')
                    | ('U', 'D')
                    | ('U', 'A')
                    | ('D', 'U')
                    | ('A', 'A')
                    | ('U', 'U')
            );
            let untracked = index_status == '?' && worktree_status == '?';
            let status_code = if conflicted {
                '!'
            } else if untracked {
                'U'
            } else if worktree_status != ' ' {
                worktree_status
            } else {
                index_status
            };
            let status_label = match status_code {
                'A' => "Added",
                'D' => "Deleted",
                'R' => "Renamed",
                'C' => "Copied",
                'U' if conflicted => "Conflict",
                'U' => "Untracked",
                '!' => "Conflict",
                'T' => "Type changed",
                _ => "Modified",
            };
            let staged = !untracked && index_status != ' ' && index_status != '?';
            let unstaged = untracked || worktree_status != ' ';
            let state_label = match (staged, unstaged) {
                (true, true) => "BOTH",
                (true, false) => "STAGED",
                _ => "UNSTAGED",
            };

            // For renames, make the destination the prominent file name while
            // retaining the complete old-to-new path in the supporting detail.
            let display_path = raw_path.rsplit_once(" -> ").map_or(raw_path, |(_, to)| to);
            let name = display_path
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or(display_path);
            let parent = display_path
                .rfind(['/', '\\'])
                .map(|separator| &display_path[..separator])
                .unwrap_or("");
            let detail = if raw_path.contains(" -> ") {
                format!("{status_label} · {raw_path}")
            } else if parent.is_empty() {
                status_label.to_owned()
            } else {
                format!("{status_label} · {parent}")
            };

            Some(ParsedChangeRow {
                row: ChangeRow {
                    name: name.into(),
                    detail: detail.into(),
                    status: status_code.to_string().into(),
                    status_label: status_label.into(),
                    state_label: state_label.into(),
                },
                path: display_path.to_owned(),
                staged,
                unstaged,
            })
        })
        .collect()
}

#[derive(Default)]
pub(super) struct FileTreeNode {
    directories: BTreeMap<String, FileTreeNode>,
    files: BTreeMap<String, String>,
}

pub(super) fn file_rows(paths: &[String], changed: &HashSet<String>) -> Vec<FileRow> {
    let mut root = FileTreeNode::default();
    for path in paths {
        let components = path
            .split(['\\', '/'])
            .filter(|component| !component.is_empty())
            .collect::<Vec<_>>();
        let Some((file_name, directories)) = components.split_last() else {
            continue;
        };
        let mut node = &mut root;
        for directory in directories {
            node = node.directories.entry((*directory).to_owned()).or_default();
        }
        node.files.insert((*file_name).to_owned(), path.clone());
    }

    let mut rows = Vec::new();
    append_file_rows(&root, 0, changed, &mut rows);
    rows
}

pub(super) fn append_file_rows(
    node: &FileTreeNode,
    depth: i32,
    changed: &HashSet<String>,
    rows: &mut Vec<FileRow>,
) {
    append_file_rows_with_guides(node, depth, changed, [false; 6], rows);
}

pub(super) fn append_file_rows_with_guides(
    node: &FileTreeNode,
    depth: i32,
    changed: &HashSet<String>,
    guides: [bool; 6],
    rows: &mut Vec<FileRow>,
) {
    let child_count = node.directories.len() + node.files.len();
    let mut child_index = 0;

    for (name, child) in &node.directories {
        child_index += 1;
        let last = child_index == child_count;
        rows.push(FileRow {
            name: name.clone().into(),
            depth,
            directory: true,
            changed: false,
            connector_height: if last { 12 } else { 26 },
            guide0: guides[0],
            guide1: guides[1],
            guide2: guides[2],
            guide3: guides[3],
            guide4: guides[4],
            guide5: guides[5],
        });

        let mut child_guides = guides;
        if let Some(guide) = child_guides.get_mut(depth as usize) {
            *guide = !last;
        }
        append_file_rows_with_guides(child, depth + 1, changed, child_guides, rows);
    }
    for (name, path) in &node.files {
        child_index += 1;
        let last = child_index == child_count;
        rows.push(FileRow {
            name: name.clone().into(),
            depth,
            directory: false,
            changed: changed.contains(path),
            connector_height: if last { 12 } else { 26 },
            guide0: guides[0],
            guide1: guides[1],
            guide2: guides[2],
            guide3: guides[3],
            guide4: guides[4],
            guide5: guides[5],
        });
    }
}
