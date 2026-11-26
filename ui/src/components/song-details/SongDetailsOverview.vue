<script setup lang="ts">
import { computed } from 'vue';
import { formatTime } from '@/lib/format';
import type { AudioIndexInfo, LoopPoint, Song, StoredSubtitle } from '@/lib/types';
import Icon from '@/components/Icon.vue';
import WaveformPreview from './WaveformPreview.vue';

const props = defineProps<{
  song: Song;
  duration: number;
  subtitle: StoredSubtitle | null;
  loopPoint: LoopPoint | null;
  decoded: boolean;
  indexedInfo: AudioIndexInfo | null;
}>();

const extension = computed(() => {
  const dot = props.song.fileName.lastIndexOf('.');
  return dot >= 0 ? props.song.fileName.slice(dot + 1).toUpperCase() : '音频';
});

const facts = computed(() => {
  const rows = [
    { label: '分类', value: props.song.category || '未分类' },
    { label: '艺术家', value: props.song.artist || '未标注' },
    { label: '专辑', value: props.song.album || '未标注' },
    { label: '时长', value: props.duration > 0 ? formatTime(props.duration) : '—' },
    { label: '格式', value: extension.value },
  ];
  // 只有该曲目确实进了索引时才补充音频特征，避免没建索引的曲库满屏「未索引」。
  const indexed = props.indexedInfo;
  if (indexed) {
    rows.push(
      { label: 'BPM', value: indexed.bpm != null ? indexed.bpm.toFixed(1) : '—' },
      { label: '调性', value: indexed.musicalKey ?? '—' },
      { label: '综合响度', value: indexed.integratedLufs != null ? `${indexed.integratedLufs.toFixed(1)} LUFS` : '—' },
      { label: '真峰值', value: indexed.truePeakDb != null ? `${indexed.truePeakDb.toFixed(1)} dBTP` : '—' },
    );
  }
  rows.push({ label: '音频状态', value: props.decoded ? '已解码，可分析' : '等待音频解码' });
  return rows;
});

const loopLabel = computed(() => {
  const point = props.loopPoint;
  if (!point) return '未设置循环区间';
  return `${formatTime(point.start)}–${formatTime(point.end)} · ${point.enabled ? '已启用' : '已关闭'}`;
});
</script>

<template>
  <div class="details-overview">
    <section class="overview-card">
      <div class="overview-card-head">
        <span class="overview-card-icon"><Icon name="music" /></span>
        <div>
          <h3>曲目信息</h3>
          <p>当前播放曲目的基础资料</p>
        </div>
      </div>
      <dl class="overview-facts">
        <div v-for="fact in facts" :key="fact.label" class="overview-fact">
          <dt>{{ fact.label }}</dt>
          <dd :title="fact.value">{{ fact.value }}</dd>
        </div>
      </dl>
      <div v-if="indexedInfo?.waveform.length" class="overview-waveform">
        <span>索引波形 · 本地缓存</span>
        <WaveformPreview :waveform="indexedInfo.waveform" />
      </div>
    </section>

    <section class="overview-card">
      <div class="overview-card-head">
        <span class="overview-card-icon"><Icon name="captions" /></span>
        <div>
          <h3>字幕与歌词</h3>
          <p>生成结果可在字幕页继续编辑和导出</p>
        </div>
      </div>
      <div class="overview-status-row">
        <span>字幕状态</span>
        <strong :class="{ 'is-ready': subtitle }">
          {{ subtitle ? `已生成 · ${subtitle.segments.length} 段` : '尚未生成' }}
        </strong>
      </div>
      <div class="overview-status-row">
        <span>A/B 循环</span>
        <strong :class="{ 'is-ready': loopPoint?.enabled }">{{ loopLabel }}</strong>
      </div>
    </section>

    <section class="overview-card overview-file">
      <div class="overview-card-head">
        <span class="overview-card-icon"><Icon name="folder" /></span>
        <div>
          <h3>源文件</h3>
          <p>诊断分析直接读取该文件解码后的音频</p>
        </div>
      </div>
      <strong :title="song.fileName">{{ song.fileName }}</strong>
      <p :title="song.path">{{ song.path }}</p>
    </section>
  </div>
</template>

<style scoped>
.details-overview {
  display: flex;
  flex-direction: column;
  gap: 12px;
  height: 100%;
  min-height: 0;
  overflow-y: auto;
  padding-right: 8px;
  scrollbar-color: rgba(255, 255, 255, 0.14) transparent;
  scrollbar-width: thin;
}

.overview-card {
  flex: none;
  padding: 16px;
  border: 1px solid rgba(255, 255, 255, 0.085);
  border-radius: 15px;
  background: rgba(255, 255, 255, 0.035);
}

.overview-card-head {
  display: flex;
  align-items: center;
  gap: 11px;
  margin-bottom: 14px;
}

.overview-card-icon {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 34px;
  height: 34px;
  flex: none;
  border-radius: 11px;
  color: var(--details-accent, #19d98b);
  background: rgba(255, 255, 255, 0.055);
}

.overview-card-icon [data-icon] {
  width: 16px;
  height: 16px;
}

.overview-card h3 {
  margin: 0;
  color: var(--text);
  font-size: 13px;
  font-weight: 650;
}

.overview-card-head p {
  margin: 3px 0 0;
  color: var(--text-3);
  font-size: 10.5px;
}

.overview-facts {
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr));
  gap: 14px 18px;
  margin: 0;
}

.overview-fact {
  min-width: 0;
}

.overview-fact dt {
  margin-bottom: 4px;
  color: var(--text-3);
  font-size: 10.5px;
}

.overview-fact dd {
  overflow: hidden;
  margin: 0;
  color: var(--text-2);
  font-size: 12.5px;
  font-weight: 550;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.overview-waveform { margin-top: 15px; padding-top: 12px; border-top: 1px solid rgba(255, 255, 255, 0.06); }
.overview-waveform > span { display: block; margin-bottom: 7px; color: var(--text-3); font-size: 10px; }

.overview-status-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 20px;
  padding: 9px 0;
  border-top: 1px solid rgba(255, 255, 255, 0.06);
  color: var(--text-3);
  font-size: 11.5px;
}

.overview-status-row strong {
  overflow: hidden;
  color: var(--text-2);
  font-weight: 550;
  text-align: right;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.overview-status-row strong.is-ready {
  color: #9df7cf;
}

.overview-file > strong,
.overview-file > p {
  display: block;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.overview-file > strong {
  color: var(--text-2);
  font-size: 12px;
  font-weight: 550;
}

.overview-file > p {
  margin: 6px 0 0;
  color: var(--text-3);
  font-size: 10.5px;
}

@media (max-width: 760px) {
  .overview-facts {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }
}
</style>
