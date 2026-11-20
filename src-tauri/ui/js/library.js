import { invoke } from './tauri.js';
import {
  applyMutation,
  categoryById,
  clearSelection,
  favoriteCount,
  isSelected,
  on,
  replaceLibrary,
  resetFilters,
  selectMany,
  selectedKeys,
  setCategory,
  setFavorites,
  setSearch,
  setSelected,
  setSortBy,
  sourceLabel,
  state,
  toggleFavorite,
  toggleFavoritesOnly,
  visibleSongs,
} from './store.js';
import {
  ALL_CATEGORY,
  COMMANDS,
  DEFAULT_ACCENT,
  EVENTS,
  FAVORITES_ACCENT,
  HERO_KINDS,
  LIBRARY_SOURCES,
  PLAY_MODES,
  SORT_KEYS,
} from './constants.js';
import { applyConfig, formatCopy, heroCopy } from './config.js';
import { equalizer, setIcon } from './icons.js';
import { showModal } from './modal.js';
import { el, normalizeError, toast, withAlpha } from './ui.js';
import {
  exportSong,
  playSong,
  playSongWithMode,
  toggleDrawer,
} from './player.js';

const els = {};

/** Banner 右侧的写意山水插画：以分类主题色着色，随分类切换整体色调。 */
const HERO_ART = `
<svg viewBox="0 0 660 160" preserveAspectRatio="xMaxYMid slice" aria-hidden="true">
  <defs>
    <radialGradient id="hero-moon" cx="50%" cy="50%" r="50%">
      <stop offset="0%" stop-color="#f2fff8" stop-opacity="0.92"/>
      <stop offset="38%" stop-color="#a7f3d0" stop-opacity="0.3"/>
      <stop offset="100%" stop-color="#34d97f" stop-opacity="0"/>
    </radialGradient>
    <linearGradient id="hero-fog" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0%" stop-color="#d1fae5" stop-opacity="0"/>
      <stop offset="50%" stop-color="#d1fae5" stop-opacity="0.14"/>
      <stop offset="100%" stop-color="#d1fae5" stop-opacity="0"/>
    </linearGradient>
    <filter id="hero-blur" x="-30%" y="-80%" width="160%" height="260%">
      <feGaussianBlur stdDeviation="6"/>
    </filter>
  </defs>

  <circle cx="556" cy="50" r="52" fill="url(#hero-moon)"/>
  <circle cx="556" cy="50" r="15" fill="#ecfdf5" opacity="0.18"/>

  <g fill="var(--hero-color, #19d98b)">
    <path opacity="0.08" d="M0 100 L26 92 L44 98 L66 70 L78 92 L96 84 L114 52 L128 76 L142 62 L156 88 L176 80 L198 96 L216 66 L230 88 L248 78 L268 96 L286 72 L300 90 L318 82 L338 58 L354 84 L372 74 L392 92 L410 84 L430 62 L444 86 L462 78 L486 96 L504 88 L528 68 L544 90 L562 80 L586 96 L606 88 L628 74 L646 90 L660 84 L660 160 L0 160 Z"/>
    <path opacity="0.14" d="M0 128 L30 116 L54 124 L76 104 L96 120 L118 110 L140 130 L166 114 L188 126 L210 106 L232 122 L256 112 L280 132 L304 118 L330 128 L352 108 L374 124 L398 114 L424 134 L448 120 L474 130 L500 110 L522 126 L546 116 L572 132 L596 122 L620 134 L642 124 L660 132 L660 160 L0 160 Z"/>
    <path opacity="0.24" d="M0 148 L48 144 L94 147 L138 140 L184 146 L230 141 L276 148 L322 143 L368 149 L414 145 L462 151 L512 146 L560 152 L612 148 L660 152 L660 160 L0 160 Z"/>
  </g>

  <g fill="#02120a" opacity="0.6">
    <path d="M392 150h2v-7h-2z"/><path d="M385 144l8-13 8 13z"/><path d="M387 136l6-10 6 10z"/>
    <path d="M418 151h2v-5h-2z"/><path d="M412 146l7-11 7 11z"/>
    <path d="M446 150h2v-6h-2z"/><path d="M439 144l8-12 8 12z"/><path d="M441 137l6-9 6 9z"/>
  </g>

  <g fill="url(#hero-fog)" filter="url(#hero-blur)">
    <rect x="250" y="112" width="410" height="22" rx="11"/>
    <rect x="330" y="136" width="330" height="16" rx="8"/>
  </g>

  <g fill="none" stroke="var(--hero-color, #19d98b)" stroke-width="1.2" stroke-linecap="round" opacity="0.45">
    <path d="M392 52q5-5 10 0q5-5 10 0"/>
    <path d="M424 38q4-4 8 0q4-4 8 0"/>
    <path d="M362 74q3-3 6 0q3-3 6 0"/>
  </g>
</svg>`;


let initialized = false;
let dragKey = null;
let warningsDismissed = false;
let selectionAnchor = null;
let contextMenu = null;

function allHero() {
  const copy = heroCopy(HERO_KINDS.all);
  return {
    title: copy.title,
    labelEn: copy.labelEn,
    color: DEFAULT_ACCENT,
    icon: 'music',
    description: formatCopy(copy.description, {
      categories: state.library.categories.map((category) => category.nameZh).join('、'),
    }),
  };
}

