use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct OpenMethod {
    label: String,
    program: PathBuf,
    arguments: Vec<OsString>,
    pass_project_path: bool,
    use_project_as_working_directory: bool,
}

impl OpenMethod {
    fn new(
        label: impl Into<String>,
        program: PathBuf,
        arguments: impl IntoIterator<Item = impl Into<OsString>>,
        pass_project_path: bool,
        use_project_as_working_directory: bool,
    ) -> Self {
        Self {
            label: label.into(),
            program,
            arguments: arguments.into_iter().map(Into::into).collect(),
            pass_project_path,
            use_project_as_working_directory,
        }
    }

    pub(super) fn label(&self) -> &str {
        &self.label
    }

    pub(super) fn open(&self, project_path: &Path) -> Result<(), String> {
        if !project_path.is_dir() {
            return Err(format!(
                "Project folder does not exist: {}",
                project_path.display()
            ));
        }

        let mut command = Command::new(&self.program);
        command.args(&self.arguments);
        if self.pass_project_path {
            command.arg(project_path);
        }
        if self.use_project_as_working_directory {
            command.current_dir(project_path);
        }
        command
            .spawn()
            .map(|_| ())
            .map_err(|error| format!("Could not open project in {}: {error}", self.label))
    }
}

pub(super) fn available_open_methods() -> Vec<OpenMethod> {
    platform_open_methods()
}

#[cfg(windows)]
fn platform_open_methods() -> Vec<OpenMethod> {
    let mut methods = Vec::new();

    if let Some(program) = windows_app_executable(
        "Code.exe",
        &[
            ("LOCALAPPDATA", "Programs/Microsoft VS Code/Code.exe"),
            ("ProgramFiles", "Microsoft VS Code/Code.exe"),
            ("ProgramFiles(x86)", "Microsoft VS Code/Code.exe"),
        ],
        "code.cmd",
    ) {
        methods.push(OpenMethod::new(
            "Visual Studio Code",
            program,
            [] as [&str; 0],
            true,
            false,
        ));
    }

    if let Some(program) = windows_app_executable(
        "Cursor.exe",
        &[
            ("LOCALAPPDATA", "Programs/cursor/Cursor.exe"),
            ("ProgramFiles", "Cursor/Cursor.exe"),
        ],
        "cursor.cmd",
    ) {
        methods.push(OpenMethod::new(
            "Cursor",
            program,
            [] as [&str; 0],
            true,
            false,
        ));
    }

    let explorer = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .map(|root| root.join("explorer.exe"))
        .filter(|path| path.is_file())
        .or_else(|| find_on_path("explorer.exe"));
    if let Some(program) = explorer {
        methods.push(OpenMethod::new(
            "File Explorer",
            program,
            [] as [&str; 0],
            true,
            false,
        ));
    }

    let powershell = find_on_path("pwsh.exe")
        .or_else(|| find_on_path("powershell.exe"))
        .or_else(|| {
            std::env::var_os("SystemRoot")
                .map(PathBuf::from)
                .map(|root| root.join("System32/WindowsPowerShell/v1.0/powershell.exe"))
                .filter(|path| path.is_file())
        });
    if let Some(program) = powershell {
        methods.push(OpenMethod::new(
            "PowerShell",
            program,
            ["-NoExit"],
            false,
            true,
        ));
    }

    let command_prompt = std::env::var_os("COMSPEC")
        .map(PathBuf::from)
        .filter(|path| path.is_file())
        .or_else(|| find_on_path("cmd.exe"));
    if let Some(program) = command_prompt {
        methods.push(OpenMethod::new(
            "Command Prompt",
            program,
            ["/D", "/K"],
            false,
            true,
        ));
    }

    if let Some(program) = find_on_path("wt.exe") {
        methods.push(OpenMethod::new(
            "Windows Terminal",
            program,
            ["-d"],
            true,
            false,
        ));
    }

    methods
}

#[cfg(windows)]
fn windows_app_executable(
    executable_name: &str,
    known_locations: &[(&str, &str)],
    cli_name: &str,
) -> Option<PathBuf> {
    find_on_path(executable_name)
        .or_else(|| {
            known_locations.iter().find_map(|(variable, suffix)| {
                std::env::var_os(variable)
                    .map(PathBuf::from)
                    .map(|root| root.join(suffix))
                    .filter(|path| path.is_file())
            })
        })
        .or_else(|| {
            let cli = find_on_path(cli_name)?;
            let executable = cli.parent()?.parent()?.join(executable_name);
            executable.is_file().then_some(executable)
        })
}

