const browserDemo = !window.__TAURI__ && new URLSearchParams(window.location.search).has('demo');
const demoModifiedAtNs = Date.now() * 1_000_000;
const demoFiles = [
  { id: 'demo-1', name: '项目说明.md', path: '/本地演示/项目说明.md', size: 1860, modifiedAtNs: demoModifiedAtNs },
  { id: 'demo-2', name: '预算 2026.txt', path: '/本地演示/预算 2026.txt', size: 782, modifiedAtNs: demoModifiedAtNs - 86_400_000_000_000 },
  { id: 'demo-3', name: '产品截图.png', path: '/本地演示/产品截图.png', size: 248_200, modifiedAtNs: demoModifiedAtNs - 172_800_000_000_000 },
];

const invoke = window.__TAURI__?.core?.invoke || (async (command, args = {}) => {
  if (command === 'runtime_info') return { version: '浏览器预览', offline: true };
  if (command === 'active_library') return browserDemo ? { id: 'demo-library', name: '交互演示资料库', rootPath: '/本地演示' } : null;
  if (command === 'jobs') return [];
  if (command === 'poll_file_events') return null;
  if (command === 'files_page') return { items: browserDemo ? demoFiles : [], nextPath: null, nextId: null };
  if (command === 'library_overview') return browserDemo
    ? { totalFiles: demoFiles.length, totalBytes: demoFiles.reduce((total, file) => total + file.size, 0), latestModifiedAtNs: demoModifiedAtNs }
    : { totalFiles: 0, totalBytes: 0, latestModifiedAtNs: null };
  if (command === 'preview_file' && browserDemo) {
    const file = demoFiles.find((candidate) => candidate.id === args.fileId);
    if (!file) throw new Error('演示文件不存在');
    return { ...file, kind: file.name.endsWith('.png') ? 'metadata' : 'text', text: file.name.endsWith('.png') ? null : `这是 ${file.name} 的本地安全预览。`, truncated: false };
  }
  if (browserDemo && ['open_file', 'reveal_file', 'pause_job', 'resume_job', 'cancel_job'].includes(command)) return null;
  if (command === 'operation_history') return [];
  if (command === 'exact_duplicates') return { inputFiles: 0, quickFingerprintedFiles: 0, fullyHashedFiles: 0, skippedFiles: 0, groups: [] };
  if (command === 'search_files') return browserDemo ? demoFiles : [];
  if (command === 'smart_folders') return [];
  throw new Error('目录选择仅在归序桌面应用中可用');
});

const state = {
  library: null,
  files: [],
  nextPath: null,
  nextId: null,
  selectedId: null,
  loading: false,
  lastCompletedJobId: null,
  selectedIds: new Set(),
  currentPlan: null,
  activeSmartFolderId: null,
  pendingUndo: null,
  sortKey: 'name',
  sortDirection: 'asc',
  viewMode: 'list',
  refreshingJobs: false,
  previewRequestId: 0,
};

const elements = {
  runtime: document.querySelector('#runtime'),
  libraryName: document.querySelector('#library-name'),
  rootPath: document.querySelector('#root-path'),
  chooseFolder: document.querySelector('#choose-folder'),
  allFilesButton: document.querySelector('#all-files-button'),
  refreshFiles: document.querySelector('#refresh-files'),
  selectVisible: document.querySelector('#select-visible'),
  fileList: document.querySelector('#file-list'),
  tableHead: document.querySelector('.table-head'),
  fileCount: document.querySelector('#file-count'),
  loadMore: document.querySelector('#load-more'),
  preview: document.querySelector('#preview-panel'),
  notice: document.querySelector('#notice'),
  noticeText: document.querySelector('#notice-text'),
  taskList: document.querySelector('#task-list'),
  taskSummary: document.querySelector('#task-summary'),
  searchInput: document.querySelector('#search-input'),
  clearSearch: document.querySelector('#clear-search'),
  saveSearch: document.querySelector('#save-search'),
  smartFoldersButton: document.querySelector('#smart-folders-button'),
  smartFolders: document.querySelector('#smart-folders'),
  overviewFiles: document.querySelector('#overview-files'),
  overviewSize: document.querySelector('#overview-size'),
  overviewLatest: document.querySelector('#overview-latest'),
  selectionCount: document.querySelector('#selection-count'),
  batchBar: document.querySelector('#batch-bar'),
  clearSelection: document.querySelector('#clear-selection'),
  organizeSelected: document.querySelector('#organize-selected'),
  historyButton: document.querySelector('#history-button'),
  planDialog: document.querySelector('#plan-dialog'),
  planList: document.querySelector('#plan-list'),
  closePlan: document.querySelector('#close-plan'),
  cancelPlan: document.querySelector('#cancel-plan'),
  executePlan: document.querySelector('#execute-plan'),
  historyDialog: document.querySelector('#history-dialog'),
  historyList: document.querySelector('#history-list'),
  closeHistory: document.querySelector('#close-history'),
  duplicatesButton: document.querySelector('#duplicates-button'),
  duplicatesDialog: document.querySelector('#duplicates-dialog'),
  duplicatesSummary: document.querySelector('#duplicates-summary'),
  duplicatesList: document.querySelector('#duplicates-list'),
  closeDuplicates: document.querySelector('#close-duplicates'),
  smartFolderDialog: document.querySelector('#smart-folder-dialog'),
  smartFolderForm: document.querySelector('#smart-folder-form'),
  smartFolderName: document.querySelector('#smart-folder-name'),
  smartFolderQuery: document.querySelector('#smart-folder-query'),
  closeSmartFolder: document.querySelector('#close-smart-folder'),
  cancelSmartFolder: document.querySelector('#cancel-smart-folder'),
  undoDialog: document.querySelector('#undo-dialog'),
  closeUndo: document.querySelector('#close-undo'),
  cancelUndo: document.querySelector('#cancel-undo'),
  confirmUndo: document.querySelector('#confirm-undo'),
  sortFiles: document.querySelector('#sort-files'),
  sortDirection: document.querySelector('#sort-direction'),
  viewMode: document.querySelector('#view-mode'),
};