function favoritesHero() {
  const copy = heroCopy(HERO_KINDS.favorites);
  return {
    title: copy.title,
    labelEn: copy.labelEn,
    color: FAVORITES_ACCENT,
    icon: 'heart',
    description: copy.description,
  };
}

function $(id) {
  return document.getElementById(id);
}

export function initLibraryUI() {
  if (initialized) return;
  initialized = true;

  Object.assign(els, {
    brandLogo: $('brand-logo'),
    navAll: $('nav-all'),
    navFavorites: $('nav-favorites'),
    navQueue: $('nav-queue'),
    btnAddCategory: $('btn-add-category'),
    countAll: $('count-all'),
    countFavorites: $('count-favorites'),
    countQueue: $('count-queue'),
    categoryNav: $('category-nav'),
    chipAll: $('chip-all'),
    filterStrip: document.querySelector('.filter-strip'),
    statusCount: $('status-count'),
    statusPath: $('status-path'),
    statusIndicator: document.querySelector('.status-indicator'),
    btnImport: $('btn-import'),
    btnRescan: $('btn-rescan'),
    btnChoose: $('btn-choose'),
    btnOpen: $('btn-open'),
    btnReset: $('btn-reset'),
    btnConfig: $('btn-config'),
    batchBar: $('batch-bar'),
    batchCount: $('batch-count'),
    batchHint: document.querySelector('.batch-hint'),
    selectAll: $('select-all'),
    searchBox: document.querySelector('.search-box'),
    searchInput: $('search-input'),
    btnClearSearch: $('btn-clear-search'),
    btnClearSearch2: $('btn-clear-search-2'),
    sortSelect: $('sort-select'),
    warningBanner: $('warning-banner'),
    hero: $('hero'),
    resultsTitle: $('results-title'),
    resultsCount: $('results-count'),
    searchHint: $('search-hint'),
    searchHintText: $('search-hint-text'),
    emptyState: $('empty-state'),
    songTable: $('song-table'),
    songList: $('song-list'),
  });

  setIcon(els.brandLogo, 'disc');
  els.brandLogo.querySelector('svg')?.classList.add('disc-spin');

  els.navAll.addEventListener('click', () => setCategory(ALL_CATEGORY));
  els.navFavorites.addEventListener('click', toggleFavoritesOnly);
  els.navQueue.addEventListener('click', toggleDrawer);
  els.chipAll.addEventListener('click', () => setCategory(ALL_CATEGORY));
  els.btnAddCategory.addEventListener('click', () => void openCreateCategory());
  els.btnConfig.addEventListener('click', () => void openConfigFile());
  els.filterStrip.addEventListener('contextmenu', (event) => {
    const tab = event.target.closest('.category-item');
    const category = tab ? categoryById(tab.dataset.categoryId) : null;
    openCategoryMenu(event, category);
  });
  els.sortSelect.addEventListener('change', () => setSortBy(els.sortSelect.value));
  els.searchInput.addEventListener('input', () => {
    setSearch(els.searchInput.value);
  });
  els.searchInput.addEventListener('keydown', (event) => {
    // 中文输入法候选框里的回车用于上屏，不能当作“确认搜索”。
    if (event.key !== 'Enter' || event.isComposing || event.keyCode === 229) return;
    event.preventDefault();
    els.searchInput.blur();
  });
  els.btnClearSearch.addEventListener('click', clearSearch);
  els.btnClearSearch2.addEventListener('click', clearSearch);

  els.hero.addEventListener('click', (event) => {
    const button = event.target.closest('[data-hero-action]');
    if (!button) return;
    const list = visibleSongs();
    if (list.length === 0) return;
    if (button.dataset.heroAction === 'shuffle') {
      const index = Math.floor(Math.random() * list.length);
      playSong(list[index], list, true);
    } else {
      playSong(list[0], list, true);
    }
  });

  els.btnImport.addEventListener('click', () => {
    void runStatusTask(els.btnImport, openImport);
  });
  els.batchBar.addEventListener('click', (event) => {
    const button = event.target.closest('[data-batch-action]');
    if (!button) return;
    switch (button.dataset.batchAction) {
      case 'export':
        void exportSelection();
        break;
      case 'move':
        void openMove();
        break;
      case 'delete':
        void deleteSongs(selectedKeys());
        break;
      case 'clear':
        clearSelection();
        break;
      default:
        break;
    }
  });
  els.selectAll.addEventListener('change', () => {
    selectMany(
      visibleSongs().map((song) => song.key),
      els.selectAll.checked,
    );
    selectionAnchor = null;
  });
  els.btnRescan.addEventListener('click', () => {
    void runStatusTask(els.btnRescan, async () => {
      // 重新扫描前先热加载 config.json，文案与播放参数改动无需重启应用。
      const reload = await invoke(COMMANDS.reloadConfig);
      applyConfig(reload.config);
      for (const warning of reload.warnings ?? []) toast(warning, 'error');
      const snapshot = await invoke(COMMANDS.rescanLibrary);
      replaceLibrary(snapshot);
      toast(`曲库已刷新：${snapshot.songs.length} 首`, 'success');
    });
  });
  els.btnChoose.addEventListener('click', () => {
    void runStatusTask(els.btnChoose, chooseLibrary);
  });
  els.btnReset.addEventListener('click', () => {
    void runStatusTask(els.btnReset, async () => {
      const snapshot = await invoke(COMMANDS.resetLibraryDir);
      replaceLibrary(snapshot);
      toast('已恢复内置曲库', 'success');
    });
  });
  els.btnOpen.addEventListener('click', () => {
    void runStatusTask(els.btnOpen, async () => {
      await invoke(COMMANDS.openLibraryDir);
    });
  });

  els.emptyState.addEventListener('click', (event) => {
    const button = event.target.closest('[data-empty-action]');
    if (!button) return;
    if (button.dataset.emptyAction === 'reset') {
      resetFilters();
      els.searchInput.value = '';
    } else {
      void runStatusTask(button, chooseLibrary);
    }
  });

  els.warningBanner.addEventListener('click', (event) => {
    if (!event.target.closest('.warning-close')) return;
    warningsDismissed = true;
    els.warningBanner.hidden = true;
  });

  bindSongList();

  document.addEventListener('pointerdown', (event) => {
    if (contextMenu && !contextMenu.contains(event.target)) closeContextMenu();
  });
  document.addEventListener(
    'keydown',
    (event) => {
      if (event.key !== 'Escape' || !contextMenu) return;
      event.stopPropagation();
      closeContextMenu();
    },
    true,
  );
  document.addEventListener('scroll', closeContextMenu, true);
  window.addEventListener('blur', closeContextMenu);

  on(EVENTS.library, () => {
    warningsDismissed = false;
    renderAll();
  });
  on(EVENTS.view, () => {
    renderSidebar();
    renderHero();
    renderResults();
    renderList();
  });
  on(EVENTS.search, () => {
    updateSearchUi();
    renderHero();
    renderResults();
    renderList();
  });
  on(EVENTS.favorites, () => {
    renderSidebar();
    renderHero();
    renderList();
  });
  on(EVENTS.selection, updateSelectionUi);
  on(EVENTS.queue, renderQueueNav);
  on(EVENTS.player, renderList);

  els.sortSelect.value = state.sortBy;
  renderAll();
}