#[cfg(target_os = "macos")]
fn platform_open_methods() -> Vec<OpenMethod> {
    let mut methods = Vec::new();
    let open = PathBuf::from("/usr/bin/open");
    let open = open
        .is_file()
        .then_some(open)
        .or_else(|| find_on_path("open"));

    if let Some(program) = find_on_path("code") {
        methods.push(OpenMethod::new(
            "Visual Studio Code",
            program,
            [] as [&str; 0],
            true,
            false,
        ));
    } else if mac_application_exists("Visual Studio Code.app")
        && let Some(program) = open.clone()
    {
        methods.push(OpenMethod::new(
            "Visual Studio Code",
            program,
            ["-a", "Visual Studio Code"],
            true,
            false,
        ));
    }

    if let Some(program) = find_on_path("cursor") {
        methods.push(OpenMethod::new(
            "Cursor",
            program,
            [] as [&str; 0],
            true,
            false,
        ));
    } else if mac_application_exists("Cursor.app")
        && let Some(program) = open.clone()
    {
        methods.push(OpenMethod::new(
            "Cursor",
            program,
            ["-a", "Cursor"],
            true,
            false,
        ));
    }

    if mac_application_exists("Terminal.app")
        && let Some(program) = open.clone()
    {
        methods.push(OpenMethod::new(
            "Terminal",
            program,
            ["-a", "Terminal"],
            true,
            false,
        ));
    }
    if mac_application_exists("iTerm.app")
        && let Some(program) = open.clone()
    {
        methods.push(OpenMethod::new(
            "iTerm",
            program,
            ["-a", "iTerm"],
            true,
            false,
        ));
    }
    if let Some(program) = open {
        methods.push(OpenMethod::new(
            "Finder",
            program,
            [] as [&str; 0],
            true,
            false,
        ));
    }

    methods
}

#[cfg(target_os = "macos")]
fn mac_application_exists(name: &str) -> bool {
    [
        Some(PathBuf::from("/Applications").join(name)),
        Some(PathBuf::from("/System/Applications/Utilities").join(name)),
        Some(PathBuf::from("/Applications/Utilities").join(name)),
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|home| home.join("Applications").join(name)),
    ]
    .into_iter()
    .flatten()
    .any(|path| path.is_dir())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn platform_open_methods() -> Vec<OpenMethod> {
    let mut methods = Vec::new();

    for (label, command) in [
        ("Visual Studio Code", "code"),
        ("VSCodium", "codium"),
        ("Cursor", "cursor"),
    ] {
        if let Some(program) = find_on_path(command) {
            methods.push(OpenMethod::new(
                label,
                program,
                [] as [&str; 0],
                true,
                false,
            ));
        }
    }

    if let Some(program) = find_on_path("xdg-open") {
        methods.push(OpenMethod::new(
            "Files",
            program,
            [] as [&str; 0],
            true,
            false,
        ));
    }

    for (label, command) in [
        ("Terminal", "kgx"),
        ("GNOME Terminal", "gnome-terminal"),
        ("Konsole", "konsole"),
        ("Xfce Terminal", "xfce4-terminal"),
        ("Alacritty", "alacritty"),
        ("Kitty", "kitty"),
        ("xterm", "xterm"),
    ] {
        if let Some(program) = find_on_path(command) {
            methods.push(OpenMethod::new(
                label,
                program,
                [] as [&str; 0],
                false,
                true,
            ));
        }
    }

    methods
}

#[cfg(not(any(windows, unix)))]
fn platform_open_methods() -> Vec<OpenMethod> {
    Vec::new()
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    let name = Path::new(name);
    if name.components().count() > 1 {
        return executable_file(name).then(|| name.to_path_buf());
    }

    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .map(|directory| directory.join(name))
        .find(|candidate| executable_file(candidate))
}

fn executable_file(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn methods_are_unique_and_point_to_available_programs() {
        let methods = available_open_methods();
        for (index, method) in methods.iter().enumerate() {
            assert!(executable_file(&method.program));
            assert!(
                methods[index + 1..]
                    .iter()
                    .all(|other| other.label != method.label)
            );
        }
    }

    #[test]
    fn opening_requires_an_existing_project_directory() {
        let method = OpenMethod::new(
            "Test",
            PathBuf::from("unused"),
            [] as [&str; 0],
            false,
            false,
        );
        let missing = std::env::temp_dir().join("codeagent-missing-project-folder");
        let error = method.open(&missing).unwrap_err();
        assert!(error.starts_with("Project folder does not exist:"));
    }
}
