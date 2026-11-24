import { onScopeDispose, ref } from 'vue';
import { normalizeError } from '@/lib/format';
import { toast } from '@/lib/toast';

const MAX_DURATION_MS = 30_000;

export function useSoundRecording(onRecorded: (clip: Blob) => void) {
  const requesting = ref(false);
  const recording = ref(false);
  let recorder: MediaRecorder | null = null;
  let timer = 0;
  let disposed = false;

  function stop(): void {
    if (recorder?.state === 'recording') recorder.stop();
  }

  async function start(): Promise<void> {
    if (requesting.value || recording.value || disposed) return;
    if (!navigator.mediaDevices?.getUserMedia || typeof MediaRecorder === 'undefined') {
      toast('当前环境无法录音，请选择已有录音文件', 'error');
      return;
    }
    requesting.value = true;
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
        release();
        if (disposed) return;
        const clip = new Blob(chunks, { type: capture.mimeType });
        if (clip.size) onRecorded(clip);
        else toast('没有录到音频，请重试', 'error');
      };
      capture.start();
      recorder = capture;
      recording.value = true;
      timer = window.setTimeout(stop, MAX_DURATION_MS);
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
    const capture = recorder;
    stop();
    capture?.stream.getTracks().forEach((track) => track.stop());
  });

  return { requesting, recording, start, stop };
}