function clearSearch() {
  els.searchInput.value = '';
  setSearch('');
  els.searchInput.focus();
}

function handleSelectionClick(key, selected, range) {
  if (!key) return;
  if (range && selectionAnchor && selectionAnchor !== key) {
    const keys = visibleSongs().map((song) => song.key);
    const from = keys.indexOf(selectionAnchor);
    const to = keys.indexOf(key);
    if (from >= 0 && to >= 0) {
      selectMany(keys.slice(Math.min(from, to), Math.max(from, to) + 1), selected);
      selectionAnchor = key;
      return;
    }
  }
  setSelected(key, selected);
  selectionAnchor = key;
}

function categoryOptions() {
  return state.library.categories.map((category) => ({
    value: category.id,
    label: category.nameEn && category.nameEn !== category.nameZh
      ? `${category.nameZh} · ${category.nameEn}`
      : category.nameZh,
  }));
}

function closeContextMenu() {
  if (!contextMenu) return;
  contextMenu.remove();
  contextMenu = null;
}

function separator() {
  return { separator: true };
}

function showContextMenu(event, items) {
  event.preventDefault();
  closeContextMenu();

  const menu = el('div', { class: 'context-menu', attrs: { role: 'menu' } });
  for (const entry of items) {
    if (entry.separator) {
      menu.append(el('div', { class: 'context-menu-separator' }));
      continue;
    }
    const button = el('button', {
      class: `context-menu-item${entry.danger ? ' is-danger' : ''}`,
      attrs: { type: 'button', role: 'menuitem' },
    });
    const iconHost = el('span');
    setIcon(iconHost, entry.icon);
    button.append(iconHost, el('span', { text: entry.label }));
    button.addEventListener('click', () => {
      closeContextMenu();
      entry.action();
    });
    menu.append(button);
  }

  menu.style.left = `${event.clientX}px`;
  menu.style.top = `${event.clientY}px`;
  document.body.append(menu);
  contextMenu = menu;

  const rect = menu.getBoundingClientRect();
  if (event.clientX + rect.width > window.innerWidth - 8) {
    menu.style.left = `${Math.max(8, event.clientX - rect.width)}px`;
  }
  if (event.clientY + rect.height > window.innerHeight - 8) {
    menu.style.top = `${Math.max(8, event.clientY - rect.height)}px`;
  }
}

function openCategoryMenu(event, category) {
  const items = [];
  if (category) {
    items.push({
      icon: 'edit',
      label: '编辑分类',
      action: () => void openEditCategory(category),
    });
  }
  items.push({
    icon: 'plus',
    label: '新增分类',
    action: () => void openCreateCategory(),
  });
  items.push(separator(), {
    icon: 'folder',
    label: '打开分类配置文件',
    action: () => void openCategoriesFile(),
  });
  items.push({
    icon: 'edit',
    label: '打开应用配置文件',
    action: () => void openConfigFile(),
  });
  showContextMenu(event, items);
}

