/** 与 Rust `bgm-core` / Tauri 命令序列化结果一一对应的前端契约。 */

export interface Song {
  key: string;
  title: string;
  fileName: string;
  category: string;
  path: string;
  /** ID3 标签；缺失时后端省略字段。 */
  artist?: string;
  album?: string;
  genre?: string;
  durationMs?: number;
}

export interface CategorySummary {
  id: string;
  nameZh: string;
  nameEn: string;
  labelEn: string;
  color: string;
  icon: string;
  title: string;
  description: string;
  count: number;
}

export type LibrarySource = 'bundled' | 'external' | 'custom' | 'env';

export interface LibrarySnapshot {
  root: string;
  source: LibrarySource;
  songs: Song[];
  categories: CategorySummary[];
  warnings: string[];
  scannedAtMs: number;
}

export type PlayMode = 'sequence' | 'repeat-one' | 'shuffle';
export type SortBy = 'default' | 'name';
export type WallpaperFit = 'cover' | 'contain';
export type WallpaperKind = 'image' | 'video';

/** 当前运行平台；平台特有配置项据此显示。 */
export type AppPlatform = 'windows' | 'linux' | 'macos' | 'other';

/** 点击标题栏关闭按钮的行为；`ask` 表示每次弹出选择框。 */
export type CloseAction = 'ask' | 'quit' | 'tray';

/** 浏览器跨域策略：仅本机 / 任意来源 / 本机 + 自定义来源。 */
export type CorsMode = 'local' | 'any' | 'custom';

export interface CorsConfig {
  mode: CorsMode;
  /** 仅在 `mode = 'custom'` 时参与判断；`*` 等价于放行全部来源。 */
  origins: string[];
}

/** `system_fonts` 命令返回的系统字体。 */
export interface SystemFont {
  /** CSS `font-family` 使用的族名。 */
  family: string;
  /** 展示名，优先简体中文（如「微软雅黑」）。 */
  label: string;
  /** 是否含中文字形。 */
  cjk: boolean;
}

/** 一路输出设备路由；`deviceId` 为 `default` 时代表系统默认输出。 */
export interface OutputRoute {
  deviceId: string;
  label: string;
  /** 这一路的独立音量（0~1），叠加在播放器主音量之上。 */
  volume: number;
  muted: boolean;
  enabled: boolean;
}

/** `enumerateDevices()` 暴露的输出设备。 */
export interface OutputDevice {
  deviceId: string;
  label: string;
  kind: string;
}

export interface Prefs {
  version: number;
  libraryDir: string | null;
  favorites: string[];
  volume: number;
  muted: boolean;
  playbackRate: number;
  playMode: PlayMode;
  sortBy: SortBy;
  /** 切歌交叉淡入淡出时长（秒），0 为硬切。 */
  crossfadeSeconds: number;
  outputRoutes: OutputRoute[];
  loudnessNormalization: boolean;
  exportScene: string;
  wallpaper: string;
  customWallpaper: string | null;
  wallpaperOpacity: number;
  wallpaperBlur: number;
  wallpaperFit: WallpaperFit;
  /** 界面字体：`system` 或系统字体族名（见 `system_fonts` 命令）。 */
  uiFont: string;
  autostart: boolean;
  minimizeToTray: boolean;
  /** 关闭后需要重启应用才会切换 WebView 渲染后端。 */
  gpuAcceleration: boolean;
  autoplayOnStart: boolean;
  preventSleep: boolean;
  /** 点击标题栏关闭按钮时的行为。 */
  closeAction: CloseAction;
  /** Windows：系统媒体传输控制。 */
  smtc: boolean;
  /** Windows：系统空间音效。 */
  spatialAudio: boolean;
  /** Windows：系统音频增强。 */
  audioEnhancement: boolean;
  /** Linux：MPRIS2（仅 Wayland 会话生效）。 */
  mpris2: boolean;
  /** ASR 模型目录；`null` 表示系统「下载」目录下的 `bgm-models`。 */
  modelsDir: string | null;
  /** ASR 模型下载地址；空字符串表示内置的 0.6B-int8 地址。 */
  asrDownloadUrl: string;
  /** 上次加载成功的模型 id（相对模型目录）；由后端维护，启动时自动恢复。 */
  lastAsrModel: string | null;
  /** 上次确认的人声分离模型 id；每次生成前仍会再次确认。 */
  separationModel: string | null;
  /** 桌面歌词开关；由后端维护，模型可用时随启动恢复。 */
  desktopLyricsEnabled: boolean;
  /** 字幕预览偏好格式。 */
  subtitleFormat: SubtitleFormat;
}

export type SubtitleFormat = 'srt' | 'lrc';

export interface BrandInfo {
  title: string;
  subtitle: string;
  windowTitle: string;
}

export interface PlayerConfig {
  rates: number[];
  volumeStep: number;
  seekStepSeconds: number;
  restartThresholdSeconds: number;
  maxAutoSkips: number;
  loadTimeoutMs: number;
}

export interface HeroCopy {
  title: string;
  labelEn: string;
  description: string;
  resultsTitle: string;
}

export interface AppConfig {
  brand: BrandInfo;
  player: PlayerConfig;
  ui: {
    all: HeroCopy;
    favorites: HeroCopy;
  };
}

export interface Bootstrap {
  prefs: Prefs;
  library: LibrarySnapshot;
  config: AppConfig;
  schedule: Schedule;
  loops: LoopBook;
  platform: AppPlatform;
}

export interface ScheduleSong {
  /** 曲库曲目 key。 */
  key: string;
  /** 本地时间 HH:MM，落在所属节目范围内。 */
  start: string;
  /** 本地时间 HH:MM，晚于 start。 */
  end: string;
}

export interface ScheduleEntry {
  id: string;
  name: string;
  /** 本地时间 HH:MM。 */
  start: string;
  /** 本地时间 HH:MM，晚于 start。 */
  end: string;
  /** 按播放顺序排列的歌曲。 */
  songs: ScheduleSong[];
  volume: number | null;
  enabled: boolean;
}

export interface Schedule {
  version: number;
  entries: ScheduleEntry[];
}

export interface LoopPoint {
  start: number;
  end: number;
  enabled: boolean;
}

export interface LoopBook {
  version: number;
  points: Record<string, LoopPoint>;
}

/** 文件监听推送的增量快照。 */
export interface LibraryChange {
  library: LibrarySnapshot;
  added: string[];
  removed: string[];
  moved: KeyChange[];
}

export interface BatchReport {
  total: number;
  succeeded: number;
  skipped: number;
  failed: number;
  messages: string[];
  failures?: string[];
  notice?: string | null;
}

export interface KeyChange {
  from: string;
  to: string;
}

export interface LibraryMutation {
  library: LibrarySnapshot;
  changes: KeyChange[];
  removed: string[];
  report: BatchReport;
}

export interface CategoryOutcome {
  library: LibrarySnapshot;
  categoryId: string;
}

export interface ExportResult {
  path: string;
  bytes: number;
  normalized: boolean;
  warning: string | null;
}

export interface ExportBatchResult {
  dir: string;
  report: BatchReport;
}

/** 导出响度场景预设，与 `bgm-core` 的 `EXPORT_PRESETS` 对应。 */
export interface ExportPreset {
  id: string;
  label: string;
  description: string;
  targetLufs: number;
  truePeak: number;
  loudnessRange: number;
}

export interface ExportInfo {
  ffmpeg: string | null;
  presets: ExportPreset[];
}

export interface WallpaperAsset {
  id: string;
  label: string;
  kind: WallpaperKind;
  path: string;
  thumb: string;
}

export interface WallpaperPick {
  path: string;
  kind: WallpaperKind;
  label: string;
}



/** 本机开放接口状态。 */
export interface ApiStatus {
  enabled: boolean;
  running: boolean;
  ready: boolean;
  port: number;
  baseUrl: string;
  token: string;
  cors: CorsConfig;
  error: string | null;
}

/** 开放接口清单条目，与 Rust `api::ApiEndpoint` 对应。 */
export interface ApiEndpoint {
  /** 所属业务分类 id，见 `ApiCategory.id`。 */
  category: string;
  method: string;
  path: string;
  summary: string;
  description: string;
}

/** 接口的业务分类；顺序即界面展示顺序。 */
export interface ApiCategory {
  id: string;
  label: string;
  description: string;
}

/** 接口目录：分类 + 全部接口，与 Rust `api::ApiCatalog` 对应。 */
export interface ApiCatalog {
  categories: ApiCategory[];
  endpoints: ApiEndpoint[];
}

