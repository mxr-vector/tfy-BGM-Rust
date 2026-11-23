# source-separation Specification

## Purpose

从本地音频分离人声与伴奏，验证模型及交付物，并按曲目稳定键保存结果。结果保存为可独立使用的两条音轨，字幕识别复用其中的人声。

## Requirements

### Requirement: 双引擎与输入确认

分离 SHALL 使用确认的人声模型和固定的伴奏模型，伴奏模型与角色不明的自定义模型按主线校验。

#### Scenario: 执行双引擎分离

- **WHEN** 对支持的音频启动已确认的分离
- **THEN** 生成有效的 vocals.wav、accompaniment.wav 与 separation.json

### Requirement: 失败和重生成

worker 退出、产物校验及重生成 SHALL 遵循主线错误和文件保留语义。

#### Scenario: 重生成 worker 失败

- **WHEN** 存在有效旧产物时 worker 退出或产物校验失败
- **THEN** 保留旧交付物、清理本次 .part 并返回错误
