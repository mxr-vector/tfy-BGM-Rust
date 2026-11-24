//! 开机自动启动：写入 XDG autostart 桌面项，桌面环境登录时自行拉起。

use std::path::PathBuf;

use anyhow::{Context, Result, bail};

/// 桌面项文件名，同时也是 Windows 注册表值名与 macOS LaunchAgent 标签的基名。
const AUTOSTART_KEY: &str = "tfy-bgm";

pub fn set(enabled: bool) -> Result<()> {
    let exe = std::env::current_exe().context("无法定位应用可执行文件")?;
    let path = autostart_dir()
        .context("无法定位 autostart 目录：缺少 XDG_CONFIG_HOME 与 HOME")?
        .join(format!("{AUTOSTART_KEY}.desktop"));
    if !enabled {
        return match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => bail!("关闭开机自动启动失败：{error}"),
        };
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).context("创建 autostart 目录失败")?;
    }
    let content = format!(
        "[Desktop Entry]\nType=Application\nName=听风吟 · BGM\nExec=\"{}\"\nTerminal=false\nX-GNOME-Autostart-enabled=true\n",
        exe.display()
    );
    std::fs::write(&path, content).context("写入 autostart 桌面项失败")
}

fn autostart_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .map(|base| base.join("autostart"))
}
