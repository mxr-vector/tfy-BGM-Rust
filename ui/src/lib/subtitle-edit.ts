/**
 * 字幕编辑器的纯函数工具：分段 ↔ SRT / LRC 文本互转、原文解析与输入校验。
 *
 * 这些函数只服务于「编辑草稿」的即时同步与前端校验；真正的落盘解析与规范化
 * 在 Rust `subtitles.rs`（parse_srt / parse_lrc / normalize_segments）完成。
 * 保存成功后前端一律以后端返回的快照为准，避免两套规则漂移。
 */

import type { SubtitleFormat, SubtitleSegment } from './types';

const MAX_SEGMENTS = 5_000;
const MAX_TEXT_CHARS = 2_000;

function pad(value: number, width = 2): string {
  return String(Math.floor(value)).padStart(width, '0');
}

/** SRT 时间：HH:MM:SS,mmm（与后端 `srt_time` 一致）。 */
export function srtTime(seconds: number): string {
  const safe = Number.isFinite(seconds) ? seconds : 0;
  const millis = Math.round(Math.max(0, safe) * 1000);
  const hours = Math.floor(millis / 3_600_000);
  const minutes = Math.floor(millis / 60_000) % 60;
  const secs = Math.floor(millis / 1000) % 60;
  const ms = millis % 1000;
  return `${pad(hours)}:${pad(minutes)}:${pad(secs)},${pad(ms, 3)}`;
}

/** LRC 时间：MM:SS.cc（与后端 `lrc_time` 一致）。 */
export function lrcTime(seconds: number): string {
  const safe = Number.isFinite(seconds) ? seconds : 0;
  const centis = Math.round(Math.max(0, safe) * 100);
  const minutes = Math.floor(centis / 6000);
  const secs = Math.floor(centis / 100) % 60;
  const cs = centis % 100;
  return `${pad(minutes)}:${pad(secs)}.${pad(cs)}`;
}

function singleLine(text: string): string {
  return text.replace(/[\r\n]+/g, ' ').trim();
}

/** 按当前格式从分段草稿生成原文（切换编辑模式 / 格式时同步文本框）。 */
export function formatSegments(
  format: SubtitleFormat,
  title: string,
  segments: SubtitleSegment[],
): string {
  if (format === 'srt') {
    return segments
      .map(
        (segment, index) =>
          `${index + 1}\n${srtTime(segment.start)} --> ${srtTime(segment.end)}\n${singleLine(segment.text)}\n`,
      )
      .join('\n');
  }
  let out = `[ar:]\n[ti:${singleLine(title)}]\n`;
  for (const segment of segments) {
    out += `[${lrcTime(segment.start)}]${singleLine(segment.text)}\n`;
  }
  return out;
}

export type SubtitleParseResult =
  | { ok: true; segments: SubtitleSegment[] }
  | { ok: false; error: string };

/** 校验分段草稿；返回第一条错误说明，全部合法时返回 `null`。 */
export function validateSegments(segments: SubtitleSegment[]): string | null {
  if (!Array.isArray(segments) || segments.length === 0) return '至少需要保留一条字幕';
  if (segments.length > MAX_SEGMENTS) return `字幕条数过多（最多 ${MAX_SEGMENTS} 条）`;
  for (let index = 0; index < segments.length; index += 1) {
    const segment = segments[index];
    const label = index + 1;
    if (
      typeof segment.start !== 'number' ||
      typeof segment.end !== 'number' ||
      !Number.isFinite(segment.start) ||
      !Number.isFinite(segment.end)
    ) {
      return `第 ${label} 条的开始 / 结束时间不是有效数字`;
    }
    if (segment.start < 0 || segment.end < 0) return `第 ${label} 条的时间不能为负数`;
    if (segment.end <= segment.start) return `第 ${label} 条的结束时间必须大于开始时间`;
    if (typeof segment.text !== 'string') return `第 ${label} 条的文本无效`;
    // 与后端一致：换行折叠后按字符数（码点）校验长度，超限报错而非截断。
    if ([...singleLine(segment.text)].length > MAX_TEXT_CHARS) {
      return `第 ${label} 条文本过长（最多 ${MAX_TEXT_CHARS} 字）`;
    }
  }
  return null;
}

/** 把编辑中的原文解析为分段；错误以中文说明返回，供编辑器即时提示。 */
export function parseSubtitleText(
  text: string,
  format: SubtitleFormat,
  durationSecs?: number,
): SubtitleParseResult {
  const parsed = format === 'srt' ? parseSrt(text) : parseLrc(text, durationSecs);
  if (typeof parsed === 'string') return { ok: false, error: parsed };
  const error = validateSegments(parsed);
  if (error) return { ok: false, error };
  const segments = parsed
    .map((segment) => ({
      start: segment.start,
      end: segment.end,
      text: singleLine(segment.text),
    }))
    .sort((a, b) => a.start - b.start || a.end - b.end);
  return { ok: true, segments };
}

