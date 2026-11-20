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