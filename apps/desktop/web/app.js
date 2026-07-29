const inDesktopApp = Boolean(window.__TAURI__?.core?.invoke);
const browserDemo = !inDesktopApp && new URLSearchParams(window.location.search).has('demo');
const demoModifiedAtNs = Date.now() * 1_000_000;
const demoFiles = [
  { id: 'demo-1', name: '项目说明.md', path: '/本地演示/项目说明.md', size: 1860, modifiedAtNs: demoModifiedAtNs },
  { id: 'demo-2', name: '预算 2026.txt', path: '/本地演示/预算 2026.txt', size: 782, modifiedAtNs: demoModifiedAtNs - 86_400_000_000_000 },
  { id: 'demo-3', name: '产品截图.png', path: '/本地演示/产品截图.png', size: 248_200, modifiedAtNs: demoModifiedAtNs - 172_800_000_000_000 },
  { id: 'demo-4', name: '项目说明 copy.md', path: '/本地演示/备份/项目说明 copy.md', size: 1860, modifiedAtNs: demoModifiedAtNs - 3_600_000_000_000 },
  { id: 'demo-5', name: '产品截图-压缩.webp', path: '/本地演示/导出/产品截图-压缩.webp', size: 96_400, modifiedAtNs: demoModifiedAtNs - 7_200_000_000_000 },
];
const demoSmartFolders = [
  { id: 'demo-smart-1', name: '最近的大型 PDF 合同与归档文件', query: '合同 ext:pdf size:>10MB' },
];
let demoHashCacheFiles = 2;
let demoJobs = [];
const demoSimilarityResults = new Map();
const demoPlans = new Map();
const demoOperations = [];

function parseDemoSize(value) {
  const match = value.match(/^(\d+(?:\.\d+)?)(B|KB|MB|GB|KIB|MIB|GIB)?$/i);
  if (!match) return null;
  const units = { B: 1, KB: 1_000, MB: 1_000_000, GB: 1_000_000_000, KIB: 1024, MIB: 1024 ** 2, GIB: 1024 ** 3 };
  return Number(match[1]) * (units[(match[2] || 'B').toUpperCase()] || 1);
}

function searchDemoFiles(query) {
  const filters = query.trim().split(/\s+/).filter(Boolean);
  return demoFiles.filter((file) => filters.every((filter) => {
    const extensionFilter = filter.match(/^ext:([\p{L}\p{N}_+-]+)$/iu);
    if (extensionFilter) return file.name.toLocaleLowerCase().endsWith(`.${extensionFilter[1].toLocaleLowerCase()}`);
    const sizeFilter = filter.match(/^size:(>=|<=|>|<|=)?(.+)$/i);
    if (sizeFilter) {
      const expected = parseDemoSize(sizeFilter[2]);
      if (expected === null) return false;
      const operator = sizeFilter[1] || '=';
      if (operator === '>') return file.size > expected;
      if (operator === '>=') return file.size >= expected;
      if (operator === '<') return file.size < expected;
      if (operator === '<=') return file.size <= expected;
      return file.size === expected;
    }
    const needle = filter.toLocaleLowerCase();
    return file.name.toLocaleLowerCase().includes(needle) || file.path.toLocaleLowerCase().includes(needle);
  }));
}

function reducedMotionRequested() {
  return document.body.classList.contains('reduce-motion')
    || window.matchMedia('(prefers-reduced-motion: reduce)').matches;
}

function transitionUi(update, scope = 'content') {
  if (reducedMotionRequested() || typeof document.startViewTransition !== 'function') {
    update();
    return null;
  }
  document.documentElement.dataset.transitionScope = scope;
  const transition = document.startViewTransition(update);
  transition.finished.finally(() => {
    if (document.documentElement.dataset.transitionScope === scope) {
      delete document.documentElement.dataset.transitionScope;
    }
  });
  return transition;
}

function createLineIcon(markup) {
  const icon = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  icon.setAttribute('class', 'ui-icon');
  icon.setAttribute('viewBox', '0 0 24 24');
  icon.setAttribute('aria-hidden', 'true');
  icon.innerHTML = markup;
  return icon;
}

// ── Phase 2：弹窗动画 ──
function openModal(dialog) {
  if (dialog.open) return;
  dialog.showModal();
  dialog.addEventListener('cancel', (event) => { event.preventDefault(); closeModal(dialog); }, { once: true });
}
function closeModal(dialog) {
  if (!dialog.open) return;
  if (reducedMotionRequested()) {
    dialog.close();
    return;
  }
  dialog.classList.add('modal-closing');
  let fallbackTimer;
  const onEnd = () => {
    window.clearTimeout(fallbackTimer);
    dialog.removeEventListener('animationend', onEnd);
    dialog.classList.remove('modal-closing');
    if (dialog.open) dialog.close();
  };
  dialog.addEventListener('animationend', onEnd, { once: true });
  fallbackTimer = window.setTimeout(onEnd, 260);
}
function autoCloseNotice(delayMs = 5000) {
  if (state._noticeTimer) clearTimeout(state._noticeTimer);
  if (state._noticeHideTimer) {
    clearTimeout(state._noticeHideTimer);
    state._noticeHideTimer = null;
  }
  state._noticeTimer = setTimeout(() => {
    state._noticeTimer = null;
    const notice = elements.notice;
    if (notice && !notice.classList.contains('busy') && !notice.classList.contains('error')) {
      notice.classList.add('notice-dismissed');
      state._noticeHideTimer = setTimeout(() => {
        notice.hidden = true;
        state._noticeHideTimer = null;
      }, 220);
    }
  }, delayMs);
}

function setSortIcon(ascending) {
  elements.sortDirection.replaceChildren(createLineIcon('<path d="M12 19V5M6 11l6-6 6 6"/>'));
  elements.sortDirection.classList.toggle('descending', !ascending);
}

function setViewIcon(grid) {
  const markup = grid
    ? '<path d="M8 6h13M8 12h13M8 18h13"/><circle cx="4" cy="6" r="1"/><circle cx="4" cy="12" r="1"/><circle cx="4" cy="18" r="1"/>'
    : '<rect x="3" y="3" width="7" height="7" rx="1"/><rect x="14" y="3" width="7" height="7" rx="1"/><rect x="3" y="14" width="7" height="7" rx="1"/><rect x="14" y="14" width="7" height="7" rx="1"/>';
  elements.viewMode.replaceChildren(createLineIcon(markup));
}

const demoDuplicateReport = {
  inputFiles: demoFiles.length, quickFingerprintedFiles: 0, fullyHashedFiles: 0,
  quickCacheHits: demoHashCacheFiles, fullCacheHits: demoHashCacheFiles, skippedFiles: 0,
  groups: [{
    id: 'demo-duplicate-group', size: 1860, potentialSavings: 1860,
    paths: ['/本地演示/项目说明.md', '/本地演示/备份/项目说明 copy.md'],
    suggestedKeep: '/本地演示/项目说明.md',
    files: [
      { fileId: 'demo-1', path: '/本地演示/项目说明.md', retentionScore: 75, reasons: ['路径层级较浅', '名称无副本标记'], suggestedKeep: true },
      { fileId: 'demo-4', path: '/本地演示/备份/项目说明 copy.md', retentionScore: 50, reasons: ['同组中最近修改', '名称包含副本标记'], suggestedKeep: false },
    ],
  }],
};

