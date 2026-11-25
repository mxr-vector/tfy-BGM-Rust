<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue';
import AppSelect from '@/components/AppSelect.vue';
import Icon from '@/components/Icon.vue';
import { COMMANDS } from '@/lib/constants';
import { normalizeError } from '@/lib/format';
import { invoke } from '@/lib/tauri';
import { toast } from '@/lib/toast';
import { ttsScriptFormat as format, ttsScriptText as sourceText } from '@/stores/tts';

interface Segment { text: string; startMs: number | null; endMs: number | null }
type TextFormat = 'txt' | 'lrc' | 'srt';

const DEFAULT_SCRIPT = '我深怕自己本非美玉，故而不敢加以刻苦琢磨，却又半信自己是块美玉，故又不肯庸庸碌碌，与瓦砾为伍。于是我渐渐脱离凡尘，疏远世人，结果便是一任愤懑与羞恨日益助长那怯弱的自尊心。其实任何人都是驯兽师，而那野兽，无非是个人的性情而已。';

const formatOptions = [
  { value: 'txt', label: '纯文本' },
  { value: 'lrc', label: 'LRC 字幕' },
  { value: 'srt', label: 'SRT 字幕' },
];
const title = ref('未载入文本');
const segments = ref<Segment[]>([]);
const hasText = computed(() => Boolean(sourceText.value.trim()));
let textGeneration = 0;

function invalidateText(): number {
  segments.value = [];
  return ++textGeneration;
}

function setFormat(value: string): void {
  if (value === format.value) return;
  format.value = value as TextFormat;
  invalidateText();
}

async function parseDraft(text: string, selectedFormat: TextFormat, label: string, generation: number): Promise<Segment[] | null> {
  const parsed = await invoke<Segment[]>(COMMANDS.parseTtsScript, { text, format: selectedFormat });
  if (generation !== textGeneration) return null;
  segments.value = parsed;
  title.value = label;
  return parsed;
}

async function importText(event: Event): Promise<void> {
  const input = event.target as HTMLInputElement;
  const file = input.files?.[0];
  input.value = '';
  if (!file) return;
  const extension = file.name.split('.').pop()?.toLowerCase();
  if (extension !== 'txt' && extension !== 'lrc' && extension !== 'srt') {
    toast('请选择 .txt、.lrc 或 .srt 文件', 'error');
    return;
  }
  const generation = invalidateText();
  try {
    const text = await file.text();
    if (generation !== textGeneration) return;
    sourceText.value = text;
    format.value = extension;
    const parsed = await parseDraft(text, extension, file.name, generation);
    if (parsed) toast(`已载入 ${parsed.length} 条合成文本`, 'success');
  } catch (error) {
    if (generation === textGeneration) toast(`解析文本失败：${normalizeError(error)}`, 'error');
  }
}

onMounted(() => {
  if (!sourceText.value.trim()) sourceText.value = DEFAULT_SCRIPT;
  void parseText();
});

async function parseText(): Promise<Segment[] | null> {
  const generation = invalidateText();
  const label = format.value === 'txt' ? '粘贴文本' : `粘贴的 ${format.value.toUpperCase()} 字幕`;
  try {
    return await parseDraft(sourceText.value, format.value, label, generation);
  } catch (error) {
    if (generation === textGeneration) toast(`解析文本失败：${normalizeError(error)}`, 'error');
    return null;
  }
}

onBeforeUnmount(() => { ++textGeneration; });
defineExpose({ hasText, segments, parseText });
</script>

<template>
  <section class="tts-card tts-script-card">
    <div class="tts-card-heading">
      <div><span class="tts-overline">MANUSCRIPT</span><h2>合成文本</h2></div>
      <span>{{ segments.length }} 段文本</span>
    </div>
    <div class="tts-import-actions">
      <label class="tts-action tts-file-label"><Icon name="upload" />导入文本<input type="file" accept=".txt,.lrc,.srt,text/plain" @change="importText"></label>
      <AppSelect :model-value="format" :options="formatOptions" label="文本格式" :min-menu-width="132" @update:model-value="setFormat" />
      <button type="button" class="tts-action" @click="parseText">解析文本</button>
    </div>
    <textarea v-model="sourceText" class="tts-textarea" spellcheck="false" placeholder="粘贴口播稿或字幕内容。纯文本会合并段内换行，优先按标点分段；LRC 与 SRT 按字幕条目分段。" @input="invalidateText"></textarea>
    <p v-if="format !== 'txt'" class="tts-timing-note">字幕时间戳仅用于分段与预览，不会限制语音时长、语速或音质。</p>
    <div v-if="segments.length" class="tts-segment-preview">
      <div class="tts-card-heading"><h3>{{ title }}</h3><span>{{ segments.length }} 条</span></div>
      <div v-for="(segment, index) in segments.slice(0, 4)" :key="index" class="tts-segment-row">
        <time v-if="segment.startMs !== null">{{ (segment.startMs / 1000).toFixed(2) }} s</time>
        <span>{{ segment.text }}</span>
      </div>
      <span v-if="segments.length > 4" class="tts-more">另有 {{ segments.length - 4 }} 条合成文本</span>
    </div>
  </section>
</template>

<style scoped>
.tts-script-card{display:flex;flex-direction:column}
.tts-card-heading h3{font-size:13px}
.tts-import-actions{display:flex;flex-wrap:wrap;gap:6px;margin-top:6px}
.tts-textarea{display:block;width:100%;flex:1;min-height:110px;margin-top:6px;padding:8px 10px;border:1px solid var(--line);border-radius:8px;background:rgba(1,8,5,.42);color:var(--text);font:13px/1.45 var(--font-sans);resize:none}
.tts-textarea::placeholder{color:var(--text-3)}
.tts-timing-note{margin-top:6px;color:var(--text-3);font-size:11px;line-height:1.5}
.tts-segment-preview{max-height:117px;overflow:auto;margin-top:8px;padding-top:6px;border-top:1px solid var(--line)}
.tts-segment-row{display:flex;gap:8px;padding:4px 0;border-bottom:1px solid var(--line);color:var(--text-2);font-size:12px}
.tts-segment-row time{flex:none;color:var(--accent-bright);font:11px ui-monospace,Consolas,monospace}
.tts-segment-row span{min-width:0;overflow-wrap:anywhere}
.tts-more{display:block;margin-top:4px;color:var(--text-3);font-size:11px}
@media(max-width:880px){.tts-textarea{min-height:130px}}
</style>
