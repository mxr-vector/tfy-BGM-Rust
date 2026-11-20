import { reactive } from 'vue';

export type ToastKind = 'info' | 'success' | 'error';

export const toastState = reactive({
  message: '',
  kind: 'info' as ToastKind,
  shown: false,
  visible: false,
});

let hideTimer = 0;
let cleanupTimer = 0;

export function toast(message: string, kind: ToastKind = 'info'): void {
  window.clearTimeout(hideTimer);
  window.clearTimeout(cleanupTimer);

  toastState.message = message;
  toastState.kind = kind;
  toastState.shown = true;
  toastState.visible = false;
  requestAnimationFrame(() => {
    toastState.visible = true;
  });

  hideTimer = window.setTimeout(
    () => {
      toastState.visible = false;
      cleanupTimer = window.setTimeout(() => {
        toastState.shown = false;
      }, 220);
    },
    kind === 'error' ? 6000 : 3800,
  );
}