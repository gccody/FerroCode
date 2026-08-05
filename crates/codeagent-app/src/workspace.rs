use codeagent_protocol::hidden_command;
use std::{fs, path::Path};

pub(crate) fn inspect(root: &str, respect_gitignore: bool) -> (String, Vec<String>) {
    let diff = hidden_command("git")
        .args(["-C", root, "status", "--short"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        .unwrap_or_default();
    let mut files = if respect_gitignore {
        git_visible_files(root).unwrap_or_else(|| filesystem_files(root))
    } else {
        filesystem_files(root)
    };
    files.sort();
    files.truncate(500);
    (diff, files)
}

fn git_visible_files(root: &str) -> Option<Vec<String>> {
    let output = hidden_command("git")
        .args([
            "-C",
            root,
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .output()
        .ok()
        .filter(|output| output.status.success())?;

    Some(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|path| visible_relative_path(Path::new(path)))
            .map(|path| path.replace('/', std::path::MAIN_SEPARATOR_STR))
            .collect(),
    )
}

fn filesystem_files(root: &str) -> Vec<String> {
    let mut files = Vec::new();
    collect_files(Path::new(root), Path::new(root), 0, &mut files);
    files
}

fn visible_relative_path(path: &Path) -> bool {
    let components = path.components().collect::<Vec<_>>();
    components.len() <= 7
        && components.iter().all(|component| {
            let name = component.as_os_str().to_string_lossy();
            !name.starts_with('.') && !matches!(name.as_ref(), "target" | "node_modules")
        })
}

fn collect_files(root: &Path, directory: &Path, depth: usize, files: &mut Vec<String>) {
    if depth > 6 || files.len() >= 500 {
        return;
    }
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || matches!(name.as_ref(), "target" | "node_modules") {
            continue;
        }
        if path.is_dir() {
            collect_files(root, &path, depth + 1, files);
        } else if let Ok(relative) = path.strip_prefix(root) {
            files.push(relative.to_string_lossy().into_owned());
        }
        if files.len() >= 500 {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn inspector_can_respect_or_ignore_gitignore_rules() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "codeagent-gitignore-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(root.join("ignored")).unwrap();
        fs::write(root.join(".gitignore"), "*.log\nignored/\n").unwrap();
        fs::write(root.join("visible.txt"), "visible").unwrap();
        fs::write(root.join("debug.log"), "ignored").unwrap();
        fs::write(root.join("ignored").join("nested.txt"), "ignored").unwrap();

        let initialized = hidden_command("git")
            .args(["init", "--quiet", root.to_str().unwrap()])
            .status()
            .unwrap();
        assert!(initialized.success());

        let root = root.to_str().unwrap();
        let filtered = inspect(root, true).1;
        let unfiltered = inspect(root, false).1;

        assert!(filtered.iter().any(|path| path == "visible.txt"));
        assert!(!filtered.iter().any(|path| path == "debug.log"));
        assert!(!filtered.iter().any(|path| path.contains("ignored")));
        assert!(unfiltered.iter().any(|path| path == "debug.log"));
        assert!(
            unfiltered
                .iter()
                .any(|path| Path::new(path).ends_with(Path::new("ignored").join("nested.txt")))
        );

        fs::remove_dir_all(root).unwrap();
    }
}
