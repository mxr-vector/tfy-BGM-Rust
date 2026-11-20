import { DEFAULT_ACCENT } from './constants.js';

let toastTimer = 0;

export function el(tag, props = {}, children = []) {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(props)) {
    if (value === undefined || value === null || value === false) continue;
    if (key === 'class') {
      node.className = value;
    } else if (key === 'text') {
      node.textContent = value;
    } else if (key === 'dataset') {
      for (const [dataKey, dataValue] of Object.entries(value)) {
        node.dataset[dataKey] = dataValue;
      }
    } else if (key === 'style') {
      for (const [styleKey, styleValue] of Object.entries(value)) {
        node.style.setProperty(styleKey, styleValue);
      }
    } else if (key === 'attrs') {
      for (const [attrKey, attrValue] of Object.entries(value)) {
        node.setAttribute(attrKey, attrValue);
      }
    } else if (value === true) {
      node.setAttribute(key, '');
    } else {
      node.setAttribute(key, value);
    }
  }

  const list = Array.isArray(children) ? children : [children];
  for (const child of list) {
    if (child === undefined || child === null || child === false) continue;
    node.append(child instanceof Node ? child : document.createTextNode(String(child)));
  }
  return node;
}

export function formatTime(seconds) {
  if (!Number.isFinite(seconds) || seconds < 0) return '00:00';
  const minutes = Math.floor(seconds / 60);
  const rest = Math.floor(seconds % 60);
  return `${String(minutes).padStart(2, '0')}:${String(rest).padStart(2, '0')}`;
}

export function normalizeError(error) {
  if (typeof error === 'string') return error;
  if (error && typeof error.message === 'string') return error.message;
  try {
    return JSON.stringify(error);
  } catch {
    return String(error);
  }
}

export function toast(message, kind = 'info') {
  const node = document.getElementById('toast');
  if (!node) return;
  window.clearTimeout(toastTimer);
  node.textContent = message;
  node.hidden = false;
  node.classList.toggle('is-error', kind === 'error');
  node.classList.toggle('is-success', kind === 'success');
  requestAnimationFrame(() => node.classList.add('is-visible'));
  toastTimer = window.setTimeout(() => {
    node.classList.remove('is-visible');
    window.setTimeout(() => {
      node.hidden = true;
    }, 220);
  }, kind === 'error' ? 6000 : 3800);
}

export function clamp(value, min, max) {
  return Math.min(max, Math.max(min, value));
}

export function withAlpha(hex, alpha) {
  const value = /^#[0-9a-f]{6}$/i.test(hex) ? hex : DEFAULT_ACCENT;
  const red = Number.parseInt(value.slice(1, 3), 16);
  const green = Number.parseInt(value.slice(3, 5), 16);
  const blue = Number.parseInt(value.slice(5, 7), 16);
  return `rgba(${red}, ${green}, ${blue}, ${alpha})`;
}