function parseSrt(text: string): SubtitleSegment[] | string {
  const lines = text.replace(/\r\n/g, '\n').replace(/\r/g, '\n').split('\n');
  const segments: SubtitleSegment[] = [];
  let current: { start: number; end: number; lines: string[] } | null = null;
  const flush = (): void => {
    if (!current) return;
    segments.push({
      start: current.start,
      end: current.end,
      text: current.lines.join(' ').trim(),
    });
    current = null;
  };
  for (let index = 0; index < lines.length; index += 1) {
    const line = lines[index].trim();
    if (!line) {
      flush();
      continue;
    }
    const range = parseSrtRange(line);
    if (range) {
      flush();
      current = { start: range[0], end: range[1], lines: [] };
      continue;
    }
    // 含 `-->` 却解析不出时间轴：报错而不是并入上一句，避免静默改坏内容。
    if (line.includes('-->')) {
      return `SRT 第 ${index + 1} 行的时间轴无法解析：${line}`;
    }
    if (current) {
      current.lines.push(line);
      continue;
    }
    if (/^\d+$/.test(line)) continue;
    return `SRT 第 ${index + 1} 行既不是时间轴也不是序号：${line}`;
  }
  flush();
  return segments;
}

function parseSrtRange(line: string): [number, number] | null {
  const parts = line.split('-->');
  if (parts.length !== 2) return null;
  const start = parseSrtTime(parts[0]);
  const end = parseSrtTime(parts[1]);
  return start === null || end === null ? null : [start, end];
}

function parseSrtTime(raw: string): number | null {
  const parts = raw.trim().replace(/,/g, '.').split(':');
  if (parts.some((part) => part.trim() === '')) return null;
  const numbers = parts.map(Number);
  if (numbers.some((value) => !Number.isFinite(value))) return null;
  let value: number;
  if (numbers.length === 3) value = numbers[0] * 3600 + numbers[1] * 60 + numbers[2];
  else if (numbers.length === 2) value = numbers[0] * 60 + numbers[1];
  else if (numbers.length === 1) value = numbers[0];
  else return null;
  return value >= 0 ? value : null;
}

/** LRC 元数据标签键（小写）：值不是时间，解析失败时按元数据忽略，与后端一致。 */
const LRC_METADATA_KEYS = new Set([
  'ar',
  'al',
  'ti',
  'au',
  'by',
  'offset',
  're',
  've',
  'length',
  'tool',
  'kana',
  'language',
]);

function isLrcMetadataTag(tag: string): boolean {
  const key = tag.split(':')[0]?.trim().toLowerCase() ?? '';
  return LRC_METADATA_KEYS.has(key);
}

/**
 * LRC 只有起点没有终点：每条的 end 取下一条起点，末条取音频时长
 * （可得且晚于起点时），否则取起点后 5 秒 —— 与后端 `parse_lrc` 一致。
 */
function parseLrc(text: string, durationSecs?: number): SubtitleSegment[] | string {
  const lines = text.replace(/\r\n/g, '\n').replace(/\r/g, '\n').split('\n');
  const timed: Array<{ start: number; text: string }> = [];
  for (let index = 0; index < lines.length; index += 1) {
    let rest = lines[index].trim();
    if (!rest) continue;
    let tags = 0;
    const times: number[] = [];
    // 只解析行首连续的时间标签；出现时间标签之后的 `[...]`（如 `[笑]`）属于正文。
    while (rest.startsWith('[')) {
      const close = rest.indexOf(']');
      if (close < 0) break;
      const tag = rest.slice(1, close);
      const seconds = parseLrcTime(tag);
      if (seconds !== null) {
        times.push(seconds);
        rest = rest.slice(close + 1).trimStart();
        tags += 1;
        continue;
      }
      if (times.length === 0 && isLrcMetadataTag(tag)) {
        rest = rest.slice(close + 1).trimStart();
        tags += 1;
        continue;
      }
      // 行首出现既非时间也非白名单元数据的标签：报错，避免静默丢掉整行歌词。
      if (times.length === 0) return `LRC 第 ${index + 1} 行的标签无法识别：${tag}`;
      break;
    }
    if (times.length === 0) {
      // 只有「纯元数据行」可以忽略；元数据后还有正文却没有时间标签时视为解析失败。
      if (tags > 0 && rest.length === 0) continue;
      return `LRC 第 ${index + 1} 行缺少时间标签：[mm:ss.cc]`;
    }
    for (const start of times) timed.push({ start, text: rest.trim() });
  }
  if (timed.length === 0) return 'LRC 文本中没有可识别的歌词行';
  timed.sort((a, b) => a.start - b.start);
  return timed.map((item, index) => {
    const next = timed[index + 1]?.start;
    const fallback =
      durationSecs !== undefined && durationSecs > item.start
        ? durationSecs
        : item.start + 5;
    const end = Math.max(next ?? fallback, item.start + 0.1);
    return { start: item.start, end, text: item.text };
  });
}

function parseLrcTime(tag: string): number | null {
  const parts = tag.split(':');
  let minutes: number;
  let seconds: number;
  if (parts.length === 2) {
    minutes = Number(parts[0]);
    seconds = Number(parts[1]);
  } else if (parts.length === 3) {
    minutes = Number(parts[0]);
    const whole = Number(parts[1]);
    const frac = Number(parts[2]);
    const divisor = parts[2].length === 3 ? 1000 : 100;
    seconds = whole + frac / divisor;
  } else {
    return null;
  }
  const value = minutes * 60 + seconds;
  return Number.isFinite(value) && value >= 0 ? value : null;
}
