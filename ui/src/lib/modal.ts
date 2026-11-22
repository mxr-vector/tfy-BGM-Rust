import { reactive } from 'vue';

export interface ModalOption {
  value: string;
  label: string;
}

export interface ModalField {
  name: string;
  label: string;
  type?: 'text' | 'select' | 'color';
  value?: string;
  placeholder?: string;
  options?: ModalOption[];
  /** 只读展示字段：仍随表单返回，但不可编辑。 */
  readonly?: boolean;
}

export interface ModalOptions {
  title: string;
  description?: string;
  fields?: ModalField[];
  confirmText?: string;
  cancelText?: string;
  danger?: boolean;
  validate?: (values: Record<string, string>) => string | null;
}

export interface ModalRequest extends ModalOptions {
  resolve: (values: Record<string, string> | null) => void;
}

export const modalState = reactive<{ current: ModalRequest | null }>({
  current: null,
});

/** 打开表单弹窗：确认返回字段值，取消返回 null。 */
export function showModal(options: ModalOptions): Promise<Record<string, string> | null> {
  return new Promise((resolve) => {
    modalState.current = { ...options, resolve };
  });
}

export function closeModal(result: Record<string, string> | null): void {
  const current = modalState.current;
  if (!current) return;
  modalState.current = null;
  current.resolve(result);
}