//! 外部进程启动的平台适配。
//!
//! Windows 绿色版不能闪出控制台窗口：ffmpeg、人声分离 worker 等子进程都必须带
//! `CREATE_NO_WINDOW`。该标志只能用 `std::os::windows::process::CommandExt` 设置，
//! 因此把它收在这里一处，避免各调用点各自复制一份魔数。

use std::path::Path;
use std::process::{Command, Stdio};

/// ffmpeg 可执行文件名：Windows 需要显式的 `.exe` 后缀。
pub(crate) const FFMPEG: &str = if cfg!(windows) {
    "ffmpeg.exe"
} else {
    "ffmpeg"
};

/// 启动外部命令，并按平台抑制控制台窗口。
pub fn command(program: &Path) -> Command {
    let mut command = Command::new(program);
    suppress_console(&mut command);
    command
}

/// 同上，并把 stdout 接到 null：ffmpeg 这类工具的输出量不适合留在内存里。
pub fn quiet_command(program: &Path) -> Command {
    let mut command = command(program);
    command.stdout(Stdio::null());
    command
}

/// 就地关闭控制台窗口，供已经组装好参数/环境/工作目录的命令复用。
pub fn suppress_console(command: &mut Command) {
    apply_suppress_console(command);
}

#[cfg(windows)]
fn apply_suppress_console(command: &mut Command) {
    use std::os::windows::process::CommandExt;

    // CREATE_NO_WINDOW
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn apply_suppress_console(_command: &mut Command) {}
