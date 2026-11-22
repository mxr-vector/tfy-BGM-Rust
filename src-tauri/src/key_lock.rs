//! 按曲目 key 的进程内互斥。
//!
//! 字幕生成与人声分离共用 `<配置目录>/separations/<key-hash>/`：
//! `vocals.wav`、`accompaniment.wav` 与 `subtitles.srt` 都是固定文件名，且分离过程
//! 先写 `*.wav.part` 再替换正式文件。同一曲目并发执行两条链路会互相删除 / 覆盖
//! `.part`，可能产出损坏音频或交错字幕。
//!
//! 串行任务队列只约束 HTTP 任务，UI 的「生成字幕」命令走独立路径，因此这里再做
//! 一层与入口无关的 key 互斥：抢不到直接失败并给出可展示的原因，不阻塞等待。
//!
//! 锁只在进程内有效：worker 子进程不参与；主进程退出时随进程一起释放。

use std::collections::HashSet;
use std::sync::LazyLock;

use anyhow::{Result, anyhow};
use parking_lot::Mutex;

/// 抢不到 key 时统一使用的提示，HTTP 层直接复用为 409 文案。
pub const BUSY_MESSAGE: &str = "该曲目正在生成字幕或分离人声，请等待当前任务结束";

static ACTIVE: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

/// 占用凭证：`Drop` 时释放 key，正常返回与错误 / panic 展开都能释放。
pub struct KeyGuard {
    key: String,
}

impl Drop for KeyGuard {
    fn drop(&mut self) {
        ACTIVE.lock().remove(&self.key);
    }
}

/// 尝试占用曲目；已被其他链路占用时返回 `None`，不等待。
pub fn try_claim(key: &str) -> Option<KeyGuard> {
    let mut active = ACTIVE.lock();
    if !active.insert(key.to_owned()) {
        return None;
    }
    Some(KeyGuard {
        key: key.to_owned(),
    })
}

/// 占用曲目；失败时返回可直接展示给用户的错误。
pub fn claim(key: &str) -> Result<KeyGuard> {
    try_claim(key).ok_or_else(|| anyhow!(BUSY_MESSAGE))
}

/// 该曲目当前是否已有生成 / 分离在跑；供 HTTP 提交前快速返回 409。
pub fn is_busy(key: &str) -> bool {
    ACTIVE.lock().contains(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claim_is_exclusive_per_key_and_released_on_drop() {
        // 测试并行执行，用进程 id + 专用前缀避免相互影响。
        let key = format!("key-lock-test/{}/a", std::process::id());
        let other = format!("{key}-other");

        let guard = try_claim(&key).expect("首次占用应成功");
        assert!(is_busy(&key));
        assert!(try_claim(&key).is_none(), "同一 key 不能重复占用");
        assert!(claim(&key).is_err());
        assert!(try_claim(&other).is_some(), "不同 key 互不影响");

        drop(guard);
        assert!(!is_busy(&key));
        assert!(try_claim(&key).is_some(), "释放后应能再次占用");
    }

    #[test]
    fn busy_message_is_user_facing() {
        assert!(BUSY_MESSAGE.contains("该曲目"));
        assert!(BUSY_MESSAGE.contains("等待"));
    }
}
