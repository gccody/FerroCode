use std::process::Command;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

// Prevent console-subsystem child processes from creating a transient terminal
// when they are launched by the GUI. This also leaves descendants without a
// console to inherit.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub fn hidden_command(program: &str) -> Command {
    let mut command = Command::new(program);
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    command
}