function openSongMenu(event, song) {
  if (!state.selected.has(song.key)) {
    clearSelection();
    setSelected(song.key, true);
  }
  const keys = selectedKeys();
  const multiple = keys.length > 1;
  const allFavorite = keys.every((key) => state.favorites.has(key));
  const list = visibleSongs();

  const items = [
    { icon: 'play', label: '播放', action: () => playSong(song, list, true) },
    {
      icon: 'repeat',
      label: '顺序播放',
      action: () => playSongWithMode(song, list, PLAY_MODES.sequence),
    },
    {
      icon: 'shuffle',
      label: '随机播放',
      action: () => playSongWithMode(song, list, PLAY_MODES.shuffle),
    },
    separator(),
    {
      icon: 'heart',
      label: allFavorite ? '取消收藏' : '收藏',
      action: () => setFavorites(keys, !allFavorite),
    },
  ];
  if (!multiple) {
    items.push({
      icon: 'edit',
      label: '重命名文件',
      action: () => void openRenameTrack(song),
    });
  }
  items.push(
    {
      icon: 'folder',
      label: multiple ? '批量移动分类' : '修改分类',
      action: () =>
        void openMove(keys, multiple ? '批量移动分类' : '修改分类'),
    },
    {
      icon: 'download',
      label: multiple ? `导出所选 (${keys.length})` : '导出音频',
      action: () => (multiple ? void exportSelection() : void exportSong(song.key)),
    },
    separator(),
    {
      icon: 'trash',
      label: multiple ? `删除所选 (${keys.length})` : '删除',
      danger: true,
      action: () => void deleteSongs(keys),
    },
  );

  showContextMenu(event, items);
}

async function openCreateCategory() {
  const values = await showModal({
    title: '新增分类',
    description:
      '新分类会出现在分类导航中，并在曲库目录下创建同名文件夹；之后可直接向该分类导入音频。',
    fields: [
      {
        name: 'name',
        label: '分类名称',
        type: 'text',
        placeholder: '例如：旅行',
      },
      {
        name: 'id',
        label: '目录名（可选）',
        type: 'text',
        placeholder: '留空则使用分类名称',
      },
    ],
    confirmText: '创建',
    validate: (input) => (input.name.trim() ? null : '分类名称不能为空'),
  });
  if (!values) return;

  try {
    const result = await invoke(COMMANDS.createCategory, {
      name: values.name,
      id: values.id.trim() || null,
    });
    replaceLibrary(result.library);
    setCategory(result.categoryId);
    const created = result.library.categories.find((c) => c.id === result.categoryId);
    toast(`已新增分类：${created?.nameZh ?? result.categoryId}`, 'success');
  } catch (error) {
    toast(normalizeError(error), 'error');
  }
}

async function openEditCategory(category) {
  const values = await showModal({
    title: '编辑分类',
    description:
      '修改分类的显示名称与主题色，不会移动或重命名已导入的音频文件；目录名保持不变。',
    fields: [
      {
        name: 'name',
        label: '分类名称',
        type: 'text',
        value: category.nameZh,
      },
      {
        name: 'color',
        label: '主题色',
        type: 'color',
        value: category.color,
      },
    ],
    confirmText: '保存',
    validate: (input) => (input.name.trim() ? null : '分类名称不能为空'),
  });
  if (!values) return;
  const sameName = values.name.trim() === category.nameZh;
  const sameColor = values.color.toLowerCase() === category.color.toLowerCase();
  if (sameName && sameColor) return;

  try {
    const result = await invoke(COMMANDS.updateCategory, {
      id: category.id,
      name: values.name,
      color: values.color,
    });
    replaceLibrary(result.library);
    toast('分类已更新', 'success');
  } catch (error) {
    toast(normalizeError(error), 'error');
  }
}

async function openCategoriesFile() {
  try {
    await invoke(COMMANDS.openCategoriesFile);
    toast('已在文件管理器中定位 categories.json', 'success');
  } catch (error) {
    toast(normalizeError(error), 'error');
  }
}

async function openConfigFile() {
  try {
    await invoke(COMMANDS.openConfigFile);
    toast('已在文件管理器中定位 config.json，修改后点「重新扫描」热加载', 'success');
  } catch (error) {
    toast(normalizeError(error), 'error');
  }
}

async function openRenameTrack(song) {
  const values = await showModal({
    title: '重命名文件',
    description: '修改后会重写曲库中的 mp3 文件名，收藏与播放队列会自动跟随。',
    fields: [
      {
        name: 'title',
        label: '文件名',
        type: 'text',
        value: song.title,
        placeholder: '输入新的文件名（不含扩展名）',
      },
    ],
    confirmText: '重命名',
    validate: (input) => (input.title.trim() ? null : '文件名不能为空'),
  });
  if (!values || values.title.trim() === song.title) return;

  try {
    const result = await invoke(COMMANDS.updateTrack, {
      key: song.key,
      title: values.title,
      category: song.category,
    });
    applyMutation(result);
    reportToast(result.report, '文件名已更新');
  } catch (error) {
    toast(normalizeError(error), 'error');
  }
}