function setNotice(message, kind = 'info') {
  elements.noticeText.textContent = message;
  elements.notice.classList.toggle('error', kind === 'error');
  elements.notice.classList.toggle('busy', kind === 'busy');
}

function setLibrary(library) {
  state.library = library;
  elements.libraryName.textContent = library?.name || '我的资料库';
  elements.rootPath.textContent = library?.rootPath || '选择一个文件夹开始建立本地索引';
  elements.refreshFiles.disabled = !library;
  elements.searchInput.disabled = !library;
  elements.smartFoldersButton.disabled = !library;
  elements.historyButton.disabled = !library;
  elements.duplicatesButton.disabled = !library;
  state.activeSmartFolderId = null;
  state.selectedIds.clear();
  updateSelectionState();
  loadSmartFolders();
}

function updateSelectionState() {
  const count = state.selectedIds.size;
  elements.selectionCount.textContent = `已选择 ${count} 项`;
  elements.batchBar.hidden = count === 0;
  elements.organizeSelected.disabled = !state.library || count === 0;
  const visibleIds = state.files.map((file) => file.id);
  const selectedVisible = visibleIds.filter((id) => state.selectedIds.has(id)).length;
  elements.selectVisible.disabled = visibleIds.length === 0;
  elements.selectVisible.checked = visibleIds.length > 0 && selectedVisible === visibleIds.length;
  elements.selectVisible.indeterminate = selectedVisible > 0 && selectedVisible < visibleIds.length;
}

function formatBytes(bytes) {
  if (bytes < 1024) return `${bytes} B`;
  const units = ['KB', 'MB', 'GB', 'TB'];
  let value = bytes / 1024;
  let unit = units[0];
  for (let index = 1; index < units.length && value >= 1024; index += 1) {
    value /= 1024;
    unit = units[index];
  }
  return `${value >= 10 ? value.toFixed(0) : value.toFixed(1)} ${unit}`;
}

function formatDate(nanoseconds) {
  const date = new Date(Number(BigInt(nanoseconds) / 1000000n));
  return Number.isNaN(date.getTime()) ? '未知' : date.toLocaleDateString('zh-CN', { month: '2-digit', day: '2-digit', year: 'numeric' });
}

async function loadOverview() {
  if (!state.library) {
    elements.overviewFiles.textContent = '—';
    elements.overviewSize.textContent = '—';
    elements.overviewLatest.textContent = '—';
    return;
  }
  try {
    const overview = await invoke('library_overview', { libraryId: state.library.id });
    elements.overviewFiles.textContent = Number(overview.totalFiles).toLocaleString('zh-CN');
    elements.overviewSize.textContent = formatBytes(overview.totalBytes);
    elements.overviewLatest.textContent = overview.latestModifiedAtNs === null
      ? '暂无'
      : formatDate(overview.latestModifiedAtNs);
  } catch (error) {
    elements.overviewFiles.textContent = '不可用';
    elements.overviewSize.textContent = '不可用';
    elements.overviewLatest.textContent = '不可用';
  }
}

function extension(name) {
  const part = name.includes('.') ? name.split('.').pop() : 'FILE';
  return part.slice(0, 4).toUpperCase();
}

function clearPreview() {
  state.previewRequestId += 1;
  state.selectedId = null;
  elements.preview.replaceChildren();
  const placeholder = document.createElement('div');
  placeholder.className = 'preview-placeholder';
  const icon = document.createElement('span');
  icon.textContent = '⌕';
  const title = document.createElement('h2');
  title.textContent = '文件预览';
  const copy = document.createElement('p');
  copy.textContent = '选择左侧文件查看元数据与安全文本预览。';
  placeholder.append(icon, title, copy);
  elements.preview.append(placeholder);
}

function visibleFiles() {
  const direction = state.sortDirection === 'asc' ? 1 : -1;
  return [...state.files].sort((left, right) => {
    let comparison = 0;
    if (state.sortKey === 'size') comparison = Number(left.size) - Number(right.size);
    else if (state.sortKey === 'modified') comparison = Number(left.modifiedAtNs) - Number(right.modifiedAtNs);
    else comparison = left.name.localeCompare(right.name, 'zh-CN', { numeric: true, sensitivity: 'base' });
    return (comparison || left.path.localeCompare(right.path, 'zh-CN')) * direction;
  });
}

function renderFiles() {
  elements.fileList.replaceChildren();
  elements.fileList.classList.toggle('grid-mode', state.viewMode === 'grid');
  elements.tableHead.hidden = state.viewMode === 'grid';
  elements.fileCount.textContent = `${state.files.length}${state.nextPath ? '+' : ''} 项`;
  if (state.files.length === 0) {
    const empty = document.createElement('div');
    empty.className = 'empty-state';
    const icon = document.createElement('span');
    icon.className = 'empty-icon';
    icon.textContent = state.library ? '…' : '⌁';
    const title = document.createElement('h3');
    title.textContent = state.library ? '尚未发现文件' : '从一个文件夹开始';
    const copy = document.createElement('p');
    copy.textContent = state.library ? '扫描任务完成后可在这里刷新查看文件。' : '点击右上角“选择文件夹”，系统会直接打开原生目录选择器。';
    empty.append(icon, title, copy);
    elements.fileList.append(empty);
  } else {
    visibleFiles().forEach((file, index) => {
      const row = document.createElement('div');
      row.className = `file-row${file.id === state.selectedId ? ' selected' : ''}`;
      row.dataset.fileId = file.id;
      row.setAttribute('role', 'row');
      row.setAttribute('aria-selected', String(file.id === state.selectedId));

      const selector = document.createElement('input');
      selector.type = 'checkbox';
      selector.className = 'file-select';
      selector.checked = state.selectedIds.has(file.id);
      selector.setAttribute('aria-label', `选择 ${file.name}`);
      selector.tabIndex = -1;
      selector.addEventListener('click', (event) => event.stopPropagation());
      selector.addEventListener('change', () => {
        if (selector.checked) state.selectedIds.add(file.id);
        else state.selectedIds.delete(file.id);
        updateSelectionState();
      });

      const nameCell = document.createElement('button');
      nameCell.type = 'button';
      nameCell.className = 'file-name';
      nameCell.setAttribute('aria-label', `预览 ${file.name}`);
      nameCell.tabIndex = file.id === state.selectedId || (state.selectedId === null && index === 0) ? 0 : -1;
      const badge = document.createElement('span');
      badge.className = 'file-badge';
      badge.textContent = extension(file.name);
      const copy = document.createElement('span');
      copy.className = 'file-copy';
      const strong = document.createElement('strong');
      strong.textContent = file.name;
      const path = document.createElement('small');
      path.textContent = file.path;
      copy.append(strong, path);
      nameCell.append(badge, copy);
      nameCell.addEventListener('click', () => selectFile(file));
      nameCell.addEventListener('dblclick', () => openIndexedFile(file.id));

      const size = document.createElement('span');
      size.className = 'file-meta';
      size.textContent = formatBytes(file.size);
      const modified = document.createElement('span');
      modified.className = 'file-meta';
      modified.textContent = formatDate(file.modifiedAtNs);
      row.append(selector, nameCell, size, modified);
      elements.fileList.append(row);
    });
  }
  elements.loadMore.hidden = !state.nextPath;
  updateSelectionState();
}

function focusRenderedFile(fileId) {
  const row = [...elements.fileList.children].find((candidate) => candidate.dataset.fileId === fileId);
  row?.querySelector('.file-name')?.focus();
}

function navigateFiles(event) {
  const navigationKeys = ['ArrowUp', 'ArrowDown', 'ArrowLeft', 'ArrowRight', 'Home', 'End'];
  if (!navigationKeys.includes(event.key)) {
    if (event.key === 'Enter' && state.selectedId !== null) {
      event.preventDefault();
      openIndexedFile(state.selectedId);
      return;
    }
    if ((event.metaKey || event.ctrlKey) && event.key === ' ' && state.selectedId !== null) {
      event.preventDefault();
      if (state.selectedIds.has(state.selectedId)) state.selectedIds.delete(state.selectedId);
      else state.selectedIds.add(state.selectedId);
      renderFiles();
      focusRenderedFile(state.selectedId);
    }
    return;
  }
  const files = visibleFiles();
  if (files.length === 0) return;
  event.preventDefault();
  const current = Math.max(0, files.findIndex((file) => file.id === state.selectedId));
  const columns = state.viewMode === 'grid'
    ? Math.max(1, Math.floor(elements.fileList.clientWidth / 160))
    : 1;
  let targetIndex = current;
  if (event.key === 'Home') targetIndex = 0;
  else if (event.key === 'End') targetIndex = files.length - 1;
  else if (event.key === 'ArrowUp') targetIndex = current - columns;
  else if (event.key === 'ArrowDown') targetIndex = current + columns;
  else if (event.key === 'ArrowLeft') targetIndex = current - 1;
  else if (event.key === 'ArrowRight') targetIndex = current + 1;
  targetIndex = Math.max(0, Math.min(files.length - 1, targetIndex));
  const target = files[targetIndex];
  selectFile(target);
  window.requestAnimationFrame(() => focusRenderedFile(target.id));
}

