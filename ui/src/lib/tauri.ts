/** Tauri 2 全局桥接（`withGlobalTauri: true`）的最小类型声明。 */

export interface TauriAppWindow {
  minimize(): Promise<void>;
  toggleMaximize(): Promise<void>;
  close(): Promise<void>;
  isMaximized(): Promise<boolean>;
  onResized(handler: () => void): Promise<() => void>;
  onFocusChanged(handler: (event: { payload: boolean }) => void): Promise<() => void>;
}

interface TauriEventPayload<T> {
  payload: T;
}

type InvokeArgs = Record<string, unknown> | ArrayBuffer;
interface InvokeOptions {
  headers: Record<string, string>;
}

interface TauriGlobal {
  core: {
    invoke<T>(command: string, args?: InvokeArgs, options?: InvokeOptions): Promise<T>;
    convertFileSrc(path: string): string;
  };
  event?: {
    listen<T>(
      event: string,
      handler: (event: TauriEventPayload<T>) => void,
    ): Promise<() => void>;
    emit<T>(event: string, payload?: T): Promise<void>;
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

export function invoke<T>(command: string, args?: InvokeArgs, options?: InvokeOptions): Promise<T> {
  if (!bridge?.core?.invoke) {
    return Promise.reject(new Error('当前页面未运行在 Tauri 容器中，无法访问曲库。'));
  }
  return bridge.core.invoke<T>(command, args, options);
}

export function assetUrl(path: string): string {
  if (!bridge?.core?.convertFileSrc) return path;
  return bridge.core.convertFileSrc(path);
}

/** 当前窗口句柄；非 Tauri 环境返回 null，调用方自行降级。 */
export function getCurrentWindow(): TauriAppWindow | null {
  return bridge?.window?.getCurrentWindow() ?? null;
}

/** 订阅后端事件；非 Tauri 环境或事件插件不可用时返回 null。 */
export async function listen<T>(
  event: string,
  handler: (payload: T) => void,
): Promise<(() => void) | null> {
  if (!bridge?.event?.listen) return null;
  try {
    return await bridge.event.listen<T>(event, ({ payload }) => handler(payload));
  } catch (error) {
    console.warn(`[event] 无法监听 ${event}`, error);
    return null;
  }
}

/** 向前端所有窗口广播事件；非 Tauri 环境或事件插件不可用时静默忽略。 */
export function emit<T>(event: string, payload?: T): Promise<void> {
  if (!bridge?.event?.emit) return Promise.resolve();
  return bridge.event.emit(event, payload);
}