function preferredCategory() {
  if (!state.favoritesOnly && state.category !== ALL_CATEGORY) return state.category;
  return state.library.categories[0]?.id ?? '';
}

async function openImport() {
  const values = await showModal({
    title: '导入音乐',
    description: '选择本地 MP3 文件，复制到曲库的指定分类目录；原文件不会被移动。',
    fields: [
      {
        name: 'category',
        label: '目标分类',
        type: 'select',
        value: preferredCategory(),
        options: categoryOptions(),
      },
    ],
    confirmText: '选择文件并导入',
  });
  if (!values) return;

  try {
    const result = await invoke(COMMANDS.importTracks, { category: values.category });
    if (!result) return;
    applyMutation(result);
    if (result.report.succeeded > 0) {
      setCategory(values.category);
      els.searchInput.value = '';
      setSearch('');
    }
    reportToast(result.report, '导入完成');
  } catch (error) {
    toast(normalizeError(error), 'error');
  }
}

async function openEdit(song) {
  const values = await showModal({
    title: '编辑曲目',
    description: '修改曲名会重命名音频文件，修改分类会移动文件；收藏会自动跟随。',
    fields: [
      {
        name: 'title',
        label: '曲名',
        type: 'text',
        value: song.title,
        placeholder: '输入新的曲名',
      },
      {
        name: 'category',
        label: '风格分类',
        type: 'select',
        value: song.category,
        options: categoryOptions(),
      },
    ],
    confirmText: '保存',
    validate: (input) => (input.title.trim() ? null : '曲名不能为空'),
  });
  if (!values) return;
  if (values.title.trim() === song.title && values.category === song.category) return;

  try {
    const result = await invoke(COMMANDS.updateTrack, {
      key: song.key,
      title: values.title,
      category: values.category,
    });
    applyMutation(result);
    reportToast(result.report, '曲目已更新');
  } catch (error) {
    toast(normalizeError(error), 'error');
  }
}

async function openMove(keys = selectedKeys(), title = null) {
  if (keys.length === 0) return;
  const values = await showModal({
    title: title ?? `移动 ${keys.length} 首曲目`,
    description: '选择目标分类；目标目录中已有同名文件时会自动追加序号。',
    fields: [
      {
        name: 'category',
        label: '目标分类',
        type: 'select',
        value: preferredCategory(),
        options: categoryOptions(),
      },
    ],
    confirmText: '移动',
  });
  if (!values) return;

  try {
    const result = await invoke(COMMANDS.moveTracks, {
      keys,
      category: values.category,
    });
    applyMutation(result);
    clearSelection();
    reportToast(result.report, '移动完成');
  } catch (error) {
    toast(normalizeError(error), 'error');
  }
}

async function exportSelection() {
  const keys = selectedKeys();
  if (keys.length === 0) return;
  try {
    const result = await invoke(COMMANDS.exportTracks, { keys });
    if (!result) return;
    reportToast(result.report, `导出完成 · ${result.dir}`);
  } catch (error) {
    toast(normalizeError(error), 'error');
  }
}

async function deleteSongs(keys) {
  if (!keys || keys.length === 0) return;
  try {
    const result = await invoke(COMMANDS.deleteTracks, { keys });
    if (!result) return;
    applyMutation(result);
    reportToast(result.report, '删除完成');
  } catch (error) {
    toast(normalizeError(error), 'error');
  }
}

function reportToast(report, fallback) {
  if (!report) return;
  const parts = [];
  if (report.succeeded) parts.push(`${report.succeeded} 首成功`);
  if (report.skipped) parts.push(`${report.skipped} 首跳过`);
  if (report.failed) parts.push(`${report.failed} 首失败`);
  let message = parts.length > 0 ? `${fallback}：${parts.join('，')}` : fallback;
  if (report.notice) {
    message += `。${report.notice}`;
  }
  if (report.failed > 0 && report.failures?.length > 0) {
    message += `。${report.failures[0]}`;
  }
  toast(message, report.failed > 0 && report.succeeded === 0 ? 'error' : 'success');
}

async function chooseLibrary() {
  const snapshot = await invoke(COMMANDS.chooseLibraryDir);
  if (snapshot) {
    replaceLibrary(snapshot);
    toast(`已切换曲库目录：${snapshot.root}`, 'success');
  }
}

async function runStatusTask(button, task) {
  button.disabled = true;
  try {
    await task();
  } catch (error) {
    toast(normalizeError(error), 'error');
  } finally {
    button.disabled = false;
  }
}

function renderAll() {
  renderSidebar();
  renderHero();
  renderResults();
  renderList();
  renderStatus();
  renderWarnings();
  updateSearchUi();
}

