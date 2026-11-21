# AGENTS.md

## 分支定位（最重要）

本仓库是 `tfy-bgm` 项目的 **GPUI 分支**（Gitee 上的 `bgm-gpui` 分支），**不是主线**。

- 主线（Tauri 版）在 `https://gitee.com/yuanjie2001/tfy-bgm.git` 的 **`bgm-tauri2` 分支**上。
- 本分支用 **纯 Rust + GPUI** 重新实现客户端，功能范围**跟随主线**；主线新增或调整功能时，这里需要对齐。
- 不使用 WebView、Node 或任何前端构建链。视觉与交互对照主线对齐（当前基线为 `bgm-tauri2` 的 `63a0598`；
  两边历史都重建过，主线旧提交号 `203c641` 已失效，做能力同步时以 `origin/bgm-tauri2` 最新代码为准）。
- 本分支主要由 `@mojl` 维护；提交与合并到 `bgm-gpui` 的命令以其为准。

任何改动前先确认：**改的是 GPUI 实现，不要往回引入 Tauri/WebView 的东西**。

## 仓库结构

```
src/main.rs        Application 启动、窗口选项、快捷键绑定（bind_keys）
src/actions.rs     gpui::actions! 声明，按上下文区分曲库/输入框/对话框
src/view.rs        Workspace 的 Render：面板、菜单、悬浮播放器、节目单页、壁纸层（最大的文件）
src/library.rs     Library、筛选、曲库根目录解析、节目单/循环点持久化、导出设置
src/text_input.rs  自绘 TextField，IME/选区/剪贴板，EntityInputHandler
src/audio.rs       rodio 输出设备；整曲 PCM 解码，采样级 A/B 循环、倍速与音量
src/watch.rs       notify 文件监听曲库目录，防抖后触发增量扫描
src/icons.rs       内嵌 Lucide SVG 路径与着色
src/assets.rs      GPUI AssetSource，提供 icons/*.svg 与内置壁纸 bg/2.jpg
core/              从主线同步的 bgm-core：扫描/导入/配置/偏好/节目单/循环/响度导出
Cargo.toml         根包；bgm-core 以 path 依赖指向 core/
README.md          面向用户的说明，必须写清 GPUI 分支定位
```

`core/` 是从主线 `src-tauri/core` 同步过来的独立 crate。更新它时从主线复制对应文件，
并在提交信息中注明同步的主线提交号；不要在 `core/` 里做 GPUI 专属改动。

## 能力同步（主线 → GPUI）

主线在 `bgm-tauri2` 分支用 Tauri/TS 实现功能，本分支用 GPUI/Rust 对齐。同步套路：

1. `git fetch origin bgm-tauri2`，对比 `origin/bgm-tauri2:src-tauri/core/` 与本地 `core/`；
   core 是设备无关的，直接整目录复制，Cargo.toml 若有变化也照抄。
2. 读主线行为层：`src-tauri/src/state.rs`（状态机）、`commands.rs`（命令面），
   以及 `ui/src/` 下的 TS 行为（`stores/`、`composables/`）——播放侧逻辑（排程 tick、
   循环执行、跑马灯）在前端 TS 里，要按行为移植，不照搬 DOM 实现。
3. 数据落盘逻辑进 `src/library.rs` 或 `core/`，界面进 `src/view.rs`，文案与提示照主线。
4. 提交信息注明对齐的主线提交号（如 `feat: 对齐主线 b5cff69 的节目单能力`）。

## 构建与验证

需要 Rust 1.90+（edition 2024）。提交前应执行：

```bash
cargo check --all-targets   # 或 cargo check --offline；--all-targets 才会编译测试代码
cargo fmt --check
cargo test
```

依赖全部来自 crates.io 与本地 `core/`。`cargo check` 不编译 `#[cfg(test)]`，改了测试辅助
函数或数据结构字段后务必跑 `--all-targets`。

## 编码约定

- 注释密度、命名和写法跟随现有 `src/` 代码：中文模块级 `//!` 注释、简洁行内注释、不写空泛注释。
- 视图代码集中在 `view.rs` 的 `impl Workspace`：渲染逻辑用 GPUI 的 `Render`/`IntoElement` 风格，
  状态放进 `Workspace` 的字段，不要在渲染函数里做文件 IO 或耗时操作。
- 快捷键在 `main.rs::bind_keys` 统一绑定，用 context（`Library && !TextInput && !Dialog` 等）
  区分作用域，避免吃掉输入框里的按键。
- 曲库与配置逻辑放在 `src/library.rs` 或 `core/`，并在 `core/` 保持与主线一致。
- 图标用编译期内嵌 SVG（`icons.rs` + `assets.rs`），不要引入 emoji 或外部前端资源。
- 播放走 rodio；倍速改变音高是已知差异，不要为了"修"它而引入浏览器依赖。
  A/B 循环靠 `audio.rs` 的整曲 PCM 音源在采样级回绕，不要改回流式 Decoder。
- 已知的能力缺口（主线有、本分支暂无）：mp4 动态壁纸与毛玻璃模糊（GPUI 无视频解码/背景模糊）、
  批量多选操作、诊断面板。补齐时先在 README「已知差异」里更新状态。

## 提交与推送

- 提交信息用中文，格式参考主线：`feat:`、`refactor:`、`fix:` 前缀加简短描述。
- 推送目标为 Gitee `https://gitee.com/yuanjie2001/tfy-bgm.git` 的 `bgm-gpui` 分支。
- 该分支已用孤立提交重建：历史从 GPUI 实现开始，**不含主线/Tauri 的历史**。
  覆盖推送后，不要在非孤立分支上继续开发，避免把 Tauri 历史混回来。
- 推送前确认 `README.md` 与 `AGENTS.md` 的分支定位描述仍然准确。

## 不做的事

- 不引入 Tauri、WebView、Node、npm 或前端构建工具。
- 不把 `bgm/` 里的 mp3 打进包或提交（版权归原权利人，不在 Apache-2.0 内）。
- 不在 `core/` 里保留指向主线仓库外部的 path 依赖；它必须随本仓库一起构建。
