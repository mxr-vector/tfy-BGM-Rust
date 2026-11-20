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
  setSearch,
  setSelected,
  setSortBy,
  sourceLabel,
  state,
  toggleFavorite,
  toggleFavoritesOnly,
  visibleSongs,
} from './store.js';
import { CATEGORY_ICONS, equalizer, setIcon } from './icons.js';
import { showModal } from './modal.js';
import { el, normalizeError, toast, withAlpha } from './ui.js';
import { exportSong, playSong } from './player.js';

const els = {};

let initialized = false;
let dragKey = null;
let warningsDismissed = false;
let selectionAnchor = null;

const ALL_HERO = {
  title: '全部精选视频剪辑 BGM',
  labelEn: 'ALL BGM SOUNDTRACKS',
  color: '#6366f1',
  icon: 'music',
  description:
    '汇集欢快、独处、回忆、史诗、古风、乡村等 6 大分类优质背景音乐，覆盖 Vlog、宣传片、故事片、卡点等全场景剪辑需求。',
};

const FAVORITES_HERO = {
  title: '我的收藏夹 · Favorite Tracks',
  labelEn: 'FAVORITES',
  color: '#ec4899',
  icon: 'heart',
  description: '你标记喜欢的配乐都在这里，方便在剪辑过程中随时试听与导出使用。',
};

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
    countAll: $('count-all'),
    countFavorites: $('count-favorites'),
    categoryNav: $('category-nav'),
    statusCount: $('status-count'),
    statusPath: $('status-path'),
    statusIndicator: document.querySelector('.status-indicator'),
    btnImport: $('btn-import'),
    btnRescan: $('btn-rescan'),
    btnChoose: $('btn-choose'),
    btnOpen: $('btn-open'),
    btnReset: $('btn-reset'),
    batchBar: $('batch-bar'),
    batchCount: $('batch-count'),
    selectAll: $('select-all'),
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

  els.navAll.addEventListener('click', () => setCategory('all'));
  els.navFavorites.addEventListener('click', toggleFavoritesOnly);
  els.sortSelect.addEventListener('change', () => setSortBy(els.sortSelect.value));
  els.searchInput.addEventListener('input', () => {
    setSearch(els.searchInput.value);
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
      const snapshot = await invoke('rescan_library');
      replaceLibrary(snapshot);
      toast(`曲库已刷新：${snapshot.songs.length} 首`, 'success');
    });
  });
  els.btnChoose.addEventListener('click', () => {
    void runStatusTask(els.btnChoose, chooseLibrary);
  });
  els.btnReset.addEventListener('click', () => {
    void runStatusTask(els.btnReset, async () => {
      const snapshot = await invoke('reset_library_dir');
      replaceLibrary(snapshot);
      toast('已恢复内置曲库', 'success');
    });
  });
  els.btnOpen.addEventListener('click', () => {
    void runStatusTask(els.btnOpen, async () => {
      await invoke('open_library_dir');
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

  on('library', () => {
    warningsDismissed = false;
    renderAll();
  });
  on('view', () => {
    renderSidebar();
    renderHero();
    renderResults();
    renderList();
  });
  on('search', () => {
    updateSearchUi();
    renderHero();
    renderResults();
    renderList();
  });
  on('favorites', () => {
    renderSidebar();
    renderHero();
    renderList();
  });
  on('selection', updateSelectionUi);
  on('player', renderList);

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
    label: `${category.nameZh} · ${category.nameEn}`,
  }));
}

function preferredCategory() {
  if (!state.favoritesOnly && state.category !== 'all') return state.category;
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
    const result = await invoke('import_tracks', { category: values.category });
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
    const result = await invoke('update_track', {
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

async function openMove() {
  const keys = selectedKeys();
  if (keys.length === 0) return;
  const values = await showModal({
    title: `移动 ${keys.length} 首曲目`,
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
    const result = await invoke('move_tracks', {
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
    const result = await invoke('export_tracks', { keys });
    if (!result) return;
    reportToast(result.report, `导出完成 · ${result.dir}`);
  } catch (error) {
    toast(normalizeError(error), 'error');
  }
}

async function deleteSongs(keys) {
  if (!keys || keys.length === 0) return;
  try {
    const result = await invoke('delete_tracks', { keys });
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
  const snapshot = await invoke('choose_library_dir');
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

  els.navAll.classList.toggle('is-active', state.category === 'all' && !state.favoritesOnly);
  els.navFavorites.classList.toggle('is-active', state.favoritesOnly);

  els.categoryNav.replaceChildren();
  for (const category of state.library.categories) {
    const active = state.category === category.id && !state.favoritesOnly;
    const dot = el('span', { class: 'cat-dot' });
    dot.style.backgroundColor = category.color;
    dot.style.boxShadow = `0 0 8px ${withAlpha(category.color, 0.55)}`;

    const labels = el('div', { class: 'cat-labels' }, [
      el('span', { class: 'cat-name-zh', text: category.nameZh }),
      el('span', { class: 'cat-name-en', text: category.nameEn }),
    ]);

    const button = el(
      'button',
      {
        class: `nav-item category-item${active ? ' is-active' : ''}`,
        attrs: { type: 'button' },
      },
      [dot, labels, el('span', { class: 'nav-badge', text: String(category.count) })],
    );
    button.addEventListener('click', () => setCategory(category.id));
    els.categoryNav.append(button);
  }
}

function heroInfo() {
  if (state.favoritesOnly) return FAVORITES_HERO;
  if (state.category === 'all') return ALL_HERO;
  const category = categoryById(state.category);
  if (!category) return ALL_HERO;
  return {
    title: category.title,
    labelEn: category.labelEn,
    color: category.color,
    icon: CATEGORY_ICONS[category.id] ?? 'music',
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

  const source = el('span', {
    class: 'hero-source',
    text: `${sourceLabel()} · 文件名即曲名`,
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
      el('p', {
        class: 'hero-description',
        text: info.description,
        title: info.description,
      }),
    ]),
    el('div', { class: 'hero-actions' }, [playButton, shuffleButton]),
  ]);

  const glow = el('div', { class: 'hero-glow' });
  els.hero.replaceChildren(glow, content);
}

function renderResults() {
  let title = '全部音乐素材';
  if (state.favoritesOnly) {
    title = '我的收藏夹';
  } else if (state.category !== 'all') {
    const category = categoryById(state.category);
    title = category ? `${category.nameZh}音乐 (${category.nameEn})` : '音频列表';
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
  const color = category?.color ?? '#6366f1';
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
  });
  tag.style.color = color;
  tag.style.backgroundColor = withAlpha(color, 0.1);
  tag.style.borderColor = withAlpha(color, 0.22);

  const favorite = el('button', {
    class: `action-icon-btn fav-btn${isFavorite ? ' is-favorite' : ''}`,
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
    class: 'action-icon-btn',
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

  const canDrag = state.sortBy === 'default';
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
  els.btnReset.hidden = state.library.source !== 'custom';
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