/** 扫描到的一个 ASR 模型；同一目录内的 `.onnx` 归为一条。 */
export interface AsrModel {
  /** 相对模型目录的稳定标识。 */
  id: string;
  name: string;
  /** 模型所在目录的绝对路径。 */
  path: string;
  /** 目录内的 `.onnx` 文件名。 */
  files: string[];
  sizeBytes: number;
  isDefault: boolean;
  /** Qwen3 四件套是否齐全；不齐全时不能加载。 */
  loadable: boolean;
  missing: string[];
}

/** ASR 默认模型（0.6B-int8）的安装状态。 */
export interface DefaultModelStatus {
  name: string;
  path: string;
  installed: boolean;
  missing: string[];
  sizeBytes: number;
  /** 完整下载后的体积。 */
  expectedBytes: number;
}

/** 已通过 sherpa-onnx 加载到内存的 ASR 模型。 */
export interface LoadedAsrModel {
  name: string;
  path: string;
  /** 推理引擎，固定为 `sherpa-onnx`。 */
  engine: string;
  threads: number;
}

/** 「模型」分组快照。 */
/** 模型加载阶段：自动加载与手动加载共用。 */
export type ModelLoadPhase = 'idle' | 'loading' | 'success' | 'failed';

/** 最近一次模型加载状态；`error` 为失败原因，供重试入口展示。 */
export interface ModelLoadStatus {
  phase: ModelLoadPhase;
  path: string | null;
  error: string | null;
}

export interface ModelsInfo {
  root: string;
  /** 是否使用默认目录（系统「下载」目录下的 `bgm-models/`）。 */
  isDefaultRoot: boolean;
  defaultDownloadUrl: string;
  /** 当前生效的下载地址，自定义为空时等于默认地址。 */
  downloadUrl: string;
  defaultModel: DefaultModelStatus;
  models: AsrModel[];
  loaded: LoadedAsrModel | null;
  /** 最近一次模型加载状态；桌面歌词开关据此判断是否可用。 */
  loadStatus: ModelLoadStatus;
  downloading: boolean;
  warnings: string[];
}

/** 模型下载进度事件。 */
export interface ModelDownloadProgress {
  file: string;
  index: number;
  totalFiles: number;
  received: number;
  fileBytes: number;
  overallReceived: number;
  overallBytes: number;
  done: boolean;
  error: string | null;
}

// ---------------- 字幕生成 ----------------

/** 一条带起止时间的字幕。 */
export interface SubtitleSegment {
  start: number;
  end: number;
  text: string;
}

/** 分离模型：`target` 为模型主输出轨道，`instrumental` 时人声取混合减乐器。 */
export interface SeparationModelInfo {
  id: string;
  name: string;
  engine: string;
  target: 'vocals' | 'instrumental';
  /** false 时模型主输出角色不确定（自定义 UVR），生成前需要用户确认。 */
  targetKnown: boolean;
  description: string;
  available: boolean;
  detail: string;
}

/** 分离模型目录：人声模型可选，伴奏模型固定。 */
export interface SeparationCatalog {
  vocals: SeparationModelInfo[];
  accompaniment: SeparationModelInfo;
}

/** 一次人声分离的交付信息；歌词识别只使用 `vocalsPath`。 */
export interface SeparationInfo {
  modelId: string;
  modelName: string;
  engine: string;
  target: 'vocals' | 'instrumental';
  /** 固定伴奏模型的显示名。 */
  accompanimentModelName: string;
  vocalsPath: string;
  accompanimentPath: string;
  accompanimentKind: string;
  sampleRate: number;
}

export type SubtitleStage = 'separating' | 'normalizing' | 'transcribing' | 'done' | 'error';

/** `subtitle-progress` 事件负载。 */
export interface SubtitleProgress {
  stage: SubtitleStage;
  message: string;
  current: number;
  total: number;
  error: string | null;
}

/** `generate_subtitles` 的完整结果，含可预览的 SRT / LRC 文本。 */
export interface SubtitleResult {
  key: string;
  title: string;
  sourcePath: string;
  sourceFormat: string;
  /** 本次分离模型与本地人声 / 伴奏文件。 */
  separation: SeparationInfo;
  duration: number;
  segments: SubtitleSegment[];
  srt: string;
  lrc: string;
  savedPath: string;
  /** 自动落盘的 SRT / LRC 文件路径。 */
  srtPath: string;
  lrcPath: string;
}

