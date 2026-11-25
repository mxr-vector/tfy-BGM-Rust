import { computed, onScopeDispose, ref } from 'vue';
import { normalizeError } from '@/lib/format';
import { toast } from '@/lib/toast';

const MAX_DURATION_MS = 30_000;

export function useSoundRecording(
    onRecorded: (clip: Blob) => void,
    maxDuration = MAX_DURATION_MS,
    minDuration = 0,
) {
    const requesting = ref(false);
    const recording = ref(false);
    const elapsedMs = ref(0);
    const maxDurationMs = Math.max(1, maxDuration);
    const minDurationMs = Math.min(maxDurationMs, Math.max(0, minDuration));
    const canStop = computed(() => elapsedMs.value >= minDurationMs);
    let recorder: MediaRecorder | null = null;
    let timer = 0;
    let ticker = 0;
    let startedAt = 0;
    let disposed = false;

    function stop(): boolean {
        const capture = recorder;
        if (capture?.state !== 'recording') return false;
        const duration = performance.now() - startedAt;
        if (!disposed && duration < minDurationMs) {
            toast(`录音至少需要 ${Math.ceil((minDurationMs - duration) / 1000)} 秒，请继续录制`, 'info');
            return false;
        }
        window.clearTimeout(timer);
        window.clearInterval(ticker);
        elapsedMs.value = Math.min(maxDurationMs, duration);
        capture.stop();
        return true;
    }

    async function start(): Promise<void> {
        if (requesting.value || recording.value || disposed) return;
        if (!navigator.mediaDevices?.getUserMedia || typeof MediaRecorder === 'undefined') {
            toast('当前环境无法录音，请选择已有录音文件', 'error');
            return;
        }
        requesting.value = true;
        elapsedMs.value = 0;
        let stream: MediaStream | undefined;
        try {
            stream = await navigator.mediaDevices.getUserMedia({ audio: true });
            const input = stream;
            if (disposed) {
                input.getTracks().forEach((track) => track.stop());
                return;
            }
            const capture = new MediaRecorder(input, { audioBitsPerSecond: 128_000 });
            const chunks: Blob[] = [];
            const release = () => {
                window.clearTimeout(timer);
                window.clearInterval(ticker);
                input.getTracks().forEach((track) => track.stop());
                recorder = null;
                recording.value = false;
            };
            capture.ondataavailable = ({ data }) => {
                if (data.size > 0) chunks.push(data);
            };
            capture.onerror = () => {
                capture.onstop = null;
                release();
                if (!disposed) toast('麦克风录音失败，请重试或选择录音文件', 'error');
            };
            capture.onstop = () => {
                const duration = Math.min(maxDurationMs, performance.now() - startedAt);
                elapsedMs.value = duration;
                release();
                if (disposed) return;
                if (duration < minDurationMs) {
                    toast(`录音不足 ${Math.ceil(minDurationMs / 1000)} 秒，请重新录制`, 'error');
                    return;
                }
                const clip = new Blob(chunks, { type: capture.mimeType });
                if (clip.size) onRecorded(clip);
                else toast('没有录到音频，请重试', 'error');
            };
            capture.start();
            recorder = capture;
            startedAt = performance.now();
            recording.value = true;
            ticker = window.setInterval(() => {
                elapsedMs.value = Math.min(maxDurationMs, performance.now() - startedAt);
            }, 100);
            timer = window.setTimeout(stop, maxDurationMs);
        } catch (error) {
            stream?.getTracks().forEach((track) => track.stop());
            if (!disposed) toast(`无法启动麦克风：${normalizeError(error)}`, 'error');
        } finally {
            requesting.value = false;
        }
    }

    onScopeDispose(() => {
        disposed = true;
        window.clearTimeout(timer);
        window.clearInterval(ticker);
        const capture = recorder;
        stop();
        capture?.stream.getTracks().forEach((track) => track.stop());
    });

    return { requesting, recording, elapsedMs, maxDurationMs, minDurationMs, canStop, start, stop };
}
