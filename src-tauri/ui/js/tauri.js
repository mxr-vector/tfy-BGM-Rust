const bridge = window.__TAURI__;

export const isTauri = Boolean(bridge?.core?.invoke);

export async function invoke(command, args) {
  if (!isTauri) {
    throw new Error('当前页面未运行在 Tauri 容器中，无法访问曲库。');
  }
  return bridge.core.invoke(command, args);
}

export function assetUrl(path) {
  if (!isTauri) return path;
  return bridge.core.convertFileSrc(path);
}

/** 当前窗口句柄；非 Tauri 环境返回 null，调用方需自行降级。 */
export function getCurrentWindow() {
  if (!isTauri) return null;
  return bridge.window?.getCurrentWindow?.() ?? null;
}
