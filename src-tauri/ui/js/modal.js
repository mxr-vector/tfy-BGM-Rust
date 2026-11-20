import { setIcon } from './icons.js';
import { el } from './ui.js';

/**
 * 打开一个表单弹窗。确认后返回字段值对象，取消返回 null。
 * fields: [{ name, label, type: 'text' | 'select', value, placeholder, options: [{ value, label }] }]
 */
export function showModal({
  title,
  description = '',
  fields = [],
  confirmText = '确定',
  cancelText = '取消',
  danger = false,
  validate = null,
}) {
  return new Promise((resolve) => {
    const previousFocus = document.activeElement;

    const errorText = el('p', { class: 'modal-error', hidden: true });
    const controls = new Map();

    const bodyChildren = [];
    if (description) {
      bodyChildren.push(el('p', { class: 'modal-description', text: description }));
    }

    for (const field of fields) {
      let input;
      if (field.type === 'select') {
        input = el(
          'select',
          { class: 'modal-select', attrs: { name: field.name } },
          (field.options ?? []).map((option) =>
            el('option', {
              text: option.label,
              attrs: { value: option.value },
            }),
          ),
        );
        input.value = field.value ?? field.options?.[0]?.value ?? '';
      } else {
        input = el('input', {
          class: 'modal-input',
          attrs: {
            type: 'text',
            name: field.name,
            placeholder: field.placeholder ?? '',
            autocomplete: 'off',
            spellcheck: 'false',
          },
        });
        input.value = field.value ?? '';
      }
      controls.set(field.name, input);
      bodyChildren.push(
        el('label', { class: 'modal-field' }, [
          el('span', { class: 'modal-label', text: field.label }),
          input,
        ]),
      );
    }
    bodyChildren.push(errorText);

    const closeButton = el('button', {
      class: 'modal-close',
      attrs: { type: 'button', title: '关闭' },
    });
    setIcon(closeButton, 'x');

    const cancelButton = el('button', {
      class: 'modal-btn ghost',
      attrs: { type: 'button' },
      text: cancelText,
    });
    const confirmButton = el('button', {
      class: `modal-btn ${danger ? 'danger' : 'primary'}`,
      attrs: { type: 'submit' },
      text: confirmText,
    });

    const form = el('form', { class: 'modal-form' }, [
      el('div', { class: 'modal-body' }, bodyChildren),
      el('div', { class: 'modal-footer' }, [cancelButton, confirmButton]),
    ]);

    const panel = el(
      'div',
      { class: 'modal-panel', attrs: { role: 'dialog', 'aria-modal': 'true', 'aria-label': title } },
      [
        el('div', { class: 'modal-header' }, [
          el('h2', { class: 'modal-title', text: title }),
          closeButton,
        ]),
        form,
      ],
    );
    const backdrop = el('div', { class: 'modal-backdrop' }, [panel]);

    let closed = false;
    function close(result) {
      if (closed) return;
      closed = true;
      document.removeEventListener('keydown', onKeydown, true);
      backdrop.remove();
      confirmButton.disabled = false;
      cancelButton.disabled = false;
      if (previousFocus instanceof HTMLElement && previousFocus.isConnected) {
        previousFocus.focus();
      }
      resolve(result);
    }

    function onKeydown(event) {
      if (event.key === 'Escape') {
        event.preventDefault();
        event.stopPropagation();
        close(null);
      }
    }

    form.addEventListener('submit', (event) => {
      event.preventDefault();
      const values = {};
      for (const [name, input] of controls) values[name] = input.value;

      if (validate) {
        const error = validate(values);
        if (error) {
          errorText.textContent = error;
          errorText.hidden = false;
          return;
        }
      }
      close(values);
    });

    cancelButton.addEventListener('click', () => close(null));
    closeButton.addEventListener('click', () => close(null));
    backdrop.addEventListener('mousedown', (event) => {
      if (event.target !== backdrop) return;
      close(null);
    });

    document.addEventListener('keydown', onKeydown, true);
    document.body.append(backdrop);

    requestAnimationFrame(() => {
      const first = fields[0];
      const target = first ? controls.get(first.name) : null;
      if (target instanceof HTMLInputElement) {
        target.focus();
        target.select();
      } else if (target instanceof HTMLElement) {
        target.focus();
      } else {
        confirmButton.focus();
      }
    });
  });
}