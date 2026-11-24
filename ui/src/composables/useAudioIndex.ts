import { computed, effectScope, ref, watch } from 'vue';
import { COMMANDS, EVENTS } from '@/lib/constants';
import { normalizeError } from '@/lib/format';
import { invoke, listen } from '@/lib/tauri';
import { toast } from '@/lib/toast';
import type {
    AudioIndexInfo,
    AudioIndexProgress,
    AudioIndexStatus,
    DuplicateReport,
    LibraryMutation,
    Song,
    SoundSearchResult,
} from '@/lib/types';
import { reportToast } from '@/stores/actions';
import { applyMutation } from '@/stores/library';
import { state } from '@/stores/state';

export const audioIndexStatus = ref<AudioIndexStatus | null>(null);
export const audioIndexProgress = ref<AudioIndexProgress | null>(null);
export const audioIndexBusy = ref(false);
export const duplicatesLoading = ref(false);
export const soundSearchLoading = ref(false);

const infoByPath = ref<Record<string, AudioIndexInfo>>({});
let installed = false;
let unlistenProgress: (() => void) | null = null;
const scope = effectScope();

export const progressPercent = computed(() => {
    const progress = audioIndexProgress.value;
    if (!progress || progress.total <= 0) return 0;
    return Math.min(100, Math.round((progress.current / progress.total) * 100));
});

export async function refreshAudioIndexStatus(): Promise<AudioIndexStatus | null> {
    try {
        audioIndexStatus.value = await invoke<AudioIndexStatus>(COMMANDS.audioIndexStatus);
    } catch (error) {
        audioIndexStatus.value = null;
        console.warn('[audio-index]', error);
    }
    return audioIndexStatus.value;
}

export async function buildAudioIndex(): Promise<boolean> {
    if (audioIndexBusy.value) return false;
    audioIndexBusy.value = true;
    audioIndexProgress.value = null;
    try {
        audioIndexStatus.value = await invoke<AudioIndexStatus>(COMMANDS.buildAudioIndex);
        // 重新分析后指纹与波形都可能变化，详情缓存必须整体失效。
        clearAudioIndexInfo();
        toast(`音频索引已更新：${audioIndexStatus.value.readyTracks} 首可用`, 'success');
        return true;
    } catch (error) {
        toast(normalizeError(error), 'error');
        return false;
    } finally {
        audioIndexBusy.value = false;
        audioIndexProgress.value = null;
        await refreshAudioIndexStatus();
    }
}

export async function loadAudioIndexInfo(song: Song): Promise<AudioIndexInfo | null> {
    const cached = infoByPath.value[song.path];
    if (cached) return cached;
    try {
        const info = await invoke<AudioIndexInfo | null>(COMMANDS.audioIndexForSong, {
            key: song.key,
            path: song.path,
        });
        if (info) infoByPath.value[song.path] = info;
        return info;
    } catch (error) {
        console.warn('[audio-index]', error);
        return null;
    }
}

export function clearAudioIndexInfo(): void {
    infoByPath.value = {};
}

export async function loadDuplicates(): Promise<DuplicateReport | null> {
    if (!audioIndexStatus.value) await refreshAudioIndexStatus();
    if (!audioIndexStatus.value?.enabled) {
        toast('请先为当前曲库建立音频索引', 'info');
        return null;
    }
    duplicatesLoading.value = true;
    try {
        return await invoke<DuplicateReport>(COMMANDS.findAudioDuplicates);
    } catch (error) {
        toast(normalizeError(error), 'error');
        return null;
    } finally {
        duplicatesLoading.value = false;
    }
}

export async function searchLibraryBySound(): Promise<SoundSearchResult | null> {
    if (!audioIndexStatus.value) await refreshAudioIndexStatus();
    if (!audioIndexStatus.value?.enabled) {
        toast('请先为当前曲库建立音频索引', 'info');
        return null;
    }
    soundSearchLoading.value = true;
    try {
        return await invoke<SoundSearchResult | null>(COMMANDS.searchLibraryBySound);
    } catch (error) {
        toast(normalizeError(error), 'error');
        return null;
    } finally {
        soundSearchLoading.value = false;
    }
}

/**
 * 删除用户逐个勾选的重复文件。
 *
 * 前端不再额外弹一次自定义确认框：后端命令会用系统原生弹窗再确认一次，
 * 并校验每个路径确实属于当前重复分组，重复确认只会让用户多点一次。
 */
export async function deleteAudioDuplicates(paths: string[]): Promise<LibraryMutation | null> {
    if (paths.length === 0) return null;
    try {
        const result = await invoke<LibraryMutation | null>(COMMANDS.deleteAudioDuplicates, { paths });
        if (!result) return null;
        applyMutation(result);
        clearAudioIndexInfo();
        reportToast(result.report, '重复文件已移到回收站');
        await refreshAudioIndexStatus();
        return result;
    } catch (error) {
        toast(normalizeError(error), 'error');
        return null;
    }
}

/** 全局只安装一次；曲库变化后刷新状态，索引构建期间接收后端进度。 */
export async function installAudioIndex(): Promise<void> {
    if (installed) return;
    installed = true;
    unlistenProgress = await listen<AudioIndexProgress>(EVENTS.audioIndexProgress, (progress) => {
        audioIndexProgress.value = progress;
    });
    scope.run(() => {
        watch(
            () => state.library.root,
            () => {
                // 换曲库目录后旧路径的详情全部失效；同一目录的重新扫描不必清缓存。
                clearAudioIndexInfo();
                void refreshAudioIndexStatus();
            },
            { immediate: true },
        );
        watch(
            () => state.library.scannedAtMs,
            () => void refreshAudioIndexStatus(),
        );
    });
}

export function disposeAudioIndex(): void {
    scope.stop();
    unlistenProgress?.();
    unlistenProgress = null;
    installed = false;
}
