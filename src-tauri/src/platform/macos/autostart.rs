//! 开机自动启动：写入用户级 LaunchAgent，登录时由 launchd 拉起。

use std::path::PathBuf;

use anyhow::{Context, Result, bail};

const LABEL: &str = "com.yuanjie.tfybgm";

pub fn set(enabled: bool) -> Result<()> {
    let exe = std::env::current_exe().context("无法定位应用可执行文件")?;
    let path = launch_agent_path().context("无法定位 LaunchAgents 目录")?;
    if !enabled {
        return match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => bail!("关闭开机自动启动失败：{error}"),
        };
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).context("创建 LaunchAgents 目录失败")?;
    }
    let content = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n  <key>Label</key>\n  <string>{LABEL}</string>\n  <key>ProgramArguments</key>\n  <array>\n    <string>{}</string>\n  </array>\n  <key>RunAtLoad</key>\n  <true/>\n</dict>\n</plist>\n",
        exe.display()
    );
    std::fs::write(&path, content).context("写入 LaunchAgent 文件失败")
}

fn launch_agent_path() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join("Library/LaunchAgents"))
        .map(|dir| dir.join(format!("{LABEL}.plist")))
}
