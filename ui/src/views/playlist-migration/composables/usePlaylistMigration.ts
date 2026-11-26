import { computed, readonly, ref, shallowRef } from 'vue';
import { COMMANDS } from '@/lib/constants';
import { normalizeError } from '@/lib/format';
import { invoke } from '@/lib/tauri';
import { toast } from '@/lib/toast';

interface PlaylistTrack {
  title: string;
  artists: string[];
}

interface PlaylistResult {
  platform: 'netease' | 'qq' | 'qishui';
  title: string;
  tracks: PlaylistTrack[];
  expectedTotal: number | null;
  warning: string | null;
}

export interface PlaylistSource {
  id: number;
  url: string;
  status: 'queued' | 'loading' | 'success' | 'partial' | 'error';
  result: PlaylistResult | null;
  error: string | null;
}

export type PlaylistFormat = 'title-artist' | 'artist-title' | 'title-only';

export interface PlaylistStats {
  total: number;
  unique: number;
  removed: number;
}

export type PlaylistCoverage = 'loading' | 'incomplete' | 'complete';

const MAX_CONCURRENT = 3;
// Module scope keeps the draft and pending requests alive when the view unmounts.
const draft = ref('');
const sources = shallowRef<PlaylistSource[]>([]);
const format = ref<PlaylistFormat>('title-artist');
const saving = ref(false);
let nextSourceId = 0;
let activeRequests = 0;

const busy = computed(() =>
  sources.value.some(({ status }) => status === 'queued' || status === 'loading'),
);
const coverage = computed<PlaylistCoverage>(() => {
  if (busy.value) return 'loading';
  return sources.value.some(({ status }) => status === 'partial' || status === 'error')
    ? 'incomplete'
    : 'complete';
});

function singleLine(value: string): string {
  return value.replace(/\r\n|[\n\r\v\f\u0085\u2028\u2029]/g, ' ');
}

function matchField(value: string): string {
  return singleLine(value).trim().replace(/\s+/g, ' ').toLowerCase().normalize('NFC');
}

const output = computed(() => {
  const lines: string[] = [];
  const seen = new Set<string>();
  const mode = format.value;
  let total = 0;

  for (const source of sources.value) {
    if (!source.result) continue;
    for (const track of source.result.tracks) {
      total += 1;
      const titleKey = matchField(track.title);
      if (!titleKey) continue;
      const key = mode === 'title-only'
        ? titleKey
        : JSON.stringify([
            titleKey,
            [...new Set(track.artists.map(matchField).filter(Boolean))].sort(),
          ]);
      if (seen.has(key)) continue;
      seen.add(key);

      const title = singleLine(track.title);
      const artists = track.artists.map(singleLine).filter((artist) => artist.trim()).join(' / ');
      if (!artists || mode === 'title-only') lines.push(title);
      else if (mode === 'artist-title') lines.push(`${artists} - ${title}`);
      else lines.push(`${title} - ${artists}`);
    }
  }

  const stats: PlaylistStats = { total, unique: lines.length, removed: total - lines.length };
  return { stats, text: lines.join('\n') };
});

function sourceUrls(text: string): string[] {
  // Keep query strings and hash routes intact while dropping share-text delimiters.
  const matches = text.match(/https?:\/\/[^\s<>"'`()[\]{}，。！？、；：）】》」』“”‘’]+/gi) ?? [];
  return matches.map((match) => match.replace(/[.,!?;:]+$/, ''));
}

function replaceSource(previous: PlaylistSource, next: PlaylistSource): void {
  const index = sources.value.indexOf(previous);
  if (index < 0) return;
  const updated = sources.value.slice();
  updated[index] = next;
  sources.value = updated;
}

function pumpQueue(): void {
  while (activeRequests < MAX_CONCURRENT) {
    const queued = sources.value.find(({ status }) => status === 'queued');
    if (!queued) return;
    const loading: PlaylistSource = { ...queued, status: 'loading' };
    replaceSource(queued, loading);
    activeRequests += 1;
    void readSource(loading);
  }
}

async function readSource(source: PlaylistSource): Promise<void> {
  try {
    const result = await invoke<PlaylistResult>(COMMANDS.parsePlaylistMigrationSource, { url: source.url });
    replaceSource(source, { ...source, status: result.warning === null ? 'success' : 'partial', result });
  } catch (error) {
    replaceSource(source, {
      ...source,
      status: 'error',
      result: null,
      error: normalizeError(error) || '读取失败',
    });
  } finally {
    activeRequests -= 1;
    pumpQueue();
  }
}

function parseSources(): void {
  if (busy.value) return;
  const urls = sourceUrls(draft.value);
  if (!urls.length) {
    toast('未找到歌单链接，请粘贴网址或带链接的分享文案', 'error');
    return;
  }
  sources.value = urls.map((url) => ({
    id: ++nextSourceId,
    url,
    status: 'queued',
    result: null,
    error: null,
  }));
  pumpQueue();
}

function retrySource(id: number): void {
  const source = sources.value.find((item) => item.id === id);
  if (!source || (source.status !== 'partial' && source.status !== 'error')) return;
  replaceSource(source, { ...source, status: 'queued', error: null });
  pumpQueue();
}

async function copyText(): Promise<void> {
  const { text } = output.value;
  if (!text) return;
  try {
    await navigator.clipboard.writeText(text);
    toast('已复制歌单文本', 'success');
  } catch {
    toast('复制失败，请手动选择预览文本', 'error');
  }
}

async function saveText(): Promise<void> {
  const { text } = output.value;
  if (!text || saving.value) return;
  saving.value = true;
  try {
    const path = await invoke<string | null>(COMMANDS.savePlaylistMigrationText, { text });
    if (path !== null) toast(`已保存歌单文本：${path}`, 'success');
  } catch (error) {
    toast(`保存失败：${normalizeError(error)}`, 'error');
  } finally {
    saving.value = false;
  }
}

export function usePlaylistMigration() {
  return {
    draft,
    sources: readonly(sources),
    format,
    busy,
    coverage,
    output,
    saving: readonly(saving),
    parseSources,
    retrySource,
    copyText,
    saveText,
  };
}
