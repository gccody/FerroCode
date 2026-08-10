use ferro_code_protocol::hidden_command;
use std::{fs, path::Path, process::Output};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitStatus {
    pub installed: bool,
    pub is_repository: bool,
    pub has_changes: bool,
    pub has_commits: bool,
    pub has_github_remote: bool,
    pub has_unpushed_commits: bool,
    pub github_configured: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkspaceSnapshot {
    pub diff: String,
    pub files: Vec<String>,
    pub git: GitStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitAction {
    Initialize,
    Publish,
    Push,
}

pub(crate) fn inspect(
    root: Option<&str>,
    respect_gitignore: bool,
    known_github_configuration: Option<bool>,
) -> WorkspaceSnapshot {
    let github_configured = known_github_configuration
        .unwrap_or_else(|| command_success("gh", &["auth", "status", "--hostname", "github.com"]));
    let Some(root) = root else {
        return WorkspaceSnapshot {
            git: GitStatus {
                installed: command_success("git", &["--version"]),
                github_configured,
                ..GitStatus::default()
            },
            ..WorkspaceSnapshot::default()
        };
    };

    let (installed, status) = match hidden_command("git")
        .args(["-C", root, "status", "--short", "--branch"])
        .output()
    {
        Ok(output) => (
            true,
            output
                .status
                .success()
                .then(|| String::from_utf8_lossy(&output.stdout).into_owned()),
        ),
        Err(_) => (false, None),
    };
    let is_repository = status.is_some();
    let branch_status = status
        .as_deref()
        .and_then(|status| status.lines().next())
        .unwrap_or_default();
    let diff = status
        .as_deref()
        .map(|status| status.lines().skip(1).collect::<Vec<_>>().join("\n"))
        .unwrap_or_default();
    let mut files = if respect_gitignore && is_repository {
        git_visible_files(root).unwrap_or_else(|| filesystem_files(root))
    } else {
        filesystem_files(root)
    };
    files.sort();
    files.truncate(500);

    let has_commits = is_repository
        && !branch_status.contains("No commits yet")
        && !branch_status.contains("Initial commit");
    let origin = is_repository
        .then(|| command_stdout("git", &["-C", root, "remote", "get-url", "origin"]))
        .flatten()
        .unwrap_or_default();
    let has_github_remote = origin.to_ascii_lowercase().contains("github.com");
    let has_upstream = branch_status.contains("...");
    let is_ahead = branch_status
        .split("[ahead ")
        .nth(1)
        .and_then(|suffix| {
            suffix
                .split(|character: char| !character.is_ascii_digit())
                .next()
        })
        .and_then(|count| count.parse::<u64>().ok())
        .is_some_and(|count| count > 0);
    let has_unpushed_commits = if has_upstream {
        is_ahead
    } else {
        has_github_remote && has_commits
    };

    let has_changes = !diff.trim().is_empty();
    WorkspaceSnapshot {
        diff,
        files,
        git: GitStatus {
            installed,
            is_repository,
            has_changes,
            has_commits,
            has_github_remote,
            has_unpushed_commits,
            github_configured,
        },
    }
}

pub(crate) fn commit_context(root: &str) -> Result<String, String> {
    let status = run_command("git", &["-C", root, "status", "--short"])?;
    let diff = command_stdout(
        "git",
        &["-C", root, "diff", "--no-ext-diff", "--stat", "--"],
    )
    .unwrap_or_default();
    let patch = command_stdout(
        "git",
        &[
            "-C",
            root,
            "diff",
            "--no-ext-diff",
            "--",
            ":(exclude)Cargo.lock",
        ],
    )
    .unwrap_or_default();
    let context = format!("Changed files:\n{status}\nDiff summary:\n{diff}\nDiff:\n{patch}");
    Ok(context.chars().take(24_000).collect())
}

pub(crate) fn commit(root: &str, message: &str) -> Result<String, String> {
    run_command("git", &["-C", root, "add", "--all"])?;
    run_command("git", &["-C", root, "commit", "-m", message])?;
    Ok(format!("Committed changes: {message}"))
}

pub(crate) fn run_action(
    root: &str,
    action: GitAction,
    private_repository: bool,
) -> Result<String, String> {
    match action {
        GitAction::Initialize => {
            let output = hidden_command("git")
                .args(["-C", root, "init", "-b", "main"])
                .output()
                .map_err(|error| format!("Could not start Git: {error}"))?;
            if !output.status.success() {
                run_command("git", &["-C", root, "init"])?;
            }
            Ok("Initialized Git repository".into())
        }
        GitAction::Publish => {
            ensure_github_configured()?;
            let repo_name = Path::new(root)
                .file_name()
                .and_then(|name| name.to_str())
                .filter(|name| !name.is_empty())
                .ok_or("The project folder does not have a valid repository name")?;
            let args = vec![
                "repo",
                "create",
                repo_name,
                "--source",
                root,
                "--remote",
                "origin",
                if private_repository {
                    "--private"
                } else {
                    "--public"
                },
            ];
            run_command("gh", &args)?;
            if command_success("git", &["-C", root, "rev-parse", "--verify", "HEAD"])
                && let Err(error) = push_to_github(root)
            {
                return Err(format!(
                    "The GitHub repository was created, but the initial push failed: {error}"
                ));
            }
            Ok(format!("Published {repo_name} to GitHub"))
        }
        GitAction::Push => {
            ensure_github_configured()?;
            push_to_github(root)?;
            Ok("Pushed changes to GitHub".into())
        }
    }
}

fn ensure_github_configured() -> Result<(), String> {
    if command_success("gh", &["auth", "status", "--hostname", "github.com"]) {
        Ok(())
    } else {
        Err("GitHub is not configured. Run `gh auth login` and try again.".into())
    }
}

fn push_to_github(root: &str) -> Result<(), String> {
    configure_github_https_remote(root)?;
    run_command(
        "git",
        &[
            "-C",
            root,
            "-c",
            "credential.https://github.com.helper=",
            "-c",
            "credential.https://github.com.helper=!gh auth git-credential",
            "push",
            "--set-upstream",
            "origin",
            "HEAD",
        ],
    )?;
    Ok(())
}

fn configure_github_https_remote(root: &str) -> Result<String, String> {
    let origin = run_command("git", &["-C", root, "remote", "get-url", "origin"])?;
    let https = github_https_remote(&origin)
        .ok_or("The origin remote is not a supported GitHub repository URL")?;
    if origin != https {
        run_command("git", &["-C", root, "remote", "set-url", "origin", &https])?;
    }
    Ok(https)
}

fn github_https_remote(remote: &str) -> Option<String> {
    let remote = remote.trim();
    let path = remote
        .strip_prefix("git@github.com:")
        .or_else(|| remote.strip_prefix("ssh://git@github.com/"))
        .or_else(|| remote.strip_prefix("ssh://github.com/"))
        .or_else(|| remote.strip_prefix("https://github.com/"))
        .or_else(|| remote.strip_prefix("http://github.com/"))?
        .trim_start_matches('/');
    let mut segments = path.trim_end_matches('/').split('/');
    let owner = segments.next().filter(|segment| !segment.is_empty())?;
    let repository = segments.next().filter(|segment| !segment.is_empty())?;
    if segments.next().is_some() {
        return None;
    }
    Some(format!("https://github.com/{owner}/{repository}"))
}

fn command_success(program: &str, args: &[&str]) -> bool {
    hidden_command(program)
        .args(args)
        .output()
        .is_ok_and(|output| output.status.success())
}

fn command_stdout(program: &str, args: &[&str]) -> Option<String> {
    let output = hidden_command(program).args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn run_command(program: &str, args: &[&str]) -> Result<String, String> {
    let output = hidden_command(program)
        .args(args)
        .output()
        .map_err(|error| format!("Could not start {program}: {error}"))?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned());
    }
    Err(command_error(program, output))
}

fn command_error(program: &str, output: Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let detail = if !stderr.is_empty() { stderr } else { stdout };
    if detail.is_empty() {
        format!("{program} exited with {}", output.status)
    } else {
        detail
    }
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

    fn temp_workspace(name: &str) -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("ferro-code-{name}-{}-{nonce}", std::process::id()))
    }

    #[test]
    fn inspector_can_respect_or_ignore_gitignore_rules() {
        let root = temp_workspace("gitignore");
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

        let root_text = root.to_str().unwrap();
        let filtered = inspect(Some(root_text), true, Some(false)).files;
        let unfiltered = inspect(Some(root_text), false, Some(false)).files;

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

    #[test]
    fn repository_status_distinguishes_changes_and_commits() {
        let root = temp_workspace("status");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("new.txt"), "new").unwrap();
        let root_text = root.to_str().unwrap();

        run_action(root_text, GitAction::Initialize, true).unwrap();
        let status = inspect(Some(root_text), true, Some(false)).git;
        assert!(status.installed);
        assert!(status.is_repository);
        assert!(status.has_changes);
        assert!(!status.has_commits);

        run_command(
            "git",
            &["-C", root_text, "config", "user.name", "Ferro Test"],
        )
        .unwrap();
        run_command(
            "git",
            &[
                "-C",
                root_text,
                "config",
                "user.email",
                "ferro@example.test",
            ],
        )
        .unwrap();
        commit(root_text, "Add new file").unwrap();
        let committed = inspect(Some(root_text), true, Some(false)).git;
        assert!(committed.has_commits);
        assert!(!committed.has_changes);

        run_command(
            "git",
            &[
                "-C",
                root_text,
                "remote",
                "add",
                "origin",
                "git@github.com:ferro-code/example.git",
            ],
        )
        .unwrap();
        let https = configure_github_https_remote(root_text).unwrap();
        assert_eq!(https, "https://github.com/ferro-code/example.git");
        assert_eq!(
            command_stdout("git", &["-C", root_text, "remote", "get-url", "origin"]).as_deref(),
            Some("https://github.com/ferro-code/example.git")
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn github_remote_urls_are_normalized_to_https() {
        for remote in [
            "git@github.com:owner/repo.git",
            "ssh://git@github.com/owner/repo.git",
            "ssh://github.com/owner/repo.git",
            "http://github.com/owner/repo.git",
            "https://github.com/owner/repo.git",
        ] {
            assert_eq!(
                github_https_remote(remote).as_deref(),
                Some("https://github.com/owner/repo.git")
            );
        }
        assert_eq!(github_https_remote("git@example.com:owner/repo.git"), None);
        assert_eq!(github_https_remote("https://github.com/owner"), None);
    }
}
