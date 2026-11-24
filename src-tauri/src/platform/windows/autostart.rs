//! 开机自动启动：写入 HKCU 的 Run 项，不需要额外安装器。

use anyhow::{Context, Result};

/// 注册表值名，同时也是 Linux 桌面项与 macOS LaunchAgent 标签的基名。
const AUTOSTART_KEY: &str = "tfy-bgm";
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

pub fn set(enabled: bool) -> Result<()> {
    use windows_registry::CURRENT_USER;

    let exe = std::env::current_exe().context("无法定位应用可执行文件")?;
    let key = CURRENT_USER
        .create(RUN_KEY)
        .map_err(|error| anyhow::anyhow!("无法打开开机启动注册表项：{error}"))?;
    if enabled {
        let command = format!("\"{}\"", exe.display());
        key.set_string(AUTOSTART_KEY, command)
            .map_err(|error| anyhow::anyhow!("写入开机启动注册表项失败：{error}"))?;
    } else {
        // 值不存在时删除会返回错误，直接忽略。
        let _ = key.remove_value(AUTOSTART_KEY);
    }
    Ok(())
}