async function loadFiles(reset = false) {
  if (!state.library || state.loading) return;
  state.loading = true;
  if (reset) {
    state.nextPath = null;
    state.nextId = null;
    state.files = [];
    state.selectedIds.clear();
    clearPreview();
    renderFiles();
  }
  try {
    const page = await invoke('files_page', {
      request: {
        libraryId: state.library.id,
        afterPath: state.nextPath,
        afterId: state.nextId,
        limit: 100,
      },
    });
    state.files.push(...page.items);
    state.nextPath = page.nextPath;
    state.nextId = page.nextId;
    renderFiles();
    setNotice(`已载入 ${state.files.length} 个文件。所有索引和预览均在本机完成。`);
  } catch (error) {
    setNotice(`读取文件索引失败：${String(error)}`, 'error');
  } finally {
    state.loading = false;
  }
}

let searchTimer = null;
async function runSearch(query) {
  if (!state.library) return;
  const normalized = query.trim();
  state.selectedIds.clear();
  clearPreview();
  updateSelectionState();
  elements.clearSearch.hidden = normalized.length === 0;
  elements.saveSearch.disabled = normalized.length === 0;
  if (!normalized) {
    setActiveNavigation(null);
    await loadFiles(true);
    return;
  }
  try {
    const files = await invoke('search_files', { libraryId: state.library.id, query: normalized });
    state.files = files;
    state.nextPath = null;
    state.nextId = null;
    renderFiles();
    setNotice(`搜索到 ${files.length} 个文件。`);
  } catch (error) {
    setNotice(`搜索失败：${String(error)}`, 'error');
  }
}

function setActiveNavigation(folderId) {
  state.activeSmartFolderId = folderId;
  elements.allFilesButton.classList.toggle('active', folderId === null);
  elements.smartFolders.querySelectorAll('.smart-folder').forEach((button) => {
    button.classList.toggle('active', button.dataset.id === folderId);
  });
}

async function showAllFiles() {
  elements.searchInput.value = '';
  elements.clearSearch.hidden = true;
  elements.saveSearch.disabled = true;
  setActiveNavigation(null);
  await loadFiles(true);
}

async function loadSmartFolders() {
  elements.smartFolders.replaceChildren();
  if (!state.library) return;
  try {
    const folders = await invoke('smart_folders', { libraryId: state.library.id });
    folders.forEach((folder) => {
      const row = document.createElement('div');
      row.className = 'smart-folder-row';
      const button = document.createElement('button');
      button.type = 'button';
      button.className = 'smart-folder';
      button.dataset.id = folder.id;
      button.textContent = folder.name;
      button.title = folder.query;
      button.addEventListener('click', () => {
        elements.searchInput.value = folder.query;
        setActiveNavigation(folder.id);
        runSearch(folder.query);
      });
      const remove = document.createElement('button');
      remove.type = 'button';
      remove.className = 'delete-smart-folder';
      remove.textContent = '×';
      remove.setAttribute('aria-label', `删除智能文件夹 ${folder.name}`);
      remove.addEventListener('click', () => deleteSmartFolder(folder));
      row.append(button, remove);
      elements.smartFolders.append(row);
    });
    setActiveNavigation(state.activeSmartFolderId);
  } catch (error) {
    setNotice(`读取智能文件夹失败：${String(error)}`, 'error');
  }
}

async function saveCurrentSearch() {
  const query = elements.searchInput.value.trim();
  if (!state.library || !query) return;
  openSmartFolderDialog(query);
}

function openSmartFolderDialog(query = '') {
  if (!state.library) return;
  const suggested = query.length > 24 ? `${query.slice(0, 24)}…` : query;
  elements.smartFolderName.value = suggested;
  elements.smartFolderQuery.value = query;
  elements.smartFolderDialog.showModal();
  if (suggested) {
    elements.smartFolderName.focus();
    elements.smartFolderName.select();
  } else {
    elements.smartFolderQuery.focus();
  }
}

async function submitSmartFolder(event) {
  event.preventDefault();
  const query = elements.smartFolderQuery.value.trim();
  const name = elements.smartFolderName.value.trim();
  if (!state.library || !query || !name) return;
  try {
    await invoke('save_smart_folder', { libraryId: state.library.id, name, query });
    elements.smartFolderDialog.close();
    await loadSmartFolders();
    setNotice(`已保存智能文件夹“${name}”。`);
  } catch (error) {
    setNotice(`保存失败：${String(error)}`, 'error');
  }
}

async function deleteSmartFolder(folder) {
  try {
    await invoke('delete_smart_folder', { libraryId: state.library.id, id: folder.id });
    if (state.activeSmartFolderId === folder.id) await showAllFiles();
    await loadSmartFolders();
    setNotice(`已删除智能文件夹“${folder.name}”；文件本身没有变化。`);
  } catch (error) {
    setNotice(`删除智能文件夹失败：${String(error)}`, 'error');
  }
}

