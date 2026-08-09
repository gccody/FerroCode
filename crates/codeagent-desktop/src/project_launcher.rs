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

    pub(super) fn icon(&self) -> Option<slint::Image> {
        executable_icon(&self.program)
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

#[cfg(windows)]
fn executable_icon(program: &Path) -> Option<slint::Image> {
    use slint::{Image, Rgba8Pixel, SharedPixelBuffer};
    use std::{iter, os::windows::ffi::OsStrExt};
    use windows::{
        Win32::{
            Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES,
            UI::{
                Shell::{SHFILEINFOW, SHGFI_ICON, SHGetFileInfoW},
                WindowsAndMessaging::DestroyIcon,
            },
        },
        core::PCWSTR,
    };

    const ICON_SIZE: u32 = 32;
    let normalized_program = OsString::from(program.to_string_lossy().replace('/', "\\"));
    let wide_path = normalized_program
        .as_os_str()
        .encode_wide()
        .chain(iter::once(0))
        .collect::<Vec<_>>();
    let mut file_info = SHFILEINFOW::default();
    let result = unsafe {
        SHGetFileInfoW(
            PCWSTR(wide_path.as_ptr()),
            FILE_FLAGS_AND_ATTRIBUTES(0),
            Some(&mut file_info),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON,
        )
    };
    if result == 0 || file_info.hIcon.0.is_null() {
        return None;
    }

    let pixels = unsafe { hicon_rgba(file_info.hIcon, ICON_SIZE) };
    let _ = unsafe { DestroyIcon(file_info.hIcon) };
    pixels.map(|pixels| {
        let buffer = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
            pixels.as_slice(),
            ICON_SIZE,
            ICON_SIZE,
        );
        Image::from_rgba8_premultiplied(buffer)
    })
}

#[cfg(windows)]
unsafe fn hicon_rgba(
    icon: windows::Win32::UI::WindowsAndMessaging::HICON,
    size: u32,
) -> Option<Vec<u8>> {
    use windows::Win32::{
        Graphics::Gdi::{
            BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection,
            DIB_RGB_COLORS, DeleteDC, DeleteObject, HGDIOBJ, SelectObject,
        },
        UI::WindowsAndMessaging::{DI_NORMAL, DrawIconEx},
    };

    let memory_dc = unsafe { CreateCompatibleDC(None) };
    if memory_dc.0.is_null() {
        return None;
    }

    let bitmap_info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: size as i32,
            biHeight: -(size as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits = std::ptr::null_mut();
    let bitmap = match unsafe {
        CreateDIBSection(
            Some(memory_dc),
            &bitmap_info,
            DIB_RGB_COLORS,
            &mut bits,
            None,
            0,
        )
    } {
        Ok(bitmap) => bitmap,
        Err(_) => {
            let _ = unsafe { DeleteDC(memory_dc) };
            return None;
        }
    };

    let bitmap_object = HGDIOBJ(bitmap.0);
    let previous_object = unsafe { SelectObject(memory_dc, bitmap_object) };
    let byte_count = size as usize * size as usize * 4;
    let pixels = if previous_object.0.is_null() || bits.is_null() {
        None
    } else {
        unsafe { std::ptr::write_bytes(bits, 0, byte_count) };
        if unsafe {
            DrawIconEx(
                memory_dc,
                0,
                0,
                icon,
                size as i32,
                size as i32,
                0,
                None,
                DI_NORMAL,
            )
        }
        .is_ok()
        {
            let mut rgba =
                unsafe { std::slice::from_raw_parts(bits.cast::<u8>(), byte_count).to_vec() };
            for pixel in rgba.chunks_exact_mut(4) {
                pixel.swap(0, 2);
            }
            if rgba.chunks_exact(4).all(|pixel| pixel[3] == 0) {
                for pixel in rgba.chunks_exact_mut(4) {
                    if pixel[..3] != [0, 0, 0] {
                        pixel[3] = 255;
                    }
                }
            }
            Some(rgba)
        } else {
            None
        }
    };

    if !previous_object.0.is_null() {
        unsafe { SelectObject(memory_dc, previous_object) };
    }
    let _ = unsafe { DeleteObject(bitmap_object) };
    let _ = unsafe { DeleteDC(memory_dc) };
    pixels
}

#[cfg(not(windows))]
fn executable_icon(_program: &Path) -> Option<slint::Image> {
    None
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

    #[cfg(windows)]
    #[test]
    fn windows_methods_expose_renderable_shell_icons() {
        for method in available_open_methods() {
            let icon = method.icon().unwrap_or_else(|| {
                panic!(
                    "missing shell icon for {} at {}",
                    method.label,
                    method.program.display()
                )
            });
            let pixels = icon
                .to_rgba8()
                .unwrap_or_else(|| panic!("invalid shell icon for {}", method.label));
            assert_eq!(pixels.width(), 32);
            assert_eq!(pixels.height(), 32);
            assert!(
                pixels.as_bytes().chunks_exact(4).any(|pixel| pixel[3] != 0),
                "shell icon for {} is fully transparent",
                method.label
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
