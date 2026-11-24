import { onBeforeUnmount, onMounted, readonly, shallowRef } from 'vue';
import { getCurrentWindow } from '@/lib/tauri';

const viewportWidth = shallowRef(typeof window === 'undefined' ? 0 : window.innerWidth);
const viewportHeight = shallowRef(typeof window === 'undefined' ? 0 : window.innerHeight);
const maximized = shallowRef(false);

let consumers = 0;
let listenerGeneration = 0;
let maximizeRequest = 0;
let resizeFrame = 0;
let unlistenTauri: (() => void) | null = null;

function measureViewport(): void {
  viewportWidth.value = Math.round(window.visualViewport?.width ?? window.innerWidth);
  viewportHeight.value = Math.round(window.visualViewport?.height ?? window.innerHeight);
  document.documentElement.style.setProperty('--viewport-width', `${viewportWidth.value}px`);
  document.documentElement.style.setProperty('--viewport-height', `${viewportHeight.value}px`);
}

async function syncMaximized(generation: number): Promise<void> {
  const appWindow = getCurrentWindow();
  if (!appWindow) return;
  const request = ++maximizeRequest;
  try {
    const next = await appWindow.isMaximized();
    if (generation === listenerGeneration && request === maximizeRequest) {
      maximized.value = next;
    }
  } catch {
    // 窗口关闭或平台不支持查询时保留上一次状态。
  }
}

function scheduleMeasure(): void {
  if (consumers === 0 || resizeFrame) return;
  resizeFrame = window.requestAnimationFrame(() => {
    resizeFrame = 0;
    measureViewport();
    void syncMaximized(listenerGeneration);
  });
}

async function start(): Promise<void> {
  consumers += 1;
  if (consumers > 1) return;

  const generation = ++listenerGeneration;
  measureViewport();
  void syncMaximized(generation);
  window.addEventListener('resize', scheduleMeasure, { passive: true });
  window.visualViewport?.addEventListener('resize', scheduleMeasure, { passive: true });
  window.addEventListener('focus', scheduleMeasure);

  const appWindow = getCurrentWindow();
  if (!appWindow) return;
  try {
    const off = await appWindow.onResized(scheduleMeasure);
    // 监听注册是异步的；组件可能在 Promise 完成前就已卸载。
    if (generation !== listenerGeneration) {
      off();
      return;
    }
    unlistenTauri = off;
  } catch (error) {
    console.warn('[window] 无法监听窗口尺寸变化', error);
  }
}

function stop(): void {
  consumers = Math.max(0, consumers - 1);
  if (consumers > 0) return;

  listenerGeneration += 1;
  if (resizeFrame) window.cancelAnimationFrame(resizeFrame);
  resizeFrame = 0;
  window.removeEventListener('resize', scheduleMeasure);
  window.visualViewport?.removeEventListener('resize', scheduleMeasure);
  window.removeEventListener('focus', scheduleMeasure);
  unlistenTauri?.();
  unlistenTauri = null;
}

/**
 * 共享主窗口状态：合并 Tauri onResized、浏览器 resize 与 visualViewport，
 * 避免每个组件各自监听，并让最大化 / 还原和视口尺寸保持同步。
 */
export function useWindowState() {
  onMounted(() => void start());
  onBeforeUnmount(stop);

  return {
    viewportWidth: readonly(viewportWidth),
    viewportHeight: readonly(viewportHeight),
    maximized: readonly(maximized),
    refresh: () => {
      if (consumers === 0) return;
      measureViewport();
      void syncMaximized(listenerGeneration);
    },
  };
}