async function createOrganizePlan() {
  if (!state.library || state.selectedIds.size === 0) return;
  elements.organizeSelected.disabled = true;
  setNotice('正在校验所选文件并生成只读整理预览…', 'busy');
  try {
    const plan = await invoke('create_organize_plan', {
      request: {
        libraryId: state.library.id,
        fileIds: Array.from(state.selectedIds),
      },
    });
    state.currentPlan = plan;
    renderPlan(plan);
    elements.planDialog.showModal();
    setNotice(`计划已生成，共 ${plan.items.length} 项；尚未移动任何文件。`);
  } catch (error) {
    setNotice(`无法生成整理计划：${String(error)}`, 'error');
  } finally {
    updateSelectionState();
  }
}

function renderPlan(plan) {
  elements.planList.replaceChildren();
  plan.items.forEach((item) => {
    const card = document.createElement('div');
    card.className = 'plan-item';
    const category = document.createElement('span');
    category.className = 'plan-category';
    category.textContent = item.category;
    const paths = document.createElement('span');
    paths.className = 'plan-paths';
    const source = document.createElement('strong');
    source.textContent = item.sourcePath;
    const target = document.createElement('small');
    target.textContent = `→ ${item.targetPath}`;
    paths.append(source, target);
    card.append(category, paths);
    elements.planList.append(card);
  });
}

function closePlanDialog() {
  if (elements.planDialog.open) elements.planDialog.close();
}

async function executeCurrentPlan() {
  if (!state.currentPlan) return;
  elements.executePlan.disabled = true;
  elements.cancelPlan.disabled = true;
  setNotice('正在执行已确认的整理计划，请勿关闭应用。', 'busy');
  try {
    const result = await invoke('execute_organize_plan', { planId: state.currentPlan.id });
    closePlanDialog();
    state.currentPlan = null;
    state.selectedIds.clear();
    state.files = [];
    state.nextPath = null;
    state.nextId = null;
    clearPreview();
    renderFiles();
    await invoke('rescan_library', { libraryId: state.library.id });
    await refreshJobs();
    setNotice(`整理完成：${result.completedItems}/${result.totalItems} 个文件；正在同步新位置，操作已写入历史。`, 'busy');
  } catch (error) {
    setNotice(`整理未完成：${String(error)}。请在操作历史中查看逐项状态。`, 'error');
    await loadHistory(false);
  } finally {
    elements.executePlan.disabled = false;
    elements.cancelPlan.disabled = false;
  }
}

async function loadHistory(show = true) {
  if (!state.library) return;
  try {
    const operations = await invoke('operation_history', { libraryId: state.library.id });
    renderHistory(operations);
    if (show && !elements.historyDialog.open) elements.historyDialog.showModal();
  } catch (error) {
    setNotice(`读取操作历史失败：${String(error)}`, 'error');
  }
}

function renderHistory(operations) {
  elements.historyList.replaceChildren();
  if (operations.length === 0) {
    const empty = document.createElement('div');
    empty.className = 'empty-state';
    empty.textContent = '尚无已执行的整理操作。';
    elements.historyList.append(empty);
    return;
  }
  const labels = {
    completed: '已完成', rolled_back: '已撤销', failed: '失败',
    recovery_needed: '需要处理', executing: '执行中', rollback_pending: '撤销中',
  };
  operations.forEach((operation) => {
    const card = document.createElement('article');
    card.className = 'history-item';
    const head = document.createElement('div');
    head.className = 'history-item-head';
    const title = document.createElement('strong');
    title.textContent = `${new Date(operation.createdAtMs).toLocaleString('zh-CN')} · ${operation.items.length} 项`;
    const status = document.createElement('span');
    status.className = `status-pill ${operation.status}`;
    status.textContent = labels[operation.status] || operation.status;
    head.append(title, status);
    const detail = document.createElement('p');
    detail.textContent = operation.errorMessage
      || operation.items.map((item) => `${item.sourcePath} → ${item.targetPath}`).join('\n');
    card.append(head, detail);
    if (operation.status === 'completed') {
      const actions = document.createElement('div');
      actions.className = 'history-actions';
      const undo = document.createElement('button');
      undo.type = 'button';
      undo.className = 'quiet-button';
      undo.textContent = '撤销此次操作';
      undo.addEventListener('click', () => undoHistoryOperation(operation.id, undo));
      actions.append(undo);
      card.append(actions);
    }
    elements.historyList.append(card);
  });
}

async function undoHistoryOperation(operationId, button) {
  state.pendingUndo = { operationId, button };
  elements.undoDialog.showModal();
}

async function confirmUndoOperation() {
  const pending = state.pendingUndo;
  if (!pending) return;
  elements.confirmUndo.disabled = true;
  pending.button.disabled = true;
  setNotice('正在验证并撤销操作…', 'busy');
  try {
    const result = await invoke('undo_operation', { operationId: pending.operationId });
    elements.undoDialog.close();
    state.files = [];
    state.nextPath = null;
    state.nextId = null;
    clearPreview();
    renderFiles();
    await loadHistory(false);
    await invoke('rescan_library', { libraryId: state.library.id });
    await refreshJobs();
    setNotice(`已安全撤销 ${result.completedItems}/${result.totalItems} 个文件；正在同步原位置。`, 'busy');
  } catch (error) {
    setNotice(`撤销失败：${String(error)}`, 'error');
    await loadHistory(false);
  } finally {
    pending.button.disabled = false;
    elements.confirmUndo.disabled = false;
    state.pendingUndo = null;
  }
}