function renderSidebar() {
  els.countAll.textContent = String(state.library.songs.length);
  els.countFavorites.textContent = String(favoriteCount());
  renderQueueNav();

  const allActive = state.category === ALL_CATEGORY && !state.favoritesOnly;
  els.navAll.classList.toggle('is-active', allActive);
  els.navFavorites.classList.toggle('is-active', state.favoritesOnly);
  els.chipAll.classList.toggle('is-active', allActive);
  els.chipAll.setAttribute('aria-selected', String(allActive));

  els.categoryNav.replaceChildren();
  for (const category of state.library.categories) {
    const active = state.category === category.id && !state.favoritesOnly;
    let marker;
    if (category.icon) {
      marker = el('span', { class: 'chip-icon' });
      setIcon(marker, category.icon);
    } else {
      marker = el('span', { class: 'chip-dot' });
      marker.style.backgroundColor = category.color;
    }

    const chip = el(
      'button',
      {
        class: `filter-chip category-item${active ? ' is-active' : ''}`,
        attrs: {
          type: 'button',
          role: 'tab',
          'aria-selected': String(active),
          title: `${category.nameZh} · ${category.nameEn}`,
        },
        dataset: { categoryId: category.id },
      },
      [
        marker,
        el('span', { class: 'chip-label', text: category.nameZh }),
        el('span', { class: 'chip-count', text: String(category.count) }),
      ],
    );
    chip.addEventListener('click', () => setCategory(category.id));
    els.categoryNav.append(chip);
  }
}

function renderQueueNav() {
  if (!els.countQueue) return;
  els.countQueue.textContent = String(state.queue.length);
}

function heroInfo() {
  if (state.favoritesOnly) return favoritesHero();
  if (state.category === ALL_CATEGORY) return allHero();
  const category = categoryById(state.category);
  if (!category) return allHero();
  return {
    title: category.title,
    labelEn: category.labelEn,
    color: category.color,
    icon: category.icon || 'music',
    description: category.description,
  };
}

function renderHero() {
  const info = heroInfo();
  const list = visibleSongs();
  els.hero.style.setProperty('--hero-color', info.color);

  const tagIcon = el('span');
  setIcon(tagIcon, info.icon);
  const tag = el('span', { class: 'hero-tag' }, [tagIcon, info.labelEn]);
  tag.style.color = info.color;
  tag.style.backgroundColor = withAlpha(info.color, 0.13);
  tag.style.borderColor = withAlpha(info.color, 0.24);

  const source = el('span', {
    class: 'hero-source',
    text: sourceLabel(),
  });

  const playButton = el('button', {
    class: 'primary-play-btn',
    attrs: { type: 'button', 'data-hero-action': 'play-all' },
  });
  if (list.length === 0) playButton.disabled = true;
  const playIcon = el('span');
  setIcon(playIcon, 'play-fill');
  playButton.append(playIcon, el('span', { text: `全部播放 (${list.length})` }));

  const shuffleButton = el(
    'button',
    { class: 'secondary-btn', attrs: { type: 'button', 'data-hero-action': 'shuffle' } },
    [],
  );
  if (list.length === 0) shuffleButton.disabled = true;
  const shuffleIcon = el('span');
  setIcon(shuffleIcon, 'shuffle');
  shuffleButton.append(shuffleIcon, el('span', { text: '随机试听' }));

  const content = el('div', { class: 'hero-content' }, [
    el('div', { class: 'hero-heading' }, [
      el('div', { class: 'hero-badges' }, [tag, source]),
      el('h1', { class: 'hero-title', text: info.title, title: info.title }),
    ]),
    el('div', { class: 'hero-actions' }, [playButton, shuffleButton]),
    el('p', {
      class: 'hero-description',
      text: info.description,
      title: info.description,
    }),
  ]);

  const art = el('div', { class: 'hero-art', attrs: { 'aria-hidden': 'true' } });
  art.innerHTML = HERO_ART;

  els.hero.replaceChildren(art, content);
}

function renderResults() {
  let title = heroCopy(HERO_KINDS.all).resultsTitle;
  if (state.favoritesOnly) {
    title = heroCopy(HERO_KINDS.favorites).resultsTitle;
  } else if (state.category !== ALL_CATEGORY) {
    const category = categoryById(state.category);
    if (!category) {
      title = '音频列表';
    } else if (category.nameEn && category.nameEn !== category.nameZh) {
      title = `${category.nameZh}音乐 (${category.nameEn})`;
    } else {
      title = `${category.nameZh}音乐`;
    }
  }
  els.resultsTitle.textContent = title;
  els.resultsCount.textContent = `${visibleSongs().length} 首配乐`;

  const query = state.search.trim();
  els.searchHint.hidden = query.length === 0;
  els.searchHintText.textContent = `“${query}”`;
}

function updateSearchUi() {
  if (els.searchInput.value !== state.search) {
    els.searchInput.value = state.search;
  }
  els.btnClearSearch.hidden = state.search.length === 0;
  els.searchBox?.classList.toggle('has-value', state.search.length > 0);
  if (state.search.trim().length === 0 && els.searchHint) {
    els.searchHint.hidden = true;
  }
}

function renderList() {
  const list = visibleSongs();
  const isEmpty = list.length === 0;
  els.songTable.hidden = isEmpty;
  els.emptyState.hidden = !isEmpty;

  if (isEmpty) {
    els.songList.replaceChildren();
    renderEmptyState();
    updateSelectionUi();
    return;
  }

  const rows = list.map((song, index) => buildRow(song, index));
  els.songList.replaceChildren(...rows);
  updateSelectionUi();
}

