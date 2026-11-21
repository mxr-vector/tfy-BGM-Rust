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
  /** 界面字体 id：system / harmony / misans / pingfang / yahei。 */
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
}

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

export interface ConfigReload {
  config: AppConfig;
  warnings: string[];
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

export interface WallpaperSelection {
  id: string;
  path: string | null;
  opacity: number;
  blur: number;
  fit: WallpaperFit;
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

/** 开放接口清单条目，与 Rust `api::endpoints()` 对应。 */
export interface ApiEndpoint {
  method: string;
  path: string;
  summary: string;
  description: string;
}