export interface SubtitleExportResult {
  path: string;
  bytes: number;
  format: string;
}

/** 持久化在应用配置目录的字幕；桌面歌词据此读取歌词行。 */
export interface StoredSubtitle {
  key: string;
  title: string;
  segments: SubtitleSegment[];
  createdAtMs: number;
  /** 最近一次手动编辑的时间（毫秒）；未编辑过的旧记录没有该字段。 */
  updatedAtMs?: number;
  /** 源音频文件名；旧字幕文件缺失，展示时用曲目 key 末段兜底。 */
  fileName?: string;
  /** 源音频时长（毫秒）；旧字幕文件缺失。 */
  durationMs?: number;
  /** 后端按统一规则格式化好的文本，预览与导出共用。 */
  srt: string;
  lrc: string;
  /** 生成时使用的人声分离模型显示名（不是偏好里的模型 id）；旧记录没有该字段。 */
  separationModelName?: string;
  /** 生成时使用的固定伴奏模型显示名；旧记录没有该字段。 */
  accompanimentModelName?: string;
  /** 分离后的人声文件绝对路径；歌词识别只使用该文件。 */
  vocalsPath?: string;
  /** 分离后的伴奏文件绝对路径。 */
  accompanimentPath?: string;
  /** `伴奏` 或 `乐器`；新版固定为「伴奏」。 */
  accompanimentKind?: string;
  /** 自动落盘的 SRT / LRC 文件路径；旧记录没有该字段。 */
  srtPath?: string;
  lrcPath?: string;
}

// ---------------- 桌面歌词 ----------------

export interface LyricLine {
  start: number;
  end: number;
  text: string;
}

/** 主窗口推给后端、再广播给歌词窗口的播放 / 歌词快照。 */
export interface LyricsSnapshot {
  trackKey: string | null;
  title: string;
  playing: boolean;
  position: number;
  /** 播放倍速；歌词窗口在两次快照之间按同一速率本地推进。 */
  rate: number;
  lines: LyricLine[];
  hasLyrics: boolean;
}

export interface LyricsBootstrap {
  enabled: boolean;
  snapshot: LyricsSnapshot;
}

/** 桌面歌词窗口的显示样式；保存在歌词窗口自身的 localStorage 中。 */
export interface LyricsStyle {
  /** 字体族名（来自 `system_fonts`）；空串表示使用默认字体栈。 */
  fontFamily: string;
  /** 当前行字号（px）。 */
  fontSize: number;
  /** 高亮色（#rrggbb）。 */
  highlight: string;
  /** 背景不透明度（0–90，百分比）。 */
  background: number;
  /** 窗口宽度（逻辑像素）。 */
  width: number;
  /** 窗口高度（逻辑像素）。 */
  height: number;
  /** 锁定：只显示歌词、隐藏边框/控件并默认鼠标穿透。 */
  locked: boolean;
  /**
   * 已唱标记提前量（秒，可选 0 / 1 / 2）。
   *
   * 渲染时间轴整体前移这么多秒，使逐词「已唱」标记与歌词行切换同步提前，
   * 抵消 ASR 字幕起点偏晚、句间空白被计入行时长以及跨窗口同步带来的视觉滞后。
   * 0 表示严格按原始时间轴渲染。
   */
  leadSeconds: number;
}

/** `desktop-lyrics-hover` 事件负载：锁定态下鼠标是否进入歌词热区。 */
export interface LyricsHover {
  hovered: boolean;
}

/** 设置悬浮窗只能请求、必须由歌词窗口执行的动作（锁定需要歌词热区）。 */
export type LyricsCommand =
  | { action: 'reset' }
  | { action: 'lock'; locked: boolean };

/** `desktop-lyrics-settings` 事件负载：显示设置悬浮窗是否打开。 */
export interface LyricsSettingsState {
  open: boolean;
}

/** 锁定态歌词热区（窗口内逻辑像素），用于 Rust 侧光标轮询命中检测。 */
export interface LyricsHotZone {
  x: number;
  y: number;
  width: number;
  height: number;
}

/** `desktop-lyrics-size` 事件负载：窗口当前逻辑尺寸。 */
export interface LyricsWindowSize {
  width: number;
  height: number;
}