async function loadExactDuplicates() {
  if (!state.library) return;
  elements.duplicatesButton.disabled = true;
  setNotice('正在本机分析精确重复文件；只有候选文件才会进行完整哈希。', 'busy');
  try {
    const report = await invoke('exact_duplicates', { libraryId: state.library.id });
    renderDuplicates(report);
    elements.duplicatesDialog.showModal();
    setNotice(`重复分析完成：发现 ${report.groups.length} 组精确重复文件。`);
  } catch (error) {
    setNotice(`重复文件分析失败：${String(error)}`, 'error');
  } finally {
    elements.duplicatesButton.disabled = !state.library;
  }
}

function renderDuplicates(report) {
  elements.duplicatesSummary.textContent = `扫描 ${report.inputFiles} 个文件；快速指纹 ${report.quickFingerprintedFiles} 个；完整哈希 ${report.fullyHashedFiles} 个；跳过 ${report.skippedFiles} 个。结果仅供选择，不会自动删除。`;
  elements.duplicatesList.replaceChildren();
  if (report.groups.length === 0) {
    const empty = document.createElement('div');
    empty.className = 'empty-state';
    empty.textContent = '没有发现内容完全相同的文件。';
    elements.duplicatesList.append(empty);
    return;
  }
  report.groups.forEach((group) => {
    const card = document.createElement('article');
    card.className = 'history-item';
    const head = document.createElement('div');
    head.className = 'history-item-head';
    const title = document.createElement('strong');
    title.textContent = `${group.paths.length} 个相同文件 · 每个 ${formatBytes(group.size)}`;
    const savings = document.createElement('span');
    savings.className = 'status-pill';
    savings.textContent = `可节省 ${formatBytes(group.potentialSavings)}`;
    head.append(title, savings);
    const paths = document.createElement('div');
    paths.className = 'duplicate-paths';
    group.paths.forEach((path) => {
      const row = document.createElement('div');
      row.className = 'duplicate-path';
      if (path === group.suggestedKeep) {
        const keep = document.createElement('strong');
        keep.textContent = '建议保留';
        row.append(keep);
      }
      const value = document.createElement('span');
      value.textContent = path;
      value.title = path;
      row.append(value);
      paths.append(row);
    });
    card.append(head, paths);
    elements.duplicatesList.append(card);
  });
}

async function selectFile(file) {
  const requestId = state.previewRequestId + 1;
  state.previewRequestId = requestId;
  state.selectedId = file.id;
  renderFiles();
  elements.preview.replaceChildren();
  const loading = document.createElement('div');
  loading.className = 'preview-placeholder';
  loading.textContent = '正在读取安全预览…';
  elements.preview.append(loading);
  try {
    const preview = await invoke('preview_file', { libraryId: state.library.id, fileId: file.id });
    if (requestId !== state.previewRequestId || state.selectedId !== file.id) return;
    renderPreview(preview);
  } catch (error) {
    if (requestId !== state.previewRequestId || state.selectedId !== file.id) return;
    elements.preview.replaceChildren();
    const message = document.createElement('div');
    message.className = 'preview-placeholder';
    message.textContent = `无法预览：${String(error)}`;
    elements.preview.append(message);
  }
}

async function openIndexedFile(fileId) {
  if (!state.library) return;
  try {
    await invoke('open_file', { libraryId: state.library.id, fileId });
    setNotice('已交给系统默认应用打开。');
  } catch (error) {
    setNotice(`无法打开文件：${String(error)}`, 'error');
  }
}

async function revealIndexedFile(fileId) {
  if (!state.library) return;
  try {
    await invoke('reveal_file', { libraryId: state.library.id, fileId });
    setNotice('已在系统文件管理器中定位文件。');
  } catch (error) {
    setNotice(`无法定位文件：${String(error)}`, 'error');
  }
}

function renderPreview(preview) {
  elements.preview.replaceChildren();
  const content = document.createElement('div');
  content.className = 'preview-content';
  const type = document.createElement('div');
  type.className = 'preview-type';
  type.textContent = extension(preview.name);
  const title = document.createElement('h2');
  title.textContent = preview.name;
  const path = document.createElement('p');
  path.className = 'preview-path';
  path.textContent = preview.path;
  const actions = document.createElement('div');
  actions.className = 'preview-actions';
  const open = document.createElement('button');
  open.type = 'button';
  open.className = 'primary-button';
  open.textContent = '打开文件';
  open.addEventListener('click', () => openIndexedFile(preview.id));
  const reveal = document.createElement('button');
  reveal.type = 'button';
  reveal.className = 'quiet-button';
  reveal.textContent = '在文件管理器中显示';
  reveal.addEventListener('click', () => revealIndexedFile(preview.id));
  actions.append(open, reveal);
  const facts = document.createElement('div');
  facts.className = 'preview-facts';
  facts.append(fact('大小', formatBytes(preview.size)), fact('预览模式', preview.kind === 'text' ? '受限文本' : '仅元数据'));
  content.append(type, title, path, actions, facts);
  if (preview.text !== null) {
    const text = document.createElement('pre');
    text.className = 'text-preview';
    text.textContent = preview.text;
    content.append(text);
    if (preview.truncated) {
      const note = document.createElement('p');
      note.className = 'preview-note';
      note.textContent = '为控制内存占用，预览已限制为前 256 KB。';
      content.append(note);
    }
  } else {
    const note = document.createElement('p');
    note.className = 'preview-note';
    note.textContent = '当前类型仅展示安全元数据；应用不会把文件内容发送到网络。';
    content.append(note);
  }
  elements.preview.append(content);
}

