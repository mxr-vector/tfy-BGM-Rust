# AGENTS.md

## 分支定位（最重要）

本仓库是 `tfy-bgm` 项目的 **GPUI 分支**（Gitee 上的 `bgm-gpui` 分支），**不是主线**。

- 主线（Tauri 版）在 `https://gitee.com/yuanjie2001/tfy-bgm.git` 的其他分支上。
- 本分支用 **纯 Rust + GPUI** 重新实现客户端，功能范围**跟随主线**；主线新增或调整功能时，这里需要对齐。
- 不使用 WebView、Node 或任何前端构建链。视觉与交互对照主线对齐（当前基线为 `203c641`）。
- 本分支主要由 `@mojl` 维护；提交与合并到 `bgm-gpui` 的命令以其为准。

任何改动前先确认：**改的是 GPUI 实现，不要往回引入 Tauri/WebView 的东西**。

## 仓库结构

```
src/main.rs        Application 启动、窗口选项、快捷键绑定（bind_keys）
src/actions.rs     gpui::actions! 声明，按上下文区分曲库/输入框/对话框
src/view.rs        Workspace 的 Render，所有面板、菜单、悬浮播放器（最大的文件）
src/library.rs     Library/Mutation、筛选、曲库根目录解析、配置读写
src/text_input.rs  自绘 TextField，IME/选区/剪贴板，EntityInputHandler
src/audio.rs       rodio 输出设备、播放队列、倍速与音量
src/icons.rs       内嵌 Lucide SVG 路径与着色
src/assets.rs      GPUI AssetSource，提供 icons/*.svg
core/              从主线同步的 bgm-core：扫描/导入/配置/偏好/导出校验
Cargo.toml         根包；bgm-core 以 path 依赖指向 core/
README.md          面向用户的说明，必须写清 GPUI 分支定位
```

`core/` 是从主线 `src-tauri/core` 同步过来的独立 crate。更新它时从主线复制对应文件，
并在提交信息中注明同步的主线提交号；不要在 `core/` 里做 GPUI 专属改动。

## 构建与验证

需要 Rust 1.90+（edition 2024）。提交前应执行：

```bash
cargo check      # 或 cargo check --offline
cargo fmt --check
```

`cargo check` 已验证可离线通过；依赖全部来自 crates.io 与本地 `core/`，无网络构建步骤。

## 编码约定

- 注释密度、命名和写法跟随现有 `src/` 代码：中文模块级 `//!` 注释、简洁行内注释、不写空泛注释。
- 视图代码集中在 `view.rs` 的 `impl Workspace`：渲染逻辑用 GPUI 的 `Render`/`IntoElement` 风格，
  状态放进 `Workspace` 的字段，不要在渲染函数里做文件 IO 或耗时操作。
- 快捷键在 `main.rs::bind_keys` 统一绑定，用 context（`Library && !TextInput && !Dialog` 等）
  区分作用域，避免吃掉输入框里的按键。
- 曲库与配置逻辑放在 `src/library.rs` 或 `core/`，并在 `core/` 保持与主线一致。
- 图标用编译期内嵌 SVG（`icons.rs` + `assets.rs`），不要引入 emoji 或外部前端资源。
- 播放走 rodio；倍速改变音高是已知差异，不要为了"修"它而引入浏览器依赖。

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
