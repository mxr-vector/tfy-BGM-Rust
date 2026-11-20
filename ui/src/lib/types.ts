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

export interface Prefs {
  version: number;
  libraryDir: string | null;
  favorites: string[];
  volume: number;
  muted: boolean;
  playbackRate: number;
  playMode: PlayMode;
  sortBy: SortBy;
  loudnessNormalization: boolean;
  exportScene: string;
  wallpaper: string;
  customWallpaper: string | null;
  wallpaperOpacity: number;
  wallpaperBlur: number;
  wallpaperFit: WallpaperFit;
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
}

export interface ScheduleEntry {
  id: string;
  name: string;
  /** 本地时间 HH:MM。 */
  start: string;
  /** ISO 周序：1 = 周一 … 7 = 周日。 */
  days: number[];
  category: string | null;
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