function fact(label, value) {
  const box = document.createElement('div');
  box.className = 'fact';
  const small = document.createElement('small');
  small.textContent = label;
  const strong = document.createElement('strong');
  strong.textContent = value;
  box.append(small, strong);
  return box;
}

async function chooseFolder() {
  elements.chooseFolder.disabled = true;
  setNotice('请在系统窗口中选择需要整理的文件夹。', 'busy');
  try {
    const library = await invoke('select_library_folder');
    if (!library) {
      setNotice('已取消选择，现有资料库保持不变。');
      return;
    }
    setLibrary(library);
    state.files = [];
    state.nextPath = null;
    state.nextId = null;
    state.selectedId = null;
    renderFiles();
    setNotice('文件夹已授权，后台正在建立本地索引。', 'busy');
    await refreshJobs();
    window.setTimeout(() => loadFiles(true), 600);
  } catch (error) {
    setNotice(`选择文件夹失败：${String(error)}`, 'error');
  } finally {
    elements.chooseFolder.disabled = false;
  }
}

async function rescanLibrary() {
  if (!state.library) return;
  elements.refreshFiles.disabled = true;
  setNotice('已请求重新扫描资料库，后台将对账文件变化。', 'busy');
  try {
    await invoke('rescan_library', { libraryId: state.library.id });
    await refreshJobs();
  } catch (error) {
    setNotice(`无法启动重新扫描：${String(error)}`, 'error');
  } finally {
    elements.refreshFiles.disabled = false;
  }
}

async function controlJob(command, jobId, button) {
  button.disabled = true;
  try {
    await invoke(command, { jobId });
    await refreshJobs();
  } catch (error) {
    setNotice(`任务控制失败：${String(error)}`, 'error');
    button.disabled = false;
  }
}

function jobAction(label, command, jobId) {
  const button = document.createElement('button');
  button.className = 'task-action';
  button.type = 'button';
  button.textContent = label;
  button.addEventListener('click', () => controlJob(command, jobId, button));
  return button;
}

async function refreshJobs() {
  if (state.refreshingJobs) return;
  state.refreshingJobs = true;
  try {
    await invoke('poll_file_events');
    const jobs = await invoke('jobs');
    elements.taskList.replaceChildren();
    elements.taskSummary.textContent = jobs.length ? `${jobs.length} 个最近任务` : '暂无任务';
    jobs.slice(0, 5).forEach((job) => {
      const item = document.createElement('div');
      item.className = `task ${job.status}`;
      const dot = document.createElement('span');
      dot.className = 'task-status';
      const copy = document.createElement('span');
      copy.className = 'task-copy';
      const name = document.createElement('strong');
      const jobNames = { library_scan: '资料库扫描', snapshot_reconcile: '文件变更对账' };
      name.textContent = jobNames[job.kind] || job.kind;
      const status = document.createElement('small');
      const labels = {
        queued: '等待执行',
        running: '正在扫描',
        pause_requested: '正在暂停',
        paused: '已暂停',
        cancel_requested: '正在取消',
        completed: '已完成',
        failed: '失败',
        cancelled: '已取消',
      };
      const progress = job.progressTotal === null
        ? ` · 已处理 ${Number(job.progressCurrent).toLocaleString('zh-CN')} 个`
        : ` · ${job.progressCurrent}/${job.progressTotal}`;
      status.textContent = `${labels[job.status] || job.status}${progress}`;
      copy.append(name, status);
      item.append(dot, copy);
      const actions = document.createElement('span');
      actions.className = 'task-actions';
      if (job.status === 'running') {
        actions.append(jobAction('暂停', 'pause_job', job.id), jobAction('取消', 'cancel_job', job.id));
      } else if (job.status === 'queued') {
        actions.append(jobAction('取消', 'cancel_job', job.id));
      } else if (job.status === 'paused') {
        actions.append(jobAction('继续', 'resume_job', job.id), jobAction('取消', 'cancel_job', job.id));
      }
      if (actions.childElementCount > 0) item.append(actions);
      elements.taskList.append(item);
    });
    if (jobs.some((job) => ['running', 'queued', 'pause_requested', 'cancel_requested'].includes(job.status))) {
      setNotice('后台正在建立文件索引，完成后列表会自动刷新。', 'busy');
    } else if (state.library) {
      const completed = jobs.find((job) =>
        (job.kind === 'library_scan' || job.kind === 'snapshot_reconcile')
        && job.status === 'completed');
      if (completed && completed.id !== state.lastCompletedJobId) {
        state.lastCompletedJobId = completed.id;
        await loadFiles(true);
        await loadOverview();
      } else if (elements.notice.classList.contains('busy')) {
        setNotice('后台任务已完成，索引已与文件夹同步。');
      }
    }
  } catch (error) {
    elements.taskSummary.textContent = '任务状态不可用';
  } finally {
    state.refreshingJobs = false;
  }
}

