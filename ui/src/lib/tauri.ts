/** Tauri 2 全局桥接（`withGlobalTauri: true`）的最小类型声明。 */

export interface TauriAppWindow {
  minimize(): Promise<void>;
  toggleMaximize(): Promise<void>;
  close(): Promise<void>;
  isMaximized(): Promise<boolean>;
  onResized(handler: () => void): Promise<() => void>;
  onFocusChanged(handler: (event: { payload: boolean }) => void): Promise<() => void>;
}

interface TauriGlobal {
  core: {
    invoke<T>(command: string, args?: Record<string, unknown>): Promise<T>;
    convertFileSrc(path: string): string;
  };
  window?: {
    getCurrentWindow(): TauriAppWindow;
  };
}

declare global {
  interface Window {
    __TAURI__?: TauriGlobal;
  }
}

const bridge = window.__TAURI__;

export const isTauri = Boolean(bridge?.core?.invoke);

export function invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  if (!bridge?.core?.invoke) {
    return Promise.reject(new Error('当前页面未运行在 Tauri 容器中，无法访问曲库。'));
  }
  return bridge.core.invoke<T>(command, args);
}

export function assetUrl(path: string): string {
  if (!bridge?.core?.convertFileSrc) return path;
  return bridge.core.convertFileSrc(path);
}

/** 当前窗口句柄；非 Tauri 环境返回 null，调用方自行降级。 */
export function getCurrentWindow(): TauriAppWindow | null {
  return bridge?.window?.getCurrentWindow() ?? null;
}