function updateSelectionUi() {
  const list = visibleSongs();
  const count = state.selected.size;
  els.batchBar.hidden = count === 0;
  els.batchCount.textContent = `已选 ${count} 首`;

  const visibleSelected = list.reduce(
    (total, song) => total + (state.selected.has(song.key) ? 1 : 0),
    0,
  );
  if (els.batchHint) {
    els.batchHint.textContent =
      count > visibleSelected
        ? `当前筛选内 ${visibleSelected} 首`
        : '对选中曲目执行批量操作';
  }
  els.selectAll.disabled = list.length === 0;
  els.selectAll.checked = list.length > 0 && visibleSelected === list.length;
  els.selectAll.indeterminate = visibleSelected > 0 && visibleSelected < list.length;

  for (const row of els.songList.querySelectorAll('.song-row')) {
    const selected = state.selected.has(row.dataset.key);
    row.classList.toggle('is-selected', selected);
    const checkbox = row.querySelector('.row-checkbox');
    if (checkbox) checkbox.checked = selected;
  }
}

function renderEmptyState() {
  const libraryEmpty = state.library.songs.length === 0;
  const iconHost = el('span');
  setIcon(iconHost, 'music');

  if (libraryEmpty) {
    const choose = el('button', {
      class: 'reset-filter-btn',
      attrs: { type: 'button', 'data-empty-action': 'choose' },
      text: '选择曲库文件夹',
    });
    els.emptyState.replaceChildren(
      iconHost,
      el('h3', { class: 'empty-title', text: '曲库为空' }),
      el('p', {
        class: 'empty-text',
        text: `当前曲库目录：${state.library.root || '未找到'}。请把 .mp3 放入对应分类子目录，或选择其他文件夹。`,
      }),
      choose,
    );
    return;
  }

  els.emptyState.replaceChildren(
    iconHost,
    el('h3', { class: 'empty-title', text: '没有找到匹配的音频素材' }),
    el('p', {
      class: 'empty-text',
      text: '请尝试更换搜索词，或清除分类与筛选条件。',
    }),
    el('button', {
      class: 'reset-filter-btn',
      attrs: { type: 'button', 'data-empty-action': 'reset' },
      text: '重置所有筛选',
    }),
  );
}

function buildRow(song, index) {
  const category = categoryById(song.category);
  const color = category?.color ?? DEFAULT_ACCENT;
  const isCurrent = state.currentKey === song.key;
  const isPlaying = isCurrent && state.playing;
  const isFavorite = state.favorites.has(song.key);

  const checkbox = el('input', {
    class: 'row-checkbox',
    attrs: { type: 'checkbox', 'aria-label': `选择 ${song.title}` },
  });
  checkbox.checked = isSelected(song.key);

  const indexCell = el('div', { class: 'row-index' });
  if (isPlaying) {
    indexCell.append(equalizer(color));
  } else {
    indexCell.append(
      el('span', {
        class: 'index-number',
        text: String(index + 1).padStart(2, '0'),
      }),
    );
    const play = el('span', { class: 'row-play-btn' });
    setIcon(play, 'play-fill');
    indexCell.append(play);
  }

  const title = el('span', { class: 'row-title', text: song.title, title: song.title });

  const tag = el('span', {
    class: 'category-tag',
    text: category?.nameZh ?? song.category,
    dataset: { categoryId: song.category },
  });
  tag.style.color = withAlpha(color, 0.95);
  tag.style.backgroundColor = withAlpha(color, 0.07);
  tag.style.borderColor = withAlpha(color, 0.18);

  const favorite = el('button', {
    class: `action-icon-btn fav-btn${isFavorite ? ' is-persistent is-favorite' : ''}`,
    attrs: { type: 'button', title: isFavorite ? '取消收藏' : '收藏' },
    dataset: { action: 'favorite', key: song.key },
  });
  setIcon(favorite, 'heart');

  const edit = el('button', {
    class: 'action-icon-btn',
    attrs: { type: 'button', title: '编辑曲名 / 分类' },
    dataset: { action: 'edit', key: song.key },
  });
  setIcon(edit, 'edit');

  const download = el('button', {
    class: 'action-icon-btn is-persistent',
    attrs: { type: 'button', title: '导出音频' },
    dataset: { action: 'export', key: song.key },
  });
  setIcon(download, 'download');

  const remove = el('button', {
    class: 'action-icon-btn is-danger',
    attrs: { type: 'button', title: '删除（移到回收站）' },
    dataset: { action: 'delete', key: song.key },
  });
  setIcon(remove, 'trash');

  const canDrag = state.sortBy === SORT_KEYS.recommended;
  return el(
    'div',
    {
      class: `song-row${isCurrent ? ' is-current' : ''}${isSelected(song.key) ? ' is-selected' : ''}`,
      dataset: { key: song.key },
      attrs: {
        draggable: canDrag ? 'true' : 'false',
        tabindex: '0',
        title: canDrag ? '拖动可调整顺序' : '按曲名排序时不可拖动',
      },
    },
    [
      el('span', { class: 'row-check' }, [checkbox]),
      indexCell,
      el('div', { class: 'row-title-box' }, [title]),
      el('div', { class: 'row-category' }, [tag]),
      el('div', { class: 'row-actions' }, [favorite, edit, download, remove]),
    ],
  );
}