const invoke = window.__TAURI__?.core?.invoke || (async (command, args = {}) => {
  if (command === 'runtime_info') return { version: '浏览器预览', offline: true };
  if (command === 'app_settings') return { restoreLastLibrary: true, defaultViewMode: 'list', defaultSortKey: 'name', defaultSortDirection: 'asc', pageSize: 100, refreshIntervalMs: 2000, showFullPaths: true, density: 'comfortable', showOverview: true, showTaskCenter: true, fileSizeUnit: 'binary', dateFormat: 'locale', reduceMotion: false };
  if (command === 'save_app_settings') return args.settings;
  if (command === 'local_data_status') return { indexedFiles: demoFiles.length, indexedBytes: demoFiles.reduce((total, file) => total + file.size, 0), cachedFiles: demoHashCacheFiles, fullyHashedFiles: demoHashCacheFiles };
  if (command === 'clear_hash_cache') {
    const removed = demoHashCacheFiles;
    demoHashCacheFiles = 0;
    return removed;
  }
  if (command === 'active_library' || command === 'select_library_folder') return browserDemo ? { id: 'demo-library', name: '交互演示资料库', rootPath: '/本地演示' } : null;
  if (command === 'jobs') return demoJobs;
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
  if (command === 'rescan_library' && browserDemo) {
    const id = `demo-scan-${Date.now()}`;
    demoJobs = [{ id, kind: 'library_scan', status: 'completed', progressCurrent: demoFiles.length, progressTotal: demoFiles.length, updatedAtMs: Date.now() }, ...demoJobs];
    return id;
  }
  if (browserDemo && ['open_file', 'reveal_file'].includes(command)) return null;
  if (browserDemo && ['pause_job', 'resume_job', 'cancel_job'].includes(command)) {
    const job = demoJobs.find((candidate) => candidate.id === args.jobId);
    if (job) job.status = command === 'pause_job' ? 'paused' : command === 'resume_job' ? 'running' : 'cancelled';
    return null;
  }
  if (command === 'operation_history') return browserDemo ? demoOperations : [];
  if (command === 'undo_operation' && browserDemo) {
    const operation = demoOperations.find((candidate) => candidate.id === args.operationId);
    if (!operation) throw new Error('演示操作不存在');
    operation.status = 'rolled_back';
    return { operationId: operation.id, completedItems: operation.items.length, totalItems: operation.items.length };
  }
  if (command === 'exact_duplicates' || command === 'exact_duplicate_result') return demoDuplicateReport;
  if (command === 'start_exact_duplicate_analysis' && browserDemo) {
    const id = `demo-duplicates-${Date.now()}`;
    demoJobs = [{ id, kind: 'exact_duplicate_analysis', status: 'completed', progressCurrent: demoFiles.length * 2, progressTotal: demoFiles.length * 2, updatedAtMs: Date.now() }];
    return id;
  }
  if (command === 'start_similarity_analysis' && browserDemo) {
    const id = `demo-similarity-${args.contentKind}-${Date.now()}`;
    const report = args.contentKind === 'image'
      ? { contentKind: 'image', inputFiles: 2, computedFeatures: 1, cacheHits: 1, skippedFiles: 0, pairs: [{ leftFileId: 'demo-3', leftPath: '/本地演示/产品截图.png', rightFileId: 'demo-5', rightPath: '/本地演示/导出/产品截图-压缩.webp', hammingDistance: 3, similarity: 0.953125 }] }
      : { contentKind: 'text', inputFiles: 3, computedFeatures: 1, cacheHits: 2, skippedFiles: 0, pairs: [{ leftFileId: 'demo-1', leftPath: '/本地演示/项目说明.md', rightFileId: 'demo-4', rightPath: '/本地演示/备份/项目说明 copy.md', hammingDistance: 4, similarity: 0.9375 }] };
    demoSimilarityResults.set(id, report);
    demoJobs = [{ id, kind: args.contentKind === 'image' ? 'similar_image_analysis' : 'similar_text_analysis', status: 'completed', progressCurrent: demoFiles.length, progressTotal: demoFiles.length, updatedAtMs: Date.now() }, ...demoJobs];
    return id;
  }
  if (command === 'similarity_result' && browserDemo) return demoSimilarityResults.get(args.jobId) || null;
  if (command === 'similar_texts' && browserDemo) return {
    contentKind: 'text',
    inputFiles: 3, computedFeatures: 1, cacheHits: 2, skippedFiles: 0,
    pairs: [{ leftFileId: 'demo-1', leftPath: '/本地演示/项目说明.md', rightFileId: 'demo-4', rightPath: '/本地演示/备份/项目说明 copy.md', hammingDistance: 4, similarity: 0.9375 }],
  };
  if (command === 'similar_images' && browserDemo) return {
    contentKind: 'image',
    inputFiles: 2, computedFeatures: 1, cacheHits: 1, skippedFiles: 0,
    pairs: [{ leftFileId: 'demo-3', leftPath: '/本地演示/产品截图.png', rightFileId: 'demo-5', rightPath: '/本地演示/导出/产品截图-压缩.webp', hammingDistance: 3, similarity: 0.953125 }],
  };
  if (command === 'search_files') return browserDemo ? searchDemoFiles(args.query || '') : [];
  if (command === 'smart_folders') return browserDemo ? demoSmartFolders : [];
  if (command === 'save_smart_folder' && browserDemo) {
    demoSmartFolders.push({ id: `demo-smart-${demoSmartFolders.length + 1}`, name: args.name, query: args.query });
    return null;
  }
  if (command === 'delete_smart_folder' && browserDemo) {
    const index = demoSmartFolders.findIndex((folder) => folder.id === args.id);
    if (index >= 0) demoSmartFolders.splice(index, 1);
    return true;
  }
  if (command === 'create_organize_plan' && browserDemo) {
    const items = args.request.fileIds.map((id, ordinal) => {
      const file = demoFiles.find((candidate) => candidate.id === id);
      return { ordinal, fileId: id, sourcePath: file.path, targetPath: `/本地演示/归序整理/文档/${file.name}`, category: '文档' };
    });
    const plan = { id: `demo-organize-plan-${Date.now()}`, operationKind: 'organize', expiresAtMs: Date.now() + 900000, items };
    demoPlans.set(plan.id, plan);
    return plan;
  }
  if (command === 'create_rename_plan' && browserDemo) {
    const items = args.request.items.map((request, ordinal) => {
      const file = demoFiles.find((candidate) => candidate.id === request.fileId);
      const parent = file.path.slice(0, file.path.lastIndexOf('/'));
      return { ordinal, fileId: request.fileId, sourcePath: file.path, targetPath: `${parent}/${request.newName}`, category: '重命名' };
    });
    const plan = { id: `demo-rename-plan-${Date.now()}`, operationKind: 'rename', expiresAtMs: Date.now() + 900000, items };
    demoPlans.set(plan.id, plan);
    return plan;
  }
  if (command === 'create_copy_plan' && browserDemo) {
    const items = args.request.fileIds.map((id, ordinal) => {
      const file = demoFiles.find((candidate) => candidate.id === id);
      return { ordinal, fileId: id, sourcePath: file.path, targetPath: `/本地演示/安全副本/${file.name}`, category: '安全复制' };
    });
    const plan = { id: `demo-copy-plan-${Date.now()}`, operationKind: 'copy', expiresAtMs: Date.now() + 900000, items };
    demoPlans.set(plan.id, plan);
    return plan;
  }
  if (command === 'create_move_plan' && browserDemo) {
    const items = args.request.fileIds.map((id, ordinal) => {
      const file = demoFiles.find((candidate) => candidate.id === id);
      return { ordinal, fileId: id, sourcePath: file.path, targetPath: `/本地演示/移动目标/${file.name}`, category: '安全移动' };
    });
    const plan = { id: `demo-move-plan-${Date.now()}`, operationKind: 'move', expiresAtMs: Date.now() + 900000, items };
    demoPlans.set(plan.id, plan);
    return plan;
  }
  if (command === 'create_trash_plan' && browserDemo) {
    const items = args.request.fileIds.map((id, ordinal) => {
      const file = demoFiles.find((candidate) => candidate.id === id);
      return { ordinal, fileId: id, sourcePath: file.path, targetPath: `/本地演示/.guixu-trash/demo-trash-plan/${file.name}`, category: '可恢复删除' };
    });
    const plan = { id: `demo-trash-plan-${Date.now()}`, operationKind: 'trash', expiresAtMs: Date.now() + 900000, items };
    demoPlans.set(plan.id, plan);
    return plan;
  }
  if (browserDemo && ['execute_organize_plan', 'execute_rename_plan', 'execute_copy_plan', 'execute_move_plan', 'execute_trash_plan'].includes(command)) {
    const plan = demoPlans.get(args.planId);
    if (!plan) throw new Error('演示计划已失效');
    const operation = { id: `demo-operation-${Date.now()}`, operationKind: plan.operationKind, status: 'completed', createdAtMs: Date.now(), errorMessage: null, items: plan.items };
    demoOperations.unshift(operation);
    return { operationId: operation.id, completedItems: plan.items.length, totalItems: plan.items.length };
  }
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
  activeDuplicateJobId: null,
  lastDuplicateResultJobId: null,
  activeSimilarityJobId: null,
  activeSimilarityKind: 'text',
  selectedIds: new Set(),
  duplicateSelectedIds: new Set(),
  currentPlan: null,
  activeSmartFolderId: null,
  pendingUndo: null,
  sortKey: 'name',
  sortDirection: 'asc',
  viewMode: 'list',
  settings: { restoreLastLibrary: true, defaultViewMode: 'list', defaultSortKey: 'name', defaultSortDirection: 'asc', pageSize: 100, refreshIntervalMs: 2000, showFullPaths: true, density: 'comfortable', showOverview: true, showTaskCenter: true, fileSizeUnit: 'binary', dateFormat: 'locale', reduceMotion: false },
  refreshTimer: null,
  refreshingJobs: false,
  previewRequestId: 0,
  listRequestId: 0,         // P3-H8：列表/搜索请求令牌，防竞态
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
  noticeIcon: document.querySelector('.notice-icon'),
  noticeText: document.querySelector('#notice-text'),
  taskList: document.querySelector('#task-list'),
  taskSummary: document.querySelector('#task-summary'),
  searchInput: document.querySelector('#search-input'),
  clearSearch: document.querySelector('#clear-search'),
  saveSearch: document.querySelector('#save-search'),
  smartFoldersButton: document.querySelector('#smart-folders-button'),
  smartFolders: document.querySelector('#smart-folders'),
  settingsButton: document.querySelector('#settings-button'),
  largeFilesButton: document.querySelector('#large-files-button'),
  trashHistoryButton: document.querySelector('#trash-history-button'),
  overview: document.querySelector('.overview'),
  taskCenter: document.querySelector('.task-center'),
  overviewFiles: document.querySelector('#overview-files'),
  overviewSize: document.querySelector('#overview-size'),
  overviewLatest: document.querySelector('#overview-latest'),
  selectionCount: document.querySelector('#selection-count'),
  batchBar: document.querySelector('#batch-bar'),
  clearSelection: document.querySelector('#clear-selection'),
  renameSelected: document.querySelector('#rename-selected'),
  copySelected: document.querySelector('#copy-selected'),
  moveSelected: document.querySelector('#move-selected'),
  trashSelected: document.querySelector('#trash-selected'),
  organizeSelected: document.querySelector('#organize-selected'),
  historyButton: document.querySelector('#history-button'),
  planDialog: document.querySelector('#plan-dialog'),
  planEyebrow: document.querySelector('#plan-eyebrow'),
  planTitle: document.querySelector('#plan-title'),
  planNote: document.querySelector('#plan-note'),
  planList: document.querySelector('#plan-list'),
  closePlan: document.querySelector('#close-plan'),
  cancelPlan: document.querySelector('#cancel-plan'),
  executePlan: document.querySelector('#execute-plan'),
  renameDialog: document.querySelector('#rename-dialog'),
  renameForm: document.querySelector('#rename-form'),
  renameList: document.querySelector('#rename-list'),
  renameError: document.querySelector('#rename-error'),
  closeRename: document.querySelector('#close-rename'),
  cancelRename: document.querySelector('#cancel-rename'),
  historyDialog: document.querySelector('#history-dialog'),
  historyTitle: document.querySelector('#history-title'),
  historyList: document.querySelector('#history-list'),
  closeHistory: document.querySelector('#close-history'),
  duplicatesButton: document.querySelector('#duplicates-button'),
  similarTextsButton: document.querySelector('#similar-texts-button'),
  similarTextsDialog: document.querySelector('#similar-texts-dialog'),
  similarTextsSummary: document.querySelector('#similar-texts-summary'),
  similarTextsList: document.querySelector('#similar-texts-list'),
  closeSimilarTexts: document.querySelector('#close-similar-texts'),
  similarTextTab: document.querySelector('#similar-text-tab'),
  similarImageTab: document.querySelector('#similar-image-tab'),
  duplicatesDialog: document.querySelector('#duplicates-dialog'),
  duplicatesSummary: document.querySelector('#duplicates-summary'),
  duplicatesList: document.querySelector('#duplicates-list'),
  closeDuplicates: document.querySelector('#close-duplicates'),
  duplicateSelectionCount: document.querySelector('#duplicate-selection-count'),
  clearDuplicateSelection: document.querySelector('#clear-duplicate-selection'),
  trashDuplicates: document.querySelector('#trash-duplicates'),
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
  settingsDialog: document.querySelector('#settings-dialog'),
  settingsForm: document.querySelector('#settings-form'),
  closeSettings: document.querySelector('#close-settings'),
  cancelSettings: document.querySelector('#cancel-settings'),
  settingRestoreLibrary: document.querySelector('#setting-restore-library'),
  settingView: document.querySelector('#setting-view'),
  settingSortKey: document.querySelector('#setting-sort-key'),
  settingSortDirection: document.querySelector('#setting-sort-direction'),
  settingPageSize: document.querySelector('#setting-page-size'),
  settingRefresh: document.querySelector('#setting-refresh'),
  settingFullPaths: document.querySelector('#setting-full-paths'),
  settingDensity: document.querySelector('#setting-density'),
  settingOverview: document.querySelector('#setting-overview'),
  settingTaskCenter: document.querySelector('#setting-task-center'),
  settingSizeUnit: document.querySelector('#setting-size-unit'),
  settingDateFormat: document.querySelector('#setting-date-format'),
  settingReduceMotion: document.querySelector('#setting-reduce-motion'),
  settingsContent: document.querySelector('.settings-content'),
  settingsNavButtons: document.querySelectorAll('.settings-nav-button'),
  settingsSections: document.querySelectorAll('.settings-section'),
  dataIndexedFiles: document.querySelector('#data-indexed-files'),
  dataIndexedBytes: document.querySelector('#data-indexed-bytes'),
  dataCachedFiles: document.querySelector('#data-cached-files'),
  dataFullHashes: document.querySelector('#data-full-hashes'),
  settingsDataMessage: document.querySelector('#settings-data-message'),
  rebuildIndex: document.querySelector('#rebuild-index'),
  clearHashCache: document.querySelector('#clear-hash-cache'),
  sortFiles: document.querySelector('#sort-files'),
  sortDirection: document.querySelector('#sort-direction'),
  viewMode: document.querySelector('#view-mode'),
};

function setNotice(message, kind = 'info') {
  if (state._noticeHideTimer) {
    clearTimeout(state._noticeHideTimer);
    state._noticeHideTimer = null;
  }
  elements.notice.hidden = false;
  elements.notice.classList.remove('notice-dismissed');
  elements.noticeText.textContent = message;
  elements.notice.classList.toggle('error', kind === 'error');
  elements.notice.classList.toggle('busy', kind === 'busy');
  elements.noticeIcon.classList.toggle('spinner', kind === 'busy');
  const iconMarkup = kind === 'busy'
    ? '<path d="M21 12a9 9 0 1 1-5.2-8.2"/>'
    : kind === 'error'
      ? '<path d="M12 3 2.5 20h19zM12 9v4M12 17h.01"/>'
      : '<circle cx="12" cy="12" r="9"/><path d="M12 11v6M12 7h.01"/>';
  elements.noticeIcon.replaceChildren(createLineIcon(iconMarkup));
  // Phase 2：info 消息 5 秒自动消退；error/busy 常驻。
  if (kind === 'info') autoCloseNotice(5000);
  else if (state._noticeTimer) { clearTimeout(state._noticeTimer); state._noticeTimer = null; }
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
  elements.similarTextsButton.disabled = !library;
  elements.largeFilesButton.disabled = !library;
  elements.trashHistoryButton.disabled = !library;
  state.activeSmartFolderId = null;
  state.selectedIds.clear();
  updateSelectionState();
  loadSmartFolders();
}

function updateSelectionState() {
  const count = state.selectedIds.size;
  elements.selectionCount.textContent = `已选择 ${count} 项`;
  if (state._batchHideTimer) {
    window.clearTimeout(state._batchHideTimer);
    state._batchHideTimer = null;
  }
  if (count > 0) {
    elements.batchBar.hidden = false;
    window.requestAnimationFrame(() => elements.batchBar.classList.add('batch-visible'));
  } else {
    elements.batchBar.classList.remove('batch-visible');
    if (reducedMotionRequested()) elements.batchBar.hidden = true;
    else state._batchHideTimer = window.setTimeout(() => {
      elements.batchBar.hidden = true;
      state._batchHideTimer = null;
    }, 190);
  }
  elements.renameSelected.disabled = !state.library || count === 0;
  elements.copySelected.disabled = !state.library || count === 0;
  elements.moveSelected.disabled = !state.library || count === 0;
  elements.trashSelected.disabled = !state.library || count === 0;
  elements.organizeSelected.disabled = !state.library || count === 0;
  const visibleIds = state.files.map((file) => file.id);
  const selectedVisible = visibleIds.filter((id) => state.selectedIds.has(id)).length;
  elements.selectVisible.disabled = visibleIds.length === 0;
  elements.selectVisible.checked = visibleIds.length > 0 && selectedVisible === visibleIds.length;
  elements.selectVisible.indeterminate = selectedVisible > 0 && selectedVisible < visibleIds.length;
}

function formatBytes(bytes) {
  const base = state.settings.fileSizeUnit === 'decimal' ? 1000 : 1024;
  if (bytes < base) return `${bytes} B`;
  const units = state.settings.fileSizeUnit === 'decimal'
    ? ['KB', 'MB', 'GB', 'TB']
    : ['KiB', 'MiB', 'GiB', 'TiB'];
  let value = bytes / base;
  let unit = units[0];
  for (let index = 1; index < units.length && value >= base; index += 1) {
    value /= base;
    unit = units[index];
  }
  return `${value >= 10 ? value.toFixed(0) : value.toFixed(1)} ${unit}`;
}

function formatDate(nanoseconds) {
  const date = new Date(Number(BigInt(nanoseconds) / 1000000n));
  if (Number.isNaN(date.getTime())) return '未知';
  return state.settings.dateFormat === 'iso'
    ? `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, '0')}-${String(date.getDate()).padStart(2, '0')}`
    : date.toLocaleDateString('zh-CN', { month: '2-digit', day: '2-digit', year: 'numeric' });
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
  icon.append(createLineIcon('<circle cx="11" cy="11" r="7"/><path d="m20 20-4-4"/>'));
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

function renderSkeleton(count = 6) {
  elements.fileList.replaceChildren();
  elements.fileList.classList.remove('loading-fade');
  const fragment = document.createDocumentFragment();
  for (let i = 0; i < count; i++) {
    const row = document.createElement('div');
    row.className = 'skeleton-row';
    row.innerHTML = '<div class="skeleton-block skeleton-icon shimmer"></div><div><div class="skeleton-block skeleton-title shimmer"></div><div class="skeleton-block skeleton-meta shimmer" style="margin-top:6px"></div></div><div class="skeleton-block skeleton-meta shimmer"></div><div class="skeleton-block skeleton-meta shimmer"></div>';
    fragment.append(row);
  }
  elements.fileList.append(fragment);
}

function renderFiles() {
  elements.fileList.replaceChildren();
  elements.fileList.classList.remove('loading-fade');
  elements.fileList.classList.toggle('grid-mode', state.viewMode === 'grid');
  elements.tableHead.hidden = state.viewMode === 'grid';
  elements.fileCount.textContent = `${state.files.length}${state.nextPath ? '+' : ''} 项`;
  if (state.files.length === 0) {
    const empty = document.createElement('div');
    empty.className = 'empty-state';
    const icon = document.createElement('span');
    icon.className = 'empty-icon';
    icon.append(createLineIcon(state.library
      ? '<path d="M21 12a9 9 0 1 1-5.2-8.2"/>'
      : '<path d="M3 7.5h6l2 2h10v9.5a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/>'));
    icon.classList.toggle('spinner', Boolean(state.library));
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
      path.hidden = !state.settings.showFullPaths;
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
      // Phase 4：patch 勾选态。
      const row = elements.fileList.querySelector(`[data-file-id="${state.selectedId}"]`);
      if (row) row.querySelector('.file-select').checked = state.selectedIds.has(state.selectedId);
      updateSelectionState();
      focusRenderedFile(state.selectedId);
    }
    return;
  }
  const files = visibleFiles();
  if (files.length === 0) return;
  event.preventDefault();
  // P3-L10：无选中时从第一项开始，不在虚拟位置 0 上偏移。
  const selectedIndex = files.findIndex((file) => file.id === state.selectedId);
  const current = selectedIndex === -1 ? -1 : selectedIndex;
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
  // P3-H8：请求令牌，防止旧响应覆盖新结果。
  const token = ++state.listRequestId;
  if (reset) {
    state.nextPath = null;
    state.nextId = null;
    state.files = [];
    state.selectedIds.clear();
    clearPreview();
    // Phase 3：骨架屏——首次加载或切库时显示。
    renderSkeleton();
  }
  try {
    const page = await invoke('files_page', {
      request: {
        libraryId: state.library.id,
        afterPath: state.nextPath,
        afterId: state.nextId,
        limit: state.settings.pageSize,
      },
    });
    if (token !== state.listRequestId) return;
    state.files.push(...page.items);
    state.nextPath = page.nextPath;
    state.nextId = page.nextId;
    renderFiles();
    setNotice(`已载入 ${state.files.length} 个文件。所有索引和预览均在本机完成。`);
  } catch (error) {
    if (token !== state.listRequestId) return;
    setNotice(`读取文件索引失败：${String(error)}`, 'error');
  } finally {
    state.loading = false;
  }
}

let searchTimer = null;
async function runSearch(query, { preserveContext = false } = {}) {
  if (!state.library) return;
  const normalized = query.trim();
  const selectedBefore = preserveContext ? new Set(state.selectedIds) : new Set();
  const previewBefore = preserveContext ? state.selectedId : null;
  if (!preserveContext) {
    state.selectedIds.clear();
    clearPreview();
    updateSelectionState();
  }
  elements.clearSearch.hidden = normalized.length === 0;
  elements.saveSearch.disabled = normalized.length === 0;
  // P3-H8：请求令牌，防止连续搜索旧响应覆盖新结果。
  const token = ++state.listRequestId;
  if (!normalized) {
    setActiveNavigation(null);
    await loadFiles(true);
    return;
  }
  try {
    const files = await invoke('search_files', { libraryId: state.library.id, query: normalized });
    if (token !== state.listRequestId) return;
    state.files = files;
    if (preserveContext) {
      const available = new Set(files.map((file) => file.id));
      state.selectedIds = new Set([...selectedBefore].filter((id) => available.has(id)));
      if (previewBefore !== null && !available.has(previewBefore)) clearPreview();
      updateSelectionState();
    }
    state.nextPath = null;
    state.nextId = null;
    renderFiles();
    setNotice(`搜索到 ${files.length} 个文件。`);
  } catch (error) {
    if (token !== state.listRequestId) return;
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
      const name = document.createElement('strong');
      name.textContent = folder.name;
      const query = document.createElement('small');
      query.textContent = folder.query;
      button.append(name, query);
      button.title = folder.query;
      button.addEventListener('click', () => {
        elements.searchInput.value = folder.query;
        setActiveNavigation(folder.id);
        runSearch(folder.query);
      });
      const remove = document.createElement('button');
      remove.type = 'button';
      remove.className = 'delete-smart-folder';
      remove.append(createLineIcon('<path d="M6 6l12 12M18 6 6 18"/>'));
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
  openModal(elements.smartFolderDialog);
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
    closeModal(elements.smartFolderDialog);
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
    openPlanDialog(plan);
    setNotice(`计划已生成，共 ${plan.items.length} 项；尚未移动任何文件。`);
  } catch (error) {
    setNotice(`无法生成整理计划：${String(error)}`, 'error');
  } finally {
    updateSelectionState();
  }
}

async function createCopyPlan() {
  if (!state.library || state.selectedIds.size === 0) return;
  elements.copySelected.disabled = true;
  setNotice('请选择资料库内的目标文件夹；取消选择不会创建计划。', 'busy');
  try {
    const plan = await invoke('create_copy_plan', {
      request: {
        libraryId: state.library.id,
        fileIds: Array.from(state.selectedIds),
      },
    });
    if (!plan) {
      setNotice('已取消选择复制目标，文件没有变化。');
      return;
    }
    state.currentPlan = plan;
    openPlanDialog(plan);
    setNotice(`复制计划已生成，共 ${plan.items.length} 项；尚未写入任何副本。`);
  } catch (error) {
    setNotice(`无法生成复制计划：${String(error)}`, 'error');
  } finally {
    updateSelectionState();
  }
}

async function createMovePlan() {
  if (!state.library || state.selectedIds.size === 0) return;
  elements.moveSelected.disabled = true;
  setNotice('请选择资料库内的移动目标文件夹；取消选择不会创建计划。', 'busy');
  try {
    const plan = await invoke('create_move_plan', {
      request: {
        libraryId: state.library.id,
        fileIds: Array.from(state.selectedIds),
      },
    });
    if (!plan) {
      setNotice('已取消选择移动目标，文件没有变化。');
      return;
    }
    state.currentPlan = plan;
    openPlanDialog(plan);
    setNotice(`移动计划已生成，共 ${plan.items.length} 项；尚未移动任何文件。`);
  } catch (error) {
    setNotice(`无法生成移动计划：${String(error)}`, 'error');
  } finally {
    updateSelectionState();
  }
}

async function createTrashPlan() {
  if (!state.library || state.selectedIds.size === 0) return;
  elements.trashSelected.disabled = true;
  setNotice('正在校验所选文件并生成可恢复删除预览…', 'busy');
  try {
    const plan = await invoke('create_trash_plan', {
      request: {
        libraryId: state.library.id,
        fileIds: Array.from(state.selectedIds),
      },
    });
    state.currentPlan = plan;
    openPlanDialog(plan);
    setNotice(`废纸篓计划已生成，共 ${plan.items.length} 项；尚未移动任何文件。`);
  } catch (error) {
    setNotice(`无法生成废纸篓计划：${String(error)}`, 'error');
  } finally {
    updateSelectionState();
  }
}

function openRenameDialog() {
  if (!state.library || state.selectedIds.size === 0) return;
  const files = state.files
    .filter((file) => state.selectedIds.has(file.id))
    .sort((left, right) => left.path.localeCompare(right.path, 'zh-CN'));
  elements.renameList.replaceChildren();
  elements.renameError.hidden = true;
  files.forEach((file) => {
    const row = document.createElement('label');
    row.className = 'rename-row';
    const current = document.createElement('span');
    current.textContent = file.name;
    current.title = file.path;
    const input = document.createElement('input');
    input.value = file.name;
    input.maxLength = 255;
    input.required = true;
    input.autocomplete = 'off';
    input.dataset.fileId = file.id;
    input.dataset.originalName = file.name;
    input.setAttribute('aria-label', `${file.name} 的新文件名`);
    row.append(current, input);
    elements.renameList.append(row);
  });
  openModal(elements.renameDialog);
  const firstInput = elements.renameList.querySelector('input');
  if (firstInput) {
    firstInput.focus();
    const dot = firstInput.value.lastIndexOf('.');
    firstInput.setSelectionRange(0, dot > 0 ? dot : firstInput.value.length);
  }
}

async function submitRenamePlan(event) {
  event.preventDefault();
  if (!state.library) return;
  const items = Array.from(elements.renameList.querySelectorAll('input'))
    .map((input) => ({
      fileId: input.dataset.fileId,
      newName: input.value,
      originalName: input.dataset.originalName,
    }))
    .filter((item) => item.newName !== item.originalName)
    .map(({ fileId, newName }) => ({ fileId, newName }));
  if (items.length === 0) {
    elements.renameError.textContent = '没有文件名发生变化，请至少修改一项。';
    elements.renameError.hidden = false;
    setNotice('没有文件名发生变化，请至少修改一项。', 'error');
    return;
  }
  const submit = elements.renameForm.querySelector('button[type="submit"]');
  submit.disabled = true;
  elements.renameError.hidden = true;
  setNotice('正在校验文件名、目标冲突和文件状态…', 'busy');
  try {
    const plan = await invoke('create_rename_plan', {
      request: { libraryId: state.library.id, items },
    });
    closeModal(elements.renameDialog);
    state.currentPlan = plan;
    openPlanDialog(plan);
    setNotice(`重命名计划已生成，共 ${plan.items.length} 项；尚未修改任何文件。`);
  } catch (error) {
    elements.renameError.textContent = String(error);
    elements.renameError.hidden = false;
    setNotice(`无法生成重命名计划：${String(error)}`, 'error');
  } finally {
    submit.disabled = false;
  }
}

function openPlanDialog(plan) {
  // P3-M5：防止并发创建计划导致弹窗被覆盖。
  if (elements.planDialog.open) return;
  const presentation = {
    rename: {
      eyebrow: 'RENAME PREVIEW', title: '确认重命名计划',
      note: '以下只是预览。点击“确认执行”后才会重命名文件；目标冲突或文件变化会停止执行。',
    },
    copy: {
      eyebrow: 'COPY PREVIEW', title: '确认安全复制计划',
      note: '以下只是预览。执行时会校验源文件、验证副本哈希并排他发布；源文件会保留，已有目标绝不覆盖。',
    },
    move: {
      eyebrow: 'MOVE PREVIEW', title: '确认安全移动计划',
      note: '以下只是预览。同卷使用排他原子移动，跨卷使用校验复制后再移除源文件；已有目标绝不覆盖。',
    },
    trash: {
      eyebrow: 'RECOVERABLE TRASH', title: '确认移入归序废纸篓',
      note: '文件将移入资料库内部的受控隔离区，并从文件列表隐藏；可从操作历史恢复。当前版本不会立即永久删除。',
    },
    organize: {
      eyebrow: 'ORGANIZE PREVIEW', title: '确认整理计划',
      note: '以下只是预览。点击“确认执行”后才会移动文件，且不会覆盖已有文件。',
    },
  }[plan.operationKind] || {
    eyebrow: 'FILE OPERATION PREVIEW', title: '确认文件操作', note: '以下只是预览。确认后才会执行。',
  };
  elements.planEyebrow.textContent = presentation.eyebrow;
  elements.planTitle.textContent = presentation.title;
  elements.planNote.textContent = presentation.note;
  renderPlan(plan);
  openModal(elements.planDialog);
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
  if (elements.planDialog.open) closeModal(elements.planDialog);
}

async function executeCurrentPlan() {
  if (!state.currentPlan) return;
  elements.executePlan.disabled = true;
  elements.cancelPlan.disabled = true;
  const actions = { organize: '整理', rename: '重命名', copy: '复制', move: '移动', trash: '移入废纸篓' };
  const commands = {
    organize: 'execute_organize_plan',
    rename: 'execute_rename_plan',
    copy: 'execute_copy_plan',
    move: 'execute_move_plan',
    trash: 'execute_trash_plan',
  };
  const action = actions[state.currentPlan.operationKind] || '文件操作';
  setNotice(`正在执行已确认的${action}计划，请勿关闭应用。`, 'busy');
  // P3：拆分为"执行"（成败关键）与"后置同步"（best-effort），后置失败不误报。
  let executed = false;
  try {
    const command = commands[state.currentPlan.operationKind];
    if (!command) throw new Error('当前页面不支持该计划类型');
    const result = await invoke(command, { planId: state.currentPlan.id });
    executed = true;
    closePlanDialog();
    state.currentPlan = null;
    state.selectedIds.clear();
    state.files = [];
    state.nextPath = null;
    state.nextId = null;
    clearPreview();
    renderFiles();
    try {
      await invoke('rescan_library', { libraryId: state.library.id });
      await refreshJobs();
      setNotice(`${action}完成：${result.completedItems}/${result.totalItems} 个文件；正在同步新位置。`, 'busy');
    } catch {
      setNotice(`${action}已完成（${result.completedItems}/${result.totalItems} 个文件），同步稍后重试。`, 'busy');
    }
  } catch (error) {
    if (executed) {
      setNotice(`${action}已提交，但执行后同步失败：${String(error)}`, 'busy');
    } else {
      setNotice(`${action}未完成：${String(error)}。请在操作历史中查看逐项状态。`, 'error');
    }
    await loadHistory(false);
  } finally {
    elements.executePlan.disabled = false;
    elements.cancelPlan.disabled = false;
  }
}

async function loadHistory(show = true, operationKind = null) {
  if (!state.library) return;
  try {
    const operations = await invoke('operation_history', { libraryId: state.library.id });
    const visibleOperations = operationKind
      ? operations.filter((operation) => operation.operationKind === operationKind)
      : operations;
    elements.historyTitle.textContent = operationKind === 'trash' ? '可恢复文件' : '操作历史';
    renderHistory(visibleOperations, operationKind);
    if (show && !elements.historyDialog.open) openModal(elements.historyDialog);
  } catch (error) {
    setNotice(`读取操作历史失败：${String(error)}`, 'error');
  }
}

function renderHistory(operations, operationKind = null) {
  elements.historyList.replaceChildren();
  if (operations.length === 0) {
    const empty = document.createElement('div');
    empty.className = 'empty-state';
    empty.textContent = operationKind === 'trash'
      ? '尚无移入受控废纸篓的文件。'
      : '尚无已执行的文件操作。';
    elements.historyList.append(empty);
    return;
  }
  const labels = {
    completed: '已完成', rolled_back: '已撤销', failed: '失败',
    recovery_needed: '需要处理', executing: '执行中', rollback_pending: '撤销中',
  };
  const operationLabels = { organize: '整理', rename: '重命名', copy: '复制', move: '移动', trash: '废纸篓' };
  operations.forEach((operation) => {
    const card = document.createElement('article');
    card.className = 'history-item';
    const head = document.createElement('div');
    head.className = 'history-item-head';
    const title = document.createElement('strong');
    const operationLabel = operationLabels[operation.operationKind] || '文件操作';
    title.textContent = `${operationLabel} · ${new Date(operation.createdAtMs).toLocaleString('zh-CN')} · ${operation.items.length} 项`;
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
  openModal(elements.undoDialog);
}

async function confirmUndoOperation() {
  const pending = state.pendingUndo;
  if (!pending) return;
  elements.confirmUndo.disabled = true;
  pending.button.disabled = true;
  setNotice('正在验证并撤销操作…', 'busy');
  // P3：拆分执行与后置同步；失败时保留 pendingUndo 以允许重试。
  let undone = false;
  try {
    const result = await invoke('undo_operation', { operationId: pending.operationId });
    undone = true;
    closeModal(elements.undoDialog);
    state.pendingUndo = null;
    state.files = [];
    state.nextPath = null;
    state.nextId = null;
    clearPreview();
    renderFiles();
    try {
      await loadHistory(false);
      await invoke('rescan_library', { libraryId: state.library.id });
      await refreshJobs();
      setNotice(`已安全撤销 ${result.completedItems}/${result.totalItems} 个文件。`, 'busy');
    } catch {
      setNotice(`撤销已完成（${result.completedItems}/${result.totalItems} 个文件），同步稍后重试。`, 'busy');
    }
  } catch (error) {
    if (undone) {
      setNotice(`撤销已提交，但同步失败：${String(error)}`, 'busy');
      state.pendingUndo = null;
    } else {
      setNotice(`撤销失败：${String(error)}。可重试。`, 'error');
      await loadHistory(false);
      // 保留 state.pendingUndo，允许重试（修复死按钮 L6）。
    }
  } finally {
    if (state.pendingUndo === null) {
      pending.button.disabled = false;
    }
    elements.confirmUndo.disabled = false;
  }
}

async function loadExactDuplicates() {
  if (!state.library) return;
  state.duplicateSelectedIds.clear();
  updateDuplicateSelectionState();
  elements.duplicatesButton.disabled = true;
  elements.duplicatesSummary.textContent = '分析已进入本地后台任务。可在任务中心暂停、继续或取消；只有候选文件才会进行完整哈希。';
  elements.duplicatesList.replaceChildren();
  const loading = document.createElement('div');
  loading.className = 'empty-state';
  loading.textContent = '正在准备精确重复分析…';
  elements.duplicatesList.append(loading);
  if (!elements.duplicatesDialog.open) openModal(elements.duplicatesDialog);
  setNotice('重复分析已进入后台任务；可以继续浏览文件。', 'busy');
  try {
    state.activeDuplicateJobId = await invoke('start_exact_duplicate_analysis', { libraryId: state.library.id });
    state.lastDuplicateResultJobId = null;
    await refreshJobs();
  } catch (error) {
    setNotice(`重复文件分析失败：${String(error)}`, 'error');
    elements.duplicatesButton.disabled = !state.library;
  }
}

function renderDuplicates(report) {
  elements.duplicatesSummary.textContent = `扫描 ${report.inputFiles} 个文件；本次快速指纹 ${report.quickFingerprintedFiles} 个、完整哈希 ${report.fullyHashedFiles} 个；缓存命中 ${report.quickCacheHits} 个快速指纹、${report.fullCacheHits} 个完整哈希；跳过 ${report.skippedFiles} 个。评分仅供参考，必须手动勾选后才会生成可恢复清理计划。`;
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
    title.textContent = `${group.files.length} 个相同文件 · 每个 ${formatBytes(group.size)}`;
    const savings = document.createElement('span');
    savings.className = 'status-pill';
    savings.textContent = `可节省 ${formatBytes(group.potentialSavings)}`;
    head.append(title, savings);
    const paths = document.createElement('div');
    paths.className = 'duplicate-paths';
    group.files.forEach((file) => {
      const row = document.createElement('label');
      row.className = 'duplicate-path';
      const select = document.createElement('input');
      select.type = 'checkbox';
      select.checked = state.duplicateSelectedIds.has(file.fileId);
      select.setAttribute('aria-label', `选择清理 ${file.path}`);
      select.addEventListener('change', () => {
        if (select.checked) {
          state.duplicateSelectedIds.add(file.fileId);
          const selectedInGroup = group.files.filter((item) => state.duplicateSelectedIds.has(item.fileId));
          if (selectedInGroup.length === group.files.length) {
            state.duplicateSelectedIds.delete(file.fileId);
            select.checked = false;
            setNotice('每组重复文件必须至少保留一个；请选择其他保留项。', 'error');
          }
        } else {
          state.duplicateSelectedIds.delete(file.fileId);
        }
        updateDuplicateSelectionState();
      });
      row.append(select);
      if (file.suggestedKeep) {
        const keep = document.createElement('strong');
        keep.textContent = '建议保留';
        row.append(keep);
      }
      const details = document.createElement('span');
      details.className = 'duplicate-file-details';
      const value = document.createElement('span');
      value.textContent = file.path;
      value.title = file.path;
      const reasons = document.createElement('small');
      reasons.textContent = file.reasons.join(' · ');
      reasons.title = reasons.textContent;
      details.append(value, reasons);
      const score = document.createElement('span');
      score.className = 'duplicate-score';
      score.textContent = `保留分 ${file.retentionScore}`;
      row.append(details, score);
      paths.append(row);
    });
    card.append(head, paths);
    elements.duplicatesList.append(card);
  });
  updateDuplicateSelectionState();
}

async function loadSimilarContent(kind = 'text', openDialog = true) {
  if (!state.library) return;
  const isImage = kind === 'image';
  elements.similarTextTab.classList.toggle('active', !isImage);
  elements.similarImageTab.classList.toggle('active', isImage);
  elements.similarTextTab.setAttribute('aria-selected', String(!isImage));
  elements.similarImageTab.setAttribute('aria-selected', String(isImage));
  elements.similarTextsButton.disabled = true;
  elements.similarTextTab.disabled = true;
  elements.similarImageTab.disabled = true;
  elements.similarTextsSummary.textContent = `正在本机提取${isImage ? '图片' : '文本'}特征并生成只读候选…`;
  elements.similarTextsList.replaceChildren();
  const loading = document.createElement('div');
  loading.className = 'empty-state';
  loading.textContent = `正在分析相似${isImage ? '图片' : '文本'}…`;
  elements.similarTextsList.append(loading);
  if (openDialog && !elements.similarTextsDialog.open) openModal(elements.similarTextsDialog);
  setNotice(`相似${isImage ? '图片' : '文本'}分析已进入后台任务；文件内容不会上传。`, 'busy');
  try {
    state.activeSimilarityKind = kind;
    state.activeSimilarityJobId = await invoke('start_similarity_analysis', { libraryId: state.library.id, contentKind: kind });
    await refreshJobs();
  } catch (error) {
    setNotice(`相似${isImage ? '图片' : '文本'}分析失败：${String(error)}`, 'error');
    elements.similarTextsButton.disabled = !state.library;
    elements.similarTextTab.disabled = !state.library;
    elements.similarImageTab.disabled = !state.library;
  }
}

function renderSimilarContent(report) {
  const isImage = report.contentKind === 'image';
  elements.similarTextsSummary.textContent = `分析 ${report.inputFiles} 个${isImage ? '图片' : '文本'}文件；新计算 ${report.computedFeatures} 个特征，缓存命中 ${report.cacheHits} 个，跳过 ${report.skippedFiles} 个。相似候选只供比较，不代表内容完全相同。`;
  elements.similarTextsList.replaceChildren();
  if (report.pairs.length === 0) {
    const empty = document.createElement('div');
    empty.className = 'empty-state';
    empty.textContent = `没有发现达到当前阈值的相似${isImage ? '图片' : '文本'}。`;
    elements.similarTextsList.append(empty);
    return;
  }
  report.pairs.forEach((pair) => {
    const card = document.createElement('article');
    card.className = 'history-item';
    const head = document.createElement('div');
    head.className = 'history-item-head';
    const title = document.createElement('strong');
    title.textContent = `${Math.round(pair.similarity * 100)}% 相似`;
    const distance = document.createElement('span');
    distance.className = 'status-pill';
    distance.textContent = `汉明距离 ${pair.hammingDistance}`;
    head.append(title, distance);
    const paths = document.createElement('div');
    paths.className = 'duplicate-paths';
    [pair.leftPath, pair.rightPath].forEach((path) => {
      const row = document.createElement('div');
      row.className = 'duplicate-path';
      const value = document.createElement('span');
      value.textContent = path;
      value.title = path;
      row.append(value);
      paths.append(row);
    });
    card.append(head, paths);
    elements.similarTextsList.append(card);
  });
}

function updateDuplicateSelectionState() {
  const count = state.duplicateSelectedIds.size;
  elements.duplicateSelectionCount.textContent = count === 0 ? '未选择清理文件' : `已选择 ${count} 个文件`;
  elements.clearDuplicateSelection.disabled = count === 0;
  elements.trashDuplicates.disabled = count === 0 || !state.library;
}

function clearDuplicateSelection() {
  state.duplicateSelectedIds.clear();
  elements.duplicatesList.querySelectorAll('input[type="checkbox"]').forEach((input) => {
    input.checked = false;
  });
  updateDuplicateSelectionState();
}

async function createDuplicateTrashPlan() {
  if (!state.library || state.duplicateSelectedIds.size === 0) return;
  elements.trashDuplicates.disabled = true;
  setNotice('正在验证重复文件选择并生成可恢复清理预览…', 'busy');
  try {
    const plan = await invoke('create_trash_plan', {
      request: {
        libraryId: state.library.id,
        fileIds: Array.from(state.duplicateSelectedIds),
      },
    });
    state.currentPlan = plan;
    closeModal(elements.duplicatesDialog);
    openPlanDialog(plan);
    setNotice(`可恢复清理计划已生成，共 ${plan.items.length} 项；尚未移动任何文件。`);
    clearDuplicateSelection();
  } catch (error) {
    setNotice(`无法生成重复文件清理计划：${String(error)}`, 'error');
  } finally {
    updateDuplicateSelectionState();
  }
}

async function selectFile(file) {
  const requestId = state.previewRequestId + 1;
  state.previewRequestId = requestId;
  // Phase 4：patch 选中态而非重建全部行。
  const oldRow = state.selectedId ? elements.fileList.querySelector(`[data-file-id="${state.selectedId}"]`) : null;
  state.selectedId = file.id;
  const newRow = elements.fileList.querySelector(`[data-file-id="${file.id}"]`);
  if (oldRow) { oldRow.classList.remove('selected'); oldRow.setAttribute('aria-selected', 'false'); }
  if (newRow) { newRow.classList.add('selected'); newRow.setAttribute('aria-selected', 'true'); }
  // 预览区 fade 加载。
  elements.preview.style.opacity = '0.5';
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
  elements.preview.style.opacity = '';
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

async function syncDuplicateAnalysis(jobs) {
  if (!state.activeDuplicateJobId) return false;
  const job = jobs.find((candidate) => candidate.id === state.activeDuplicateJobId);
  if (!job) return false;
  if (job.status === 'completed' && job.id !== state.lastDuplicateResultJobId) {
    const report = await invoke('exact_duplicate_result', { jobId: job.id });
    if (!report) return true;
    state.lastDuplicateResultJobId = job.id;
    state.activeDuplicateJobId = null;
    renderDuplicates(report);
    elements.duplicatesButton.disabled = !state.library;
    setNotice(`重复分析完成：发现 ${report.groups.length} 组精确重复文件。`);
    return true;
  }
  if (job.status === 'failed' || job.status === 'cancelled') {
    const label = job.status === 'cancelled' ? '已取消' : '失败';
    elements.duplicatesSummary.textContent = `精确重复分析${label}。可以关闭窗口后重新开始。`;
    elements.duplicatesList.replaceChildren();
    const message = document.createElement('div');
    message.className = 'empty-state';
    message.textContent = `后台任务${label}，没有生成清理建议。`;
    elements.duplicatesList.append(message);
    state.activeDuplicateJobId = null;
    elements.duplicatesButton.disabled = !state.library;
    setNotice(`重复文件分析${label}。`, job.status === 'failed' ? 'error' : 'normal');
    return true;
  }
  return ['running', 'queued', 'pause_requested', 'paused', 'cancel_requested'].includes(job.status);
}

async function syncSimilarityAnalysis(jobs) {
  if (!state.activeSimilarityJobId) return false;
  const job = jobs.find((candidate) => candidate.id === state.activeSimilarityJobId);
  if (!job) return false;
  const isImage = state.activeSimilarityKind === 'image';
  if (job.status === 'completed') {
    const report = await invoke('similarity_result', { jobId: job.id });
    if (!report) return true;
    state.activeSimilarityJobId = null;
    renderSimilarContent(report);
    elements.similarTextsButton.disabled = !state.library;
    elements.similarTextTab.disabled = !state.library;
    elements.similarImageTab.disabled = !state.library;
    setNotice(`相似${isImage ? '图片' : '文本'}分析完成：发现 ${report.pairs.length} 对只读候选。`);
    return true;
  }
  if (job.status === 'failed' || job.status === 'cancelled') {
    const label = job.status === 'cancelled' ? '已取消' : '失败';
    elements.similarTextsSummary.textContent = `相似${isImage ? '图片' : '文本'}分析${label}，可重新开始。`;
    elements.similarTextsList.replaceChildren();
    const message = document.createElement('div');
    message.className = 'empty-state';
    message.textContent = `后台任务${label}，没有生成只读候选。`;
    elements.similarTextsList.append(message);
    state.activeSimilarityJobId = null;
    elements.similarTextsButton.disabled = !state.library;
    elements.similarTextTab.disabled = !state.library;
    elements.similarImageTab.disabled = !state.library;
    setNotice(`相似内容分析${label}。`, job.status === 'failed' ? 'error' : 'normal');
    return true;
  }
  return ['running', 'queued', 'pause_requested', 'paused', 'cancel_requested'].includes(job.status);
}

async function refreshJobs() {
  if (state.refreshingJobs) return;
  state.refreshingJobs = true;
  try {
    await invoke('poll_file_events');
    const jobs = await invoke('jobs');
    elements.taskList.replaceChildren();
    const visibleJobs = jobs.slice(0, 4);
    elements.taskSummary.textContent = visibleJobs.length ? `最近 ${visibleJobs.length} 项` : '暂无任务';
    visibleJobs.forEach((job) => {
      const item = document.createElement('div');
      item.className = `task ${job.status}`;
      const dot = document.createElement('span');
      dot.className = 'task-status';
      const copy = document.createElement('span');
      copy.className = 'task-copy';
      const name = document.createElement('strong');
      const jobNames = { library_scan: '资料库扫描', snapshot_reconcile: '文件变更对账', exact_duplicate_analysis: '精确重复分析', similar_text_analysis: '相似文本分析', similar_image_analysis: '相似图片分析' };
      name.textContent = jobNames[job.kind] || job.kind;
      const status = document.createElement('small');
      const labels = {
        queued: '等待执行',
        running: '正在处理',
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
      // Phase 3：确定性/非确定性进度条。
      if (job.status === 'running' || job.status === 'queued') {
        const progressBar = document.createElement('div');
        progressBar.className = 'task-progress';
        const bar = document.createElement('div');
        bar.className = `task-progress-bar${job.progressTotal === null ? ' indeterminate' : ''}`;
        if (job.progressTotal !== null && job.progressTotal > 0) {
          bar.style.width = `${(Number(job.progressCurrent) / Number(job.progressTotal)) * 100}%`;
        }
        progressBar.append(bar);
        item.append(progressBar);
      }
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
    const duplicateHandled = await syncDuplicateAnalysis(jobs);
    const similarityHandled = await syncSimilarityAnalysis(jobs);
    if (!duplicateHandled && !similarityHandled && jobs.some((job) => ['running', 'queued', 'pause_requested', 'cancel_requested'].includes(job.status))) {
      const hasDuplicateAnalysis = jobs.some((job) => job.kind === 'exact_duplicate_analysis' && ['running', 'queued', 'pause_requested', 'cancel_requested'].includes(job.status));
      const hasSimilarityAnalysis = jobs.some((job) => ['similar_text_analysis', 'similar_image_analysis'].includes(job.kind) && ['running', 'queued', 'pause_requested', 'cancel_requested'].includes(job.status));
      setNotice(hasDuplicateAnalysis ? '后台正在分析精确重复文件，可以继续浏览和搜索。' : hasSimilarityAnalysis ? '后台正在分析相似内容，可以继续浏览和搜索。' : '后台正在建立文件索引，完成后列表会自动刷新。', 'busy');
    } else if (state.library) {
      const completed = jobs.find((job) =>
        (job.kind === 'library_scan' || job.kind === 'snapshot_reconcile')
        && job.status === 'completed');
      if (completed && completed.id !== state.lastCompletedJobId) {
        state.lastCompletedJobId = completed.id;
        // P3-H7：扫描完成时若处于搜索态则原位刷新，不摧毁用户搜索/选择。
        const searchText = elements.searchInput.value.trim();
        const smartFolderActive = state.activeSmartFolderId !== null;
        if (smartFolderActive || searchText) {
          await runSearch(searchText, { preserveContext: true });
        } else {
          await loadFiles(true);
        }
        await loadOverview();
      } else if (!state.currentPlan && elements.notice.classList.contains('busy')) {
        // P3-L7: 仅在无执行中计划时改写 busy 通知，避免抹掉操作警告。
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

function applySettings(settings) {
  state.settings = settings;
  state.viewMode = settings.defaultViewMode;
  state.sortKey = settings.defaultSortKey;
  state.sortDirection = settings.defaultSortDirection;
  setViewIcon(settings.defaultViewMode === 'grid');
  setSortIcon(settings.defaultSortDirection === 'asc');
  elements.sortFiles.value = settings.defaultSortKey;
  elements.sortDirection.classList.toggle('descending', settings.defaultSortDirection === 'desc');
  elements.sortDirection.setAttribute('aria-label', settings.defaultSortDirection === 'asc' ? '当前升序，点击切换为降序' : '当前降序，点击切换为升序');
  elements.settingRestoreLibrary.checked = settings.restoreLastLibrary;
  elements.settingView.value = settings.defaultViewMode;
  elements.settingSortKey.value = settings.defaultSortKey;
  elements.settingSortDirection.value = settings.defaultSortDirection;
  elements.settingPageSize.value = String(settings.pageSize);
  elements.settingRefresh.value = String(settings.refreshIntervalMs);
  elements.settingFullPaths.checked = settings.showFullPaths;
  elements.settingDensity.value = settings.density;
  elements.settingOverview.checked = settings.showOverview;
  elements.settingTaskCenter.checked = settings.showTaskCenter;
  elements.settingSizeUnit.value = settings.fileSizeUnit;
  elements.settingDateFormat.value = settings.dateFormat;
  elements.settingReduceMotion.checked = settings.reduceMotion;
  document.body.classList.toggle('compact-density', settings.density === 'compact');
  document.body.classList.toggle('reduce-motion', settings.reduceMotion);
  elements.overview.hidden = !settings.showOverview;
  elements.taskCenter.hidden = !settings.showTaskCenter;
  if (state.refreshTimer !== null) window.clearInterval(state.refreshTimer);
  state.refreshTimer = window.setInterval(refreshJobs, settings.refreshIntervalMs);
  renderFiles();
}

function openSettings() {
  elements.settingRestoreLibrary.checked = state.settings.restoreLastLibrary;
  elements.settingView.value = state.settings.defaultViewMode;
  elements.settingSortKey.value = state.settings.defaultSortKey;
  elements.settingSortDirection.value = state.settings.defaultSortDirection;
  elements.settingPageSize.value = String(state.settings.pageSize);
  elements.settingRefresh.value = String(state.settings.refreshIntervalMs);
  elements.settingFullPaths.checked = state.settings.showFullPaths;
  elements.settingDensity.value = state.settings.density;
  elements.settingOverview.checked = state.settings.showOverview;
  elements.settingTaskCenter.checked = state.settings.showTaskCenter;
  elements.settingSizeUnit.value = state.settings.fileSizeUnit;
  elements.settingDateFormat.value = state.settings.dateFormat;
  elements.settingReduceMotion.checked = state.settings.reduceMotion;
  elements.settingsContent.scrollTop = 0;
  elements.settingsNavButtons.forEach((button, index) => {
    button.classList.toggle('active', index === 0);
    button.setAttribute('aria-selected', String(index === 0));
  });
  elements.settingsSections.forEach((section, index) => { section.hidden = index !== 0; });
  elements.settingsDataMessage.textContent = '';
  loadLocalDataStatus();
  openModal(elements.settingsDialog);
}

async function loadLocalDataStatus() {
  const enabled = Boolean(state.library);
  elements.rebuildIndex.disabled = !enabled;
  elements.clearHashCache.disabled = !enabled;
  if (!enabled) {
    elements.dataIndexedFiles.textContent = '—';
    elements.dataIndexedBytes.textContent = '—';
    elements.dataCachedFiles.textContent = '—';
    elements.dataFullHashes.textContent = '—';
    elements.settingsDataMessage.textContent = '选择资料库后可查看和管理本地索引数据。';
    return;
  }
  try {
    const status = await invoke('local_data_status', { libraryId: state.library.id });
    elements.dataIndexedFiles.textContent = status.indexedFiles.toLocaleString('zh-CN');
    elements.dataIndexedBytes.textContent = formatBytes(status.indexedBytes);
    elements.dataCachedFiles.textContent = status.cachedFiles.toLocaleString('zh-CN');
    elements.dataFullHashes.textContent = status.fullyHashedFiles.toLocaleString('zh-CN');
  } catch (error) {
    elements.settingsDataMessage.textContent = `读取本地数据状态失败：${String(error)}`;
  }
}

async function rebuildLibraryIndex() {
  if (!state.library) return;
  elements.rebuildIndex.disabled = true;
  elements.settingsDataMessage.textContent = '正在创建重新扫描任务…';
  try {
    await invoke('rescan_library', { libraryId: state.library.id });
    await refreshJobs();
    elements.settingsDataMessage.textContent = '重新扫描任务已创建，可在任务中心查看进度。';
  } catch (error) {
    elements.settingsDataMessage.textContent = `重新扫描失败：${String(error)}`;
  } finally {
    elements.rebuildIndex.disabled = false;
  }
}

async function clearLibraryHashCache() {
  if (!state.library) return;
  elements.clearHashCache.disabled = true;
  elements.settingsDataMessage.textContent = '正在清理可重建的哈希缓存…';
  try {
    const removed = await invoke('clear_hash_cache', { libraryId: state.library.id });
    await loadLocalDataStatus();
    elements.settingsDataMessage.textContent = `已清理 ${removed} 条缓存记录；下次重复分析会按需重建。`;
  } catch (error) {
    elements.settingsDataMessage.textContent = `清理缓存失败：${String(error)}`;
  } finally {
    elements.clearHashCache.disabled = false;
  }
}

async function submitSettings(event) {
  event.preventDefault();
  const settings = {
    restoreLastLibrary: elements.settingRestoreLibrary.checked,
    defaultViewMode: elements.settingView.value,
    defaultSortKey: elements.settingSortKey.value,
    defaultSortDirection: elements.settingSortDirection.value,
    pageSize: Number(elements.settingPageSize.value),
    refreshIntervalMs: Number(elements.settingRefresh.value),
    showFullPaths: elements.settingFullPaths.checked,
    density: elements.settingDensity.value,
    showOverview: elements.settingOverview.checked,
    showTaskCenter: elements.settingTaskCenter.checked,
    fileSizeUnit: elements.settingSizeUnit.value,
    dateFormat: elements.settingDateFormat.value,
    reduceMotion: elements.settingReduceMotion.checked,
  };
  try {
    const saved = await invoke('save_app_settings', { settings });
    applySettings(saved);
    await loadOverview();
    closeModal(elements.settingsDialog);
    setNotice('设置已保存并立即生效。');
  } catch (error) {
    setNotice(`保存设置失败：${String(error)}`, 'error');
  }
}

async function connect() {
  let runtime;
  try {
    runtime = await invoke('runtime_info');
    elements.runtime.textContent = inDesktopApp
      ? `${runtime.version} · ${runtime.offline ? '离线' : '联网'}`
      : `${runtime.version} · 未连接 APP`;
  } catch (error) {
    elements.runtime.textContent = '桌面核心不可用';
    setNotice(`无法调用桌面核心：${String(error)}`, 'error');
    return;
  }
  try {
    const settings = await invoke('app_settings');
    applySettings(settings);
    const library = settings.restoreLastLibrary ? await invoke('active_library') : null;
    setLibrary(library);
    if (library) {
      await Promise.all([loadFiles(true), loadOverview()]);
    } else {
      await loadOverview();
    }
    await refreshJobs();
    await subscribeToJobEvents();
  } catch (error) {
    setNotice(`本地数据初始化失败：${String(error)}`, 'error');
  }
}

elements.chooseFolder.addEventListener('click', chooseFolder);
elements.allFilesButton.addEventListener('click', showAllFiles);
elements.smartFoldersButton.addEventListener('click', () => openSmartFolderDialog());
elements.settingsButton.addEventListener('click', openSettings);
elements.largeFilesButton.addEventListener('click', () => {
  elements.searchInput.value = 'size:>100MB';
  setActiveNavigation(null);
  runSearch('size:>100MB');
});
elements.trashHistoryButton.addEventListener('click', () => loadHistory(true, 'trash'));
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
  transitionUi(renderFiles, 'file-layout');
});
elements.sortDirection.addEventListener('click', () => {
  state.sortDirection = state.sortDirection === 'asc' ? 'desc' : 'asc';
  const ascending = state.sortDirection === 'asc';
  setSortIcon(ascending);
  elements.sortDirection.setAttribute('aria-label', ascending ? '当前升序，点击切换为降序' : '当前降序，点击切换为升序');
  transitionUi(renderFiles, 'file-layout');
});
elements.viewMode.addEventListener('click', () => {
  state.viewMode = state.viewMode === 'list' ? 'grid' : 'list';
  const grid = state.viewMode === 'grid';
  setViewIcon(grid);
  elements.viewMode.setAttribute('aria-label', grid ? '切换为列表视图' : '切换为网格视图');
  transitionUi(renderFiles, 'file-layout');
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
elements.renameSelected.addEventListener('click', openRenameDialog);
elements.copySelected.addEventListener('click', createCopyPlan);
elements.moveSelected.addEventListener('click', createMovePlan);
elements.trashSelected.addEventListener('click', createTrashPlan);
elements.clearSelection.addEventListener('click', () => {
  state.selectedIds.clear();
  renderFiles();
});
elements.executePlan.addEventListener('click', executeCurrentPlan);
elements.renameForm.addEventListener('submit', submitRenamePlan);
elements.closeRename.addEventListener('click', () => closeModal(elements.renameDialog));
elements.cancelRename.addEventListener('click', () => closeModal(elements.renameDialog));
elements.cancelPlan.addEventListener('click', closePlanDialog);
elements.closePlan.addEventListener('click', closePlanDialog);
elements.historyButton.addEventListener('click', () => loadHistory(true));
elements.closeHistory.addEventListener('click', () => closeModal(elements.historyDialog));
elements.duplicatesButton.addEventListener('click', loadExactDuplicates);
elements.closeDuplicates.addEventListener('click', () => closeModal(elements.duplicatesDialog));
elements.similarTextsButton.addEventListener('click', () => loadSimilarContent('text'));
elements.similarTextTab.addEventListener('click', () => loadSimilarContent('text', false));
elements.similarImageTab.addEventListener('click', () => loadSimilarContent('image', false));
elements.closeSimilarTexts.addEventListener('click', () => closeModal(elements.similarTextsDialog));
elements.clearDuplicateSelection.addEventListener('click', clearDuplicateSelection);
elements.trashDuplicates.addEventListener('click', createDuplicateTrashPlan);
elements.smartFolderForm.addEventListener('submit', submitSmartFolder);
elements.closeSmartFolder.addEventListener('click', () => closeModal(elements.smartFolderDialog));
elements.cancelSmartFolder.addEventListener('click', () => closeModal(elements.smartFolderDialog));
elements.confirmUndo.addEventListener('click', confirmUndoOperation);
elements.closeUndo.addEventListener('click', () => closeModal(elements.undoDialog));
elements.cancelUndo.addEventListener('click', () => closeModal(elements.undoDialog));
elements.settingsForm.addEventListener('submit', submitSettings);
elements.settingsNavButtons.forEach((button) => {
  button.addEventListener('click', () => {
    const target = document.querySelector(`#${button.dataset.settingsTarget}`);
    if (!target) return;
    transitionUi(() => {
      elements.settingsNavButtons.forEach((item) => {
        item.classList.toggle('active', item === button);
        item.setAttribute('aria-selected', String(item === button));
      });
      elements.settingsSections.forEach((section) => { section.hidden = section !== target; });
      elements.settingsContent.scrollTop = 0;
    }, 'settings-section');
    if (target.id === 'settings-data') loadLocalDataStatus();
  });
});
elements.rebuildIndex.addEventListener('click', rebuildLibraryIndex);
elements.clearHashCache.addEventListener('click', clearLibraryHashCache);
elements.closeSettings.addEventListener('click', () => closeModal(elements.settingsDialog));
elements.cancelSettings.addEventListener('click', () => closeModal(elements.settingsDialog));
elements.notice.addEventListener('mouseenter', () => {
  if (state._noticeTimer) {
    window.clearTimeout(state._noticeTimer);
    state._noticeTimer = null;
  }
});
elements.notice.addEventListener('mouseleave', () => {
  if (!elements.notice.hidden && !elements.notice.classList.contains('busy') && !elements.notice.classList.contains('error')) {
    autoCloseNotice(5000);
  }
});
elements.undoDialog.addEventListener('close', () => {
  if (!elements.confirmUndo.disabled) state.pendingUndo = null;
});
elements.planDialog.addEventListener('close', () => {
  if (!elements.executePlan.disabled) state.currentPlan = null;
});

connect();
