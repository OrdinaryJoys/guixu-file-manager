const state = { planId: null, root: '', rule: 'type', moves: [], rules: [], automations: [], automationRuns: [], workspaceMode: 'select' };
const $ = (selector) => document.querySelector(selector);
const colors = { 图片: '#e1f2ff', 文档: '#fbe9d1', 音视频: '#e8e0fb', 压缩包: '#ffe3e0', 代码: '#dff4e6', 其他文件: '#edf0ed' };
const routes = {
  workspace: { title: '整理工作台', eyebrow: 'LOCAL ORGANIZER' },
  rules: { title: '整理规则', eyebrow: 'RULE LIBRARY', element: 'rulesPage' },
  insights: { title: '空间分析', eyebrow: 'FILE INSIGHTS', element: 'insightsPage' },
  automations: { title: '自动化', eyebrow: 'CONTROLLED AUTOMATION', element: 'automationsPage' },
  history: { title: '操作记录', eyebrow: 'ACTIVITY LOG', element: 'history' },
};
const routePanels = ['folderPanel', 'analysisPanel', 'rulesPage', 'insightsPage', 'automationsPage', 'history'];

async function api(path, { method = 'GET', body } = {}) {
  const response = await fetch(`/api/${path}`, {
    method, headers: body ? { 'Content-Type': 'application/json' } : {}, body: body ? JSON.stringify(body) : undefined,
  });
  const data = await response.json();
  if (!response.ok) throw new Error(data.error || '请求失败');
  return data;
}
const escapeHtml = (value) => String(value).replace(/[&<>"']/g, (char) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[char]));
const bytes = (value) => value < 1024 ? `${value} B` : value < 1048576 ? `${(value / 1024).toFixed(1)} KB` : `${(value / 1048576).toFixed(1)} MB`;
function toast(message) { const element = $('#toast'); element.textContent = message; element.classList.add('show'); clearTimeout(window.toastTimer); window.toastTimer = setTimeout(() => element.classList.remove('show'), 3500); }

async function showRoute(routeName, { updateHistory = true } = {}) {
  const name = routes[routeName] ? routeName : 'workspace';
  const route = routes[name];
  routePanels.forEach((id) => $(`#${id}`).classList.add('hidden'));
  const targetId = name === 'workspace' ? (state.workspaceMode === 'analysis' ? 'analysisPanel' : 'folderPanel') : route.element;
  $(`#${targetId}`).classList.remove('hidden');
  document.querySelectorAll('.nav-item').forEach((item) => item.classList.toggle('active', item.getAttribute('href') === `#${name}`));
  $('#pageTitle').textContent = route.title;
  $('#pageEyebrow').textContent = route.eyebrow;
  document.title = `${route.title} · 归序`;
  if (updateHistory && window.location.hash !== `#${name}`) window.history.pushState({ route: name }, '', `#${name}`);
  window.scrollTo({ top: 0, behavior: 'auto' });
  if (name === 'rules') await loadRules();
  if (name === 'automations') await loadAutomations();
  if (name === 'history') await renderHistory();
}

function renderPlan() {
  $('#moveCount').textContent = `${state.moves.length} 项待移动`;
  $('#executeTitle').textContent = state.moves.length ? `已生成 ${state.moves.length} 项安全计划` : '无需整理';
  $('#executeSub').textContent = state.moves.length ? '执行前会再次校验文件是否被更改，计划 15 分钟内有效' : '当前范围内没有命中规则的文件';
  $('#organizeButton').disabled = !state.moves.length;
  $('#previewList').innerHTML = state.moves.map((move) => {
    const ext = move.name.includes('.') ? move.name.split('.').pop().slice(0, 4).toUpperCase() : 'FILE';
    const destination = move.destination || move.category;
    const matched = move.matched_rule ? ` · ${escapeHtml(move.matched_rule)}` : '';
    return `<div class="file-row"><span class="type-chip" style="background:${colors[move.category] || '#edf0ed'}">${escapeHtml(ext)}</span><div><div class="file-name">${escapeHtml(move.name)}</div><div class="file-meta">${bytes(move.size)} · ${new Date(move.modified * 1000).toLocaleDateString('zh-CN')}${matched}</div></div><span class="arrow">→</span><span class="destination">${escapeHtml(destination)}/</span></div>`;
  }).join('');
  $('#emptyPreview').classList.toggle('hidden', state.moves.length !== 0);
}

async function scan() {
  const path = $('#folderPath').value.trim() || state.root;
  if (!path) return toast('请先选择要整理的文件夹');
  try {
    const result = await api('plan', { method: 'POST', body: {
      path, rule: state.rule, recursive: !$('#skipFolders').checked, ignore_hidden: $('#ignoreHidden').checked,
    }});
    state.planId = result.plan_id; state.root = result.root; state.moves = result.moves;
    $('#folderPath').value = result.root;
    $('#folderName').textContent = result.root.split('/').filter(Boolean).pop() || result.root;
    $('#fileCount').textContent = `${result.file_count} 个文件`;
    state.workspaceMode = 'analysis';
    await showRoute('workspace', { updateHistory: false });
    $('#statusText').textContent = '本地服务已连接'; document.querySelector('.status-pill i').style.background = '#74ae38';
    renderPlan(); await renderHistory();
  } catch (error) { toast(error.message); }
}

async function chooseFolder(trigger = $('#pickFolder')) {
  const original = trigger.innerHTML;
  trigger.disabled = true; trigger.textContent = '正在打开…';
  try {
    const result = await api('pick-folder', { method: 'POST', body: {} });
    if (result.cancelled) return;
    $('#folderPath').value = result.path;
    await scan();
  } catch (error) { toast(error.message); }
  finally { trigger.disabled = false; trigger.innerHTML = original; }
}

async function execute() {
  if (!state.planId) return;
  const button = $('#organizeButton'); button.disabled = true; button.textContent = '正在整理…';
  try { const result = await api('execute', { method: 'POST', body: { plan_id: state.planId } }); toast(`整理完成：移动 ${result.moved} 个文件${result.failed.length ? `，${result.failed.length} 个失败` : ''}`); await scan(); }
  catch (error) { toast(error.message); }
  finally { button.innerHTML = '确认并整理 <span>→</span>'; }
}

async function renderHistory() {
  try {
    const { items } = await api('history');
    const statusText = { completed: '已完成', partial: '部分完成', failed: '失败', undone: '已撤销', undo_partial: '部分撤销' };
    $('#historyList').innerHTML = items.length ? items.map((item) => {
      const canUndo = ['completed', 'partial', 'undo_partial'].includes(item.status);
      const action = canUndo ? `<button class="text-button undo" data-id="${escapeHtml(item.id)}">撤销</button>` : `<span class="run-status">${escapeHtml(statusText[item.status] || item.status)}</span>`;
      return `<div class="history-row"><span>整理了 ${item.moved.length} 个文件 · ${escapeHtml(statusText[item.status] || item.status)}</span><span class="file-meta">${escapeHtml(item.at)}</span>${action}</div>`;
    }).join('') : '<div class="empty-preview">还没有整理记录</div>';
  } catch (error) { console.warn(error); }
}
async function undo(id) { try { const result = await api('undo', { method: 'POST', body: { transaction_id: id } }); toast(`已撤销 ${result.reverted} 个文件`); await renderHistory(); if (state.root) await scan(); } catch (error) { toast(error.message); } }

function ruleDescription(rule) {
  const c = rule.conditions; const tests = [];
  if (c.extensions?.length) tests.push(c.extensions.join(', '));
  if (c.name_contains) tests.push(`名称含“${c.name_contains}”`);
  if (c.min_size) tests.push(`≥ ${bytes(c.min_size)}`);
  if (c.older_than_days) tests.push(`存放 ≥ ${c.older_than_days} 天`);
  return tests.join(' · ') || '匹配所有文件';
}
function renderRules() {
  const list = $('#ruleList');
  list.innerHTML = state.rules.length ? state.rules.map((rule) => `<article class="rule-card-row ${rule.enabled ? '' : 'disabled'}"><div class="rule-top"><div><h3>${escapeHtml(rule.name)}</h3><p>${escapeHtml(ruleDescription(rule))}</p></div><span class="move-count">P${rule.priority}</span></div><div class="rule-badges"><span class="rule-badge">→ ${escapeHtml(rule.action.destination)}</span><span class="rule-badge">${rule.mode === 'auto' ? '自动队列' : '预览后执行'}</span>${rule.enabled ? '' : '<span class="rule-badge">已停用</span>'}</div><div class="rule-actions"><button class="text-button edit-rule" data-id="${rule.id}">编辑</button><button class="text-button toggle-rule" data-id="${rule.id}">${rule.enabled ? '停用' : '启用'}</button><button class="text-button delete-rule" data-id="${rule.id}">删除</button></div></article>`).join('') : '<div class="panel empty-preview">还没有规则。新建一条规则后，在工作台选择“自定义规则”即可使用。</div>';
}
async function loadRules() { try { const { items } = await api('rules'); state.rules = items; renderRules(); } catch (error) { toast(error.message); } }
function resetRuleForm(rule = null) {
  const form = $('#ruleForm'); form.reset(); $('#ruleId').value = rule?.id || ''; $('#ruleFormTitle').textContent = rule ? '编辑规则' : '新建规则';
  $('#ruleName').value = rule?.name || ''; $('#rulePriority').value = rule?.priority ?? 100; $('#ruleMode').value = rule?.mode || 'preview'; $('#ruleEnabled').checked = rule?.enabled ?? true;
  $('#ruleExtensions').value = rule?.conditions.extensions?.join(', ') || ''; $('#ruleNameContains').value = rule?.conditions.name_contains || ''; $('#ruleMinSize').value = rule?.conditions.min_size || ''; $('#ruleOlderDays').value = rule?.conditions.older_than_days || ''; $('#ruleDestination').value = rule?.action.destination || '';
  form.classList.remove('hidden'); $('#ruleName').focus();
}
function formRule() { return { name: $('#ruleName').value, enabled: $('#ruleEnabled').checked, priority: $('#rulePriority').value, mode: $('#ruleMode').value, conditions: { extensions: $('#ruleExtensions').value.split(',').map((value) => value.trim()).filter(Boolean), name_contains: $('#ruleNameContains').value, min_size: $('#ruleMinSize').value, older_than_days: $('#ruleOlderDays').value }, action: { destination: $('#ruleDestination').value } }; }
async function saveRule(event) { event.preventDefault(); try { const id = $('#ruleId').value; await api(id ? `rules/${id}` : 'rules', { method: id ? 'PUT' : 'POST', body: formRule() }); $('#ruleForm').classList.add('hidden'); toast('规则已保存'); await loadRules(); } catch (error) { toast(error.message); } }
async function modifyRule(event) { const button = event.target.closest('button'); if (!button) return; const rule = state.rules.find((item) => item.id === button.dataset.id); try { if (button.classList.contains('edit-rule')) resetRuleForm(rule); if (button.classList.contains('toggle-rule')) { await api(`rules/${rule.id}`, { method: 'PUT', body: { ...rule, enabled: !rule.enabled } }); await loadRules(); } if (button.classList.contains('delete-rule')) { if (confirm(`删除规则“${rule.name}”？`)) { await api(`rules/${rule.id}`, { method: 'DELETE' }); await loadRules(); } } } catch (error) { toast(error.message); } }

function renderAnalysis(result) {
  const reclaimable = result.duplicates.reduce((sum, group) => sum + group.reclaimable, 0);
  $('#statScanned').textContent = result.scanned;
  $('#statDuplicates').textContent = result.duplicates.length;
  $('#statReclaimable').textContent = bytes(reclaimable);
  $('#statWarnings').textContent = result.empty.length + result.suspicious.length;
  $('#duplicateResults').innerHTML = result.duplicates.length ? result.duplicates.map((group) => `<article class="duplicate-group"><header><strong>${group.files.length} 个相同文件</strong><span>${bytes(group.reclaimable)} 可回收</span></header>${group.files.map((path) => `<code>${escapeHtml(path)}</code>`).join('')}</article>`).join('') : '<div class="empty-preview">没有发现精确重复文件</div>';
  $('#largestResults').innerHTML = result.largest.length ? result.largest.map((file) => `<div class="result-row"><strong>${escapeHtml(file.path)}</strong><span>${bytes(file.size)}</span></div>`).join('') : '<div class="empty-preview">没有文件</div>';
  const warnings = [
    ...result.empty.map((path) => ({ path, detail: '空文件 · 0 B' })),
    ...result.suspicious.map((item) => ({ path: item.path, detail: `扩展名 .${item.extension}，内容可能为 ${item.detected.join('/')}` })),
  ];
  $('#warningResults').innerHTML = warnings.length ? warnings.map((item) => `<div class="result-row"><strong>${escapeHtml(item.path)}</strong><small class="warning">${escapeHtml(item.detail)}</small></div>`).join('') : '<div class="empty-preview">没有发现异常项</div>';
}
async function runAnalysis() {
  const path = $('#folderPath').value.trim() || state.root;
  if (!path) return toast('请先在工作台输入文件夹路径');
  const button = $('#runAnalysis'); button.disabled = true; button.textContent = '正在分析…';
  try {
    const result = await api('analyze', { method: 'POST', body: { path, recursive: !$('#skipFolders').checked, ignore_hidden: $('#ignoreHidden').checked } });
    renderAnalysis(result); toast(`分析完成：扫描 ${result.scanned} 个文件`);
  } catch (error) { toast(error.message); }
  finally { button.disabled = false; button.textContent = '开始分析'; }
}

function renderAutomations() {
  $('#automationList').innerHTML = state.automations.length ? state.automations.map((task) => `<article class="rule-card-row ${task.enabled ? '' : 'disabled'}"><div class="rule-top"><div><h3><i class="status-dot ${task.enabled ? '' : 'off'}"></i>${escapeHtml(task.name)}</h3><p class="automation-path">${escapeHtml(task.path)}</p></div><span class="move-count">${task.kind === 'watch' ? '监听' : '定时'}</span></div><div class="rule-badges"><span class="rule-badge">每 ${task.interval_seconds} 秒</span><span class="rule-badge">${task.mode === 'auto' ? '自动执行' : '等待确认'}</span><span class="rule-badge">${task.last_status || '尚未运行'}</span></div><div class="rule-actions"><button class="text-button run-automation" data-id="${task.id}">立即检查</button><button class="text-button edit-automation" data-id="${task.id}">编辑</button><button class="text-button toggle-automation" data-id="${task.id}">${task.enabled ? '停用' : '启用'}</button><button class="text-button delete-automation" data-id="${task.id}">删除</button></div></article>`).join('') : '<div class="panel empty-preview">还没有自动化任务</div>';
  $('#automationRuns').innerHTML = state.automationRuns.length ? state.automationRuns.map((run) => `<div class="result-row"><div><strong>${escapeHtml(run.status)} · 命中 ${run.matched} · 移动 ${run.moved}</strong><small>${escapeHtml(run.at)}${run.detail ? ` · ${escapeHtml(run.detail)}` : ''}</small></div>${run.plan_id && run.status === 'preview_ready' ? `<button class="text-button apply-automation-plan" data-plan="${run.plan_id}">确认执行</button>` : `<span class="run-status ${run.status === 'error' ? 'error' : ''}">${escapeHtml(run.status)}</span>`}</div>`).join('') : '<div class="empty-preview">还没有运行记录</div>';
}
async function loadAutomations() { try { const data = await api('automations'); state.automations = data.items; state.automationRuns = data.runs; renderAutomations(); } catch (error) { toast(error.message); } }
function resetAutomationForm(task = null) {
  $('#automationForm').reset(); $('#automationId').value = task?.id || ''; $('#automationFormTitle').textContent = task ? '编辑任务' : '新建任务';
  $('#automationName').value = task?.name || ''; $('#automationPath').value = task?.path || state.root || $('#folderPath').value; $('#automationKind').value = task?.kind || 'watch'; $('#automationMode').value = task?.mode || 'preview'; $('#automationInterval').value = task?.interval_seconds || 60; $('#automationRecursive').checked = task?.recursive || false; $('#automationEnabled').checked = task?.enabled ?? true;
  $('#automationForm').classList.remove('hidden'); $('#automationName').focus();
}
function formAutomation() { return { name: $('#automationName').value, path: $('#automationPath').value, kind: $('#automationKind').value, mode: $('#automationMode').value, interval_seconds: $('#automationInterval').value, recursive: $('#automationRecursive').checked, ignore_hidden: true, enabled: $('#automationEnabled').checked }; }
async function saveAutomation(event) { event.preventDefault(); const body = formAutomation(); if (body.mode === 'auto' && !confirm('自动执行会在满足规则时直接移动文件。确认启用？')) return; try { const id = $('#automationId').value; await api(id ? `automations/${id}` : 'automations', { method: id ? 'PUT' : 'POST', body }); $('#automationForm').classList.add('hidden'); toast('自动化任务已保存'); await loadAutomations(); } catch (error) { toast(error.message); } }
async function modifyAutomation(event) {
  const button = event.target.closest('button'); if (!button) return;
  try {
    if (button.classList.contains('apply-automation-plan')) { const result = await api('execute', { method: 'POST', body: { plan_id: button.dataset.plan } }); toast(`已移动 ${result.moved} 个文件`); await loadAutomations(); return; }
    const task = state.automations.find((item) => item.id === button.dataset.id); if (!task) return;
    if (button.classList.contains('edit-automation')) resetAutomationForm(task);
    if (button.classList.contains('toggle-automation')) { await api(`automations/${task.id}`, { method: 'PUT', body: { ...task, enabled: !task.enabled } }); await loadAutomations(); }
    if (button.classList.contains('run-automation')) { const result = await api(`automations/${task.id}/run`, { method: 'POST', body: {} }); toast(`检查完成：${result.status}，命中 ${result.matched}`); await loadAutomations(); }
    if (button.classList.contains('delete-automation') && confirm(`删除任务“${task.name}”？`)) { await api(`automations/${task.id}`, { method: 'DELETE' }); await loadAutomations(); }
  } catch (error) { toast(error.message); }
}

$('#pickFolder').onclick = () => chooseFolder(); $('#changeFolder').onclick = () => chooseFolder($('#changeFolder')); $('#rescan').onclick = scan; $('#organizeButton').onclick = execute;
$('#ruleGrid').onclick = (event) => { const card = event.target.closest('.rule-card'); if (!card) return; document.querySelectorAll('.rule-card').forEach((item) => item.classList.toggle('selected', item === card)); state.rule = card.dataset.rule; state.root && scan(); };
$('#skipFolders').onchange = () => state.root && scan(); $('#ignoreHidden').onchange = () => state.root && scan(); $('#helpButton').onclick = () => $('#helpDialog').showModal(); $('#closeHelp').onclick = () => $('#helpDialog').close();
$('#historyList').onclick = (event) => { const button = event.target.closest('.undo'); if (button) undo(button.dataset.id); }; $('#newRule').onclick = () => resetRuleForm(); $('#cancelRule').onclick = () => $('#ruleForm').classList.add('hidden'); $('#ruleForm').onsubmit = saveRule; $('#ruleList').onclick = modifyRule;
$('#runAnalysis').onclick = runAnalysis;
$('#newAutomation').onclick = () => resetAutomationForm(); $('#cancelAutomation').onclick = () => $('#automationForm').classList.add('hidden'); $('#automationForm').onsubmit = saveAutomation; $('#automationList').onclick = modifyAutomation; $('#automationRuns').onclick = modifyAutomation;
document.querySelectorAll('.nav-item, .brand').forEach((item) => { item.onclick = (event) => { event.preventDefault(); showRoute(item.hash.slice(1)); }; });
window.addEventListener('popstate', () => showRoute(window.location.hash.slice(1), { updateHistory: false }));
showRoute(window.location.hash.slice(1) || 'workspace', { updateHistory: false });