function bindSongList() {
  els.songList.addEventListener('contextmenu', (event) => {
    const tag = event.target.closest('.category-tag');
    const tagCategory = tag ? categoryById(tag.dataset.categoryId) : null;
    if (tagCategory) {
      openCategoryMenu(event, tagCategory);
      return;
    }
    const row = event.target.closest('.song-row');
    if (!row) return;
    const song = state.library.songs.find((item) => item.key === row.dataset.key);
    if (song) openSongMenu(event, song);
  });

  els.songList.addEventListener('click', (event) => {
    const checkbox = event.target.closest('.row-checkbox');
    if (checkbox) {
      event.stopPropagation();
      const row = checkbox.closest('.song-row');
      handleSelectionClick(row?.dataset.key, checkbox.checked, event.shiftKey);
      return;
    }

    const action = event.target.closest('[data-action]');
    if (action) {
      event.stopPropagation();
      const { action: kind, key } = action.dataset;
      if (kind === 'favorite') {
        toggleFavorite(key);
      } else if (kind === 'export') {
        void exportSong(key);
      } else if (kind === 'edit') {
        const song = state.library.songs.find((item) => item.key === key);
        if (song) void openEdit(song);
      } else if (kind === 'delete') {
        void deleteSongs([key]);
      }
      return;
    }
    const row = event.target.closest('.song-row');
    if (!row) return;
    const song = state.library.songs.find((item) => item.key === row.dataset.key);
    if (song) playSong(song, visibleSongs());
  });

  els.songList.addEventListener('keydown', (event) => {
    if (event.key !== 'Enter' && event.key !== ' ') return;
    if (event.target.closest('button, a, [data-action], input, select, textarea')) return;
    const row = event.target.closest('.song-row');
    if (!row) return;
    event.preventDefault();
    event.stopPropagation();
    const song = state.library.songs.find((item) => item.key === row.dataset.key);
    if (song) playSong(song, visibleSongs());
  });

  els.songList.addEventListener('dragstart', (event) => {
    const row = event.target.closest('.song-row');
    if (!row || row.getAttribute('draggable') !== 'true') {
      event.preventDefault();
      return;
    }
    dragKey = row.dataset.key;
    row.classList.add('is-dragging');
    event.dataTransfer.effectAllowed = 'move';
    event.dataTransfer.setData('text/plain', dragKey);
  });

  els.songList.addEventListener('dragover', (event) => {
    if (!dragKey) return;
    event.preventDefault();
    event.dataTransfer.dropEffect = 'move';
    const row = event.target.closest('.song-row');
    for (const other of els.songList.querySelectorAll('.is-drag-over')) {
      other.classList.remove('is-drag-over');
    }
    if (row && row.dataset.key !== dragKey) {
      row.classList.add('is-drag-over');
    }
  });

  els.songList.addEventListener('drop', (event) => {
    event.preventDefault();
    const row = event.target.closest('.song-row');
    const targetKey = row?.dataset.key;
    clearDragStyles();
    if (dragKey && targetKey && dragKey !== targetKey) {
      reorderLibrary(dragKey, targetKey);
      renderList();
    }
    dragKey = null;
  });

  els.songList.addEventListener('dragend', () => {
    dragKey = null;
    clearDragStyles();
  });
}

function clearDragStyles() {
  for (const row of els.songList.querySelectorAll('.is-drag-over, .is-dragging')) {
    row.classList.remove('is-drag-over', 'is-dragging');
  }
}

function reorderLibrary(fromKey, toKey) {
  const songs = state.library.songs;
  const fromIndex = songs.findIndex((song) => song.key === fromKey);
  const targetIndex = songs.findIndex((song) => song.key === toKey);
  if (fromIndex < 0 || targetIndex < 0 || fromIndex === targetIndex) return;

  const [moved] = songs.splice(fromIndex, 1);
  const insertAt = songs.findIndex((song) => song.key === toKey);
  songs.splice(fromIndex < targetIndex ? insertAt + 1 : insertAt, 0, moved);
}

function renderStatus() {
  els.statusCount.textContent = `${state.library.songs.length} 首`;
  els.statusPath.textContent = `${sourceLabel()} · ${state.library.root || '未找到曲库目录'}`;
  els.statusPath.title = state.library.root;
  els.btnReset.hidden = state.library.source !== LIBRARY_SOURCES.custom;
  els.statusIndicator?.classList.toggle(
    'is-warn',
    state.library.warnings.length > 0,
  );
}

function renderWarnings() {
  const warnings = state.library.warnings;
  if (warnings.length === 0 || warningsDismissed) {
    els.warningBanner.hidden = true;
    return;
  }

  const iconHost = el('span');
  setIcon(iconHost, 'alert');
  const close = el('button', {
    class: 'warning-close',
    attrs: { type: 'button', title: '暂时隐藏' },
  });
  setIcon(close, 'x');

  els.warningBanner.replaceChildren(
    iconHost,
    el('div', { class: 'warning-list' }, warnings.map((warning) => el('div', { text: warning }))),
    close,
  );
  els.warningBanner.hidden = false;
}