/**
 * 无边框窗口的顶栏行为。
 *
 * 窗口装饰由 Tauri 关闭（`decorations: false`），顶栏即标题栏：
 * - 拖拽/双击最大化由顶栏的 `data-tauri-drag-region` 交给 Tauri 注入脚本处理；
 * - 最小化、最大化、关闭三枚按钮由这里调用窗口 API；
 * - 最大化图标随窗口状态切换，失焦时给外壳挂 `is-inactive` 还原系统标题栏的暗化观感。
 */

import { setIcon } from './icons.js';
import { getCurrentWindow } from './tauri.js';

const MAX_ICON = { maximized: 'win-restore', normal: 'win-max' };

/** 窗口操作偶尔会因权限或窗口已关闭而失败，这里只记日志，不打断界面。 */
function swallow(promise) {
  void Promise.resolve(promise).catch((error) => console.warn('[window]', error));
}

export function initWindowChrome() {
  const appWindow = getCurrentWindow();
  const controls = document.getElementById('window-controls');
  if (!appWindow) {
    // 浏览器预览等非 Tauri 环境下没有窗口可控制，直接隐藏按钮，避免出现无效控件。
    controls?.remove();
    return;
  }

  const shell = document.querySelector('.app-shell');
  const btnMin = document.getElementById('win-min');
  const btnMax = document.getElementById('win-max');
  const btnClose = document.getElementById('win-close');
  const maxIcon = btnMax?.querySelector('[data-icon]');

  btnMin?.addEventListener('click', () => swallow(appWindow.minimize()));
  btnMax?.addEventListener('click', () => swallow(appWindow.toggleMaximize()));
  btnClose?.addEventListener('click', () => swallow(appWindow.close()));

  const syncMaximize = async () => {
    let maximized;
    try {
      maximized = await appWindow.isMaximized();
    } catch {
      return;
    }
    if (!btnMax || !maxIcon) return;
    setIcon(maxIcon, maximized ? MAX_ICON.maximized : MAX_ICON.normal);
    const label = maximized ? '还原' : '最大化';
    btnMax.title = label;
    btnMax.setAttribute('aria-label', label);
  };

  void syncMaximize();
  swallow(appWindow.onResized(() => void syncMaximize()));
  swallow(
    appWindow.onFocusChanged(({ payload: focused }) => {
      shell?.classList.toggle('is-inactive', !focused);
    }),
  );
}
