## ADDED Requirements

### Requirement: 预设和转码

导出 SHALL 默认以 −14 LUFS 双遍归一化至 MP3，支持文档列出的四档预设及原格式复制。

#### Scenario: 两档测量

- **WHEN** 以 −14 LUFS 和 −20 LUFS 导出同一曲目
- **THEN** 各自误差在 ±1 LU 内，两次测量相差约 6 LU

#### Scenario: 缺少 ffmpeg 和错扩展名

- **WHEN** 禁用 ffmpeg 或目标扩展名与兼容输出不一致
- **THEN** 允许的原格式复制成功且提示回退，非法的扩展名组合不写入错误格式

### Requirement: 批量与文件完整性

批量导出 MUST 复用一次 ffmpeg 探测，保留目标文件并为同名追加后缀。

#### Scenario: 并存文件

- **WHEN** 目标目录已有同名外部文件
- **THEN** 原文件保留，新文件按约定命名；不完整复制会清理残留