async function subscribeToJobEvents() {
  const listen = window.__TAURI__?.event?.listen;
  if (!listen) return;
  let refreshTimer = null;
  await listen('job-status-changed', () => {
    if (refreshTimer !== null) window.clearTimeout(refreshTimer);
    refreshTimer = window.setTimeout(() => {
      refreshTimer = null;
      refreshJobs();
    }, 100);
  });
}

async function connect() {
  try {
    const [runtime, library] = await Promise.all([invoke('runtime_info'), invoke('active_library')]);
    elements.runtime.textContent = `${runtime.version} · ${runtime.offline ? '离线' : '联网'}`;
    setLibrary(library);
    if (library) {
      await Promise.all([loadFiles(true), loadOverview()]);
    } else {
      await loadOverview();
    }
    await refreshJobs();
    await subscribeToJobEvents();
  } catch (error) {
    elements.runtime.textContent = '核心连接失败';
    setNotice(`本地核心连接失败：${String(error)}`, 'error');
  }
}

elements.chooseFolder.addEventListener('click', chooseFolder);
elements.allFilesButton.addEventListener('click', showAllFiles);
elements.smartFoldersButton.addEventListener('click', () => openSmartFolderDialog());
elements.refreshFiles.addEventListener('click', rescanLibrary);
elements.selectVisible.addEventListener('change', () => {
  state.files.forEach((file) => {
    if (elements.selectVisible.checked) state.selectedIds.add(file.id);
    else state.selectedIds.delete(file.id);
  });
  renderFiles();
});
elements.loadMore.addEventListener('click', () => loadFiles(false));
elements.fileList.addEventListener('keydown', navigateFiles);
elements.sortFiles.addEventListener('change', () => {
  state.sortKey = elements.sortFiles.value;
  renderFiles();
});
elements.sortDirection.addEventListener('click', () => {
  state.sortDirection = state.sortDirection === 'asc' ? 'desc' : 'asc';
  const ascending = state.sortDirection === 'asc';
  elements.sortDirection.textContent = ascending ? '↑' : '↓';
  elements.sortDirection.setAttribute('aria-label', ascending ? '当前升序，点击切换为降序' : '当前降序，点击切换为升序');
  renderFiles();
});
elements.viewMode.addEventListener('click', () => {
  state.viewMode = state.viewMode === 'list' ? 'grid' : 'list';
  const grid = state.viewMode === 'grid';
  elements.viewMode.textContent = grid ? '☷' : '▦';
  elements.viewMode.setAttribute('aria-label', grid ? '切换为列表视图' : '切换为网格视图');
  renderFiles();
  if (state.selectedId !== null) focusRenderedFile(state.selectedId);
});
elements.searchInput.addEventListener('input', () => {
  setActiveNavigation(null);
  window.clearTimeout(searchTimer);
  searchTimer = window.setTimeout(() => runSearch(elements.searchInput.value), 220);
});
elements.clearSearch.addEventListener('click', () => {
  elements.searchInput.value = '';
  runSearch('');
});
elements.saveSearch.addEventListener('click', saveCurrentSearch);
elements.organizeSelected.addEventListener('click', createOrganizePlan);
elements.clearSelection.addEventListener('click', () => {
  state.selectedIds.clear();
  renderFiles();
});
elements.executePlan.addEventListener('click', executeCurrentPlan);
elements.cancelPlan.addEventListener('click', closePlanDialog);
elements.closePlan.addEventListener('click', closePlanDialog);
elements.historyButton.addEventListener('click', () => loadHistory(true));
elements.closeHistory.addEventListener('click', () => elements.historyDialog.close());
elements.duplicatesButton.addEventListener('click', loadExactDuplicates);
elements.closeDuplicates.addEventListener('click', () => elements.duplicatesDialog.close());
elements.smartFolderForm.addEventListener('submit', submitSmartFolder);
elements.closeSmartFolder.addEventListener('click', () => elements.smartFolderDialog.close());
elements.cancelSmartFolder.addEventListener('click', () => elements.smartFolderDialog.close());
elements.confirmUndo.addEventListener('click', confirmUndoOperation);
elements.closeUndo.addEventListener('click', () => elements.undoDialog.close());
elements.cancelUndo.addEventListener('click', () => elements.undoDialog.close());
elements.undoDialog.addEventListener('close', () => {
  if (!elements.confirmUndo.disabled) state.pendingUndo = null;
});
elements.planDialog.addEventListener('close', () => {
  if (!elements.executePlan.disabled) state.currentPlan = null;
});

connect();
window.setInterval(refreshJobs, 2000);
