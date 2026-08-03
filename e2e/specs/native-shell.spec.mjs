import { cleanupScreenshotPath, screenshotPath } from '../paths.mjs';
import assert from 'node:assert/strict';

async function read(selector, property) {
  return browser.execute((target, key) => {
    const element = document.querySelector(target);
    if (!element) return null;
    if (key === 'text') return element.textContent.trim();
    if (key === 'open') return element.hasAttribute('open');
    if (key === 'displayed') return !element.hidden && getComputedStyle(element).display !== 'none';
    if (key === 'count') return element.children.length;
    return element[key];
  }, selector, property);
}

async function click(selector) {
  const clicked = await browser.execute((target) => {
    const element = document.querySelector(target);
    if (!element) return false;
    element.click();
    return true;
  }, selector);
  assert.equal(clicked, true, `找不到可点击元素：${selector}`);
}

async function waitForAppReady() {
  await browser.waitUntil(async () => browser.execute(() => {
    const runtime = document.querySelector('#runtime')?.textContent?.trim();
    return document.readyState === 'complete'
      && document.querySelector('#search-input') !== null
      && typeof runtime === 'string'
      && runtime.length > 0
      && !runtime.includes('正在连接');
  }), {
    timeout: 20000,
    interval: 100,
    timeoutMsg: '应用页面或本地核心未在 20 秒内准备完成',
  });
}

describe('归序原生窗口', () => {
  before(async () => {
    // fixture 已由 `npm run prepare:fixture` 在 app 启动前准备，spec 内不再删库。
    await waitForAppReady();
  });

  it('连接本地核心并完成真实资料库扫描', async () => {
    // 防回归：核心 API 必须注入（capabilities 必须含 core:default），否则前端走 demo 分支。
    assert.equal(
      await browser.execute(() => Boolean(window.__TAURI__?.core?.invoke)),
      true,
      'window.__TAURI__.core 未注入：检查 capabilities/main.json 是否含 core:default'
    );
    const runtime = await read('#runtime', 'text');
    assert.equal(typeof runtime, 'string');
    assert.match(runtime, /0\.1\.0/);
    await browser.waitUntil(async () => await read('#library-name', 'text') === 'E2E Fixture');
    await browser.waitUntil(async () => await read('#file-count', 'text') === '7 项', { timeout: 20000 });
    assert.equal(await read('#choose-folder', 'disabled'), false);
    assert.equal(await read('#duplicates-button', 'disabled'), false);
    assert.equal(await read('#similar-texts-button', 'disabled'), false);
  });

  it('搜索、预览、智能文件夹和整理计划使用真实后端数据', async () => {
    await browser.execute(() => {
      const input = document.querySelector('#search-input');
      input.value = 'contract';
      input.dispatchEvent(new Event('input', { bubbles: true }));
    });
    await browser.waitUntil(async () => await read('#file-count', 'text') === '1 项');
    assert.match(await read('#file-list', 'text'), /contract-final\.txt/);
    await click('#file-list .file-name');
    await browser.waitUntil(async () => ((await read('#preview-panel', 'text')) || '').includes('Confidential'));

    await click('#save-search');
    await browser.execute(() => {
      document.querySelector('#smart-folder-name').value = 'Contracts';
      document.querySelector('#smart-folder-form button[type="submit"]').click();
    });
    await browser.waitUntil(async () => ((await read('#smart-folders', 'text')) || '').includes('Contracts'));

    await click('#all-files-button');
    await browser.waitUntil(async () => await read('#file-count', 'text') === '7 项');
    await browser.execute(() => document.querySelector('#file-list .file-select').click());
    await click('#organize-selected');
    await browser.waitUntil(async () => await read('#plan-dialog', 'open') === true);
    assert.equal(await browser.execute(() => document.querySelectorAll('#plan-list .plan-item').length), 1);
    await click('#cancel-plan');
    await browser.waitUntil(async () => await read('#plan-dialog', 'open') === false);

    await click('#cleanup-suggestions-button');
    await browser.waitUntil(async () => ((await read('#cleanup-summary', 'text')) || '').startsWith('扫描 7 个文件'));
    assert.match(await read('#cleanup-summary', 'text'), /发现 1 个候选/);
    assert.match(await read('#cleanup-list', 'text'), /notes\.json/);
    await browser.saveScreenshot(cleanupScreenshotPath);
    await click('#close-cleanup');
    await browser.waitUntil(async () => await read('#cleanup-dialog', 'open') === false);
  });

  it('重复与相似内容后台算法回填真实报告', async () => {
    await click('#duplicates-button');
    await browser.waitUntil(async () => ((await read('#duplicates-summary', 'text')) || '').startsWith('扫描 7 个文件'), { timeout: 30000 });
    assert.ok(await browser.execute(() => document.querySelectorAll('#duplicates-list .duplicate-path').length) >= 2);
    await click('#close-duplicates');
    await browser.waitUntil(async () => await read('#duplicates-dialog', 'open') === false);

    await click('#similar-texts-button');
    await browser.waitUntil(async () => ((await read('#similar-texts-summary', 'text')) || '').includes('二次验证保留 1 对'), { timeout: 30000 });
    assert.ok(await browser.execute(() => document.querySelectorAll('#similar-texts-list .history-item').length) >= 1);
    await click('#close-similar-texts');
    await browser.waitUntil(async () => await read('#similar-texts-dialog', 'open') === false);
  });

  it('真实重命名经过计划、执行、历史并安全撤销', async () => {
    await click('#all-files-button');
    await browser.waitUntil(async () => await read('#file-count', 'text') === '7 项');
    const selected = await browser.execute(() => {
      const rows = [...document.querySelectorAll('#file-list .file-row')];
      const row = rows.find((candidate) => candidate.textContent.includes('contract-final.txt'));
      row?.querySelector('.file-select')?.click();
      return Boolean(row);
    });
    assert.equal(selected, true);
    await click('#rename-selected');
    await browser.waitUntil(async () => await read('#rename-dialog', 'open') === true);
    await browser.execute(() => {
      document.querySelector('#rename-list input').value = 'contract-renamed.txt';
      document.querySelector('#rename-form button[type="submit"]').click();
    });
    await browser.waitUntil(async () => await read('#plan-dialog', 'open') === true);
    assert.match(await read('#plan-list', 'text'), /contract-renamed\.txt/);
    await click('#execute-plan');
    await browser.waitUntil(async () => await read('#plan-dialog', 'open') === false);
    await click('#all-files-button');
    await browser.waitUntil(async () => ((await read('#file-list', 'text')) || '').includes('contract-renamed.txt'));

    await click('#history-button');
    await browser.waitUntil(async () => ((await read('#history-list', 'text')) || '').includes('已完成'));
    await click('#history-list .history-actions button');
    await browser.waitUntil(async () => await read('#undo-dialog', 'open') === true);
    await click('#confirm-undo');
    await browser.waitUntil(async () => await read('#undo-dialog', 'open') === false);
    await browser.waitUntil(async () => ((await read('#history-list', 'text')) || '').includes('已撤销'));
    await click('#close-history');
    await browser.waitUntil(async () => await read('#history-dialog', 'open') === false);
    await click('#all-files-button');
    await browser.waitUntil(async () => ((await read('#file-list', 'text')) || '').includes('contract-final.txt'));
  });

  it('设置分区可操作、保存后立即生效并可重新读取', async () => {
    await click('#settings-button');
    assert.equal(await read('#settings-dialog', 'open'), true);

    await click('#settings-tab-appearance');
    await browser.waitUntil(async () => await read('#settings-appearance', 'displayed') === true);
    await browser.execute(() => {
      const density = document.querySelector('#setting-density');
      density.value = 'compact';
      density.dispatchEvent(new Event('change', { bubbles: true }));
      const reduceMotion = document.querySelector('#setting-reduce-motion');
      reduceMotion.checked = true;
      reduceMotion.dispatchEvent(new Event('change', { bubbles: true }));
      document.querySelector('#settings-form button[type="submit"]').click();
    });

    await browser.waitUntil(async () => await read('#settings-dialog', 'open') === false);
    assert.equal(await browser.execute(() => document.body.classList.contains('compact-density')), true);
    await click('#settings-button');
    await click('#settings-tab-appearance');
    assert.equal(await read('#setting-density', 'value'), 'compact');
    assert.equal(await read('#setting-reduce-motion', 'checked'), true);
    await browser.saveScreenshot(screenshotPath);
    // 关闭设置对话框，避免残留 modal 影响后续场景。
    await click('#close-settings');
    await browser.waitUntil(async () => await read('#settings-dialog', 'open') === false);
  });
});

// Q2 / P0 文件安全：多文件批量重命名、整理、废纸篓的"计划 → 执行 → 历史 → 撤销"闭环。
// 与上一 describe 共用同一 app session（单 spec 文件强制串行，避免 artifacts 目录互删）。
describe('多文件操作闭环', () => {
  before(async () => {
    // 与上一 describe 共用已启动的 app；fixture 已含子目录（7 文件）。
    // 清搜索态并触发重新扫描，等待 7 文件基线。
    await click('#all-files-button');
    await click('#refresh-files');
    await browser.waitUntil(async () => await read('#file-count', 'text') === '7 项', {
      timeout: 30000,
      timeoutMsg: '扩展资料库未扫描到 7 项',
    });
    await waitForIdleJobs();
  });

  // 等待任务中心无运行中任务，避免扫描/对账完成时的 loadFiles(true) 清空选择。
  async function waitForIdleJobs() {
    await browser.waitUntil(async () => browser.execute(() => {
      const active = ['running', 'queued', 'pause_requested', 'cancel_requested'];
      return ![...document.querySelectorAll('.task')].some((task) =>
        active.some((status) => task.classList.contains(status)));
    }), { timeout: 30000, timeoutMsg: '后台任务未在 30 秒内空闲' });
  }

  async function pickRows(names) {
    return browser.execute((targets) => {
      const rows = [...document.querySelectorAll('#file-list .file-row')];
      let count = 0;
      for (const row of rows) {
        if (targets.some((name) => row.textContent.includes(name))) {
          row.querySelector('.file-select')?.click();
          count += 1;
        }
      }
      return count;
    }, names);
  }

  async function undoLatestOperation() {
    await click('#history-button');
    await browser.waitUntil(async () => await read('#history-dialog', 'open') === true);
    await click('#history-list .history-actions button');
    await browser.waitUntil(async () => await read('#undo-dialog', 'open') === true);
    await click('#confirm-undo');
    await browser.waitUntil(async () => await read('#undo-dialog', 'open') === false, { timeout: 30000 });
    await browser.waitUntil(async () => ((await read('#history-list', 'text')) || '').includes('已撤销'), { timeout: 30000 });
    await click('#close-history');
    await browser.waitUntil(async () => await read('#history-dialog', 'open') === false);
  }

  it('批量重命名两个文件后可从历史安全撤销', async () => {
    await waitForIdleJobs();
    assert.equal(await pickRows(['alpha.txt', 'beta.txt']), 2);
    await click('#rename-selected');
    await browser.waitUntil(async () => await read('#rename-dialog', 'open') === true);
    assert.equal(await read('#rename-list', 'count'), 2);
    await browser.execute(() => {
      const inputs = document.querySelectorAll('#rename-list input');
      inputs[0].value = 'alpha-renamed.txt';
      inputs[1].value = 'beta-renamed.txt';
      document.querySelector('#rename-form button[type="submit"]').click();
    });
    await browser.waitUntil(async () => await read('#plan-dialog', 'open') === true);
    assert.equal(await read('#plan-list', 'count'), 2);
    assert.match(await read('#plan-list', 'text'), /alpha-renamed\.txt/);
    assert.match(await read('#plan-list', 'text'), /beta-renamed\.txt/);

    await click('#execute-plan');
    await browser.waitUntil(async () => await read('#plan-dialog', 'open') === false, { timeout: 30000 });
    await click('#all-files-button');
    await browser.waitUntil(async () => ((await read('#file-list', 'text')) || '').includes('alpha-renamed.txt'), { timeout: 30000 });

    await undoLatestOperation();
    await click('#all-files-button');
    await browser.waitUntil(async () => ((await read('#file-list', 'text')) || '').includes('alpha.txt'), { timeout: 30000 });
    await browser.waitUntil(async () => await read('#file-count', 'text') === '7 项', { timeout: 30000 });
  });

  it('多文件整理（自动分类目录）执行后可撤销', async () => {
    await waitForIdleJobs();
    assert.equal(await pickRows(['alpha.txt', 'beta.txt']), 2);
    await click('#organize-selected');
    await browser.waitUntil(async () => await read('#plan-dialog', 'open') === true);
    assert.equal(await read('#plan-list', 'count'), 2);
    assert.match(await read('#plan-list', 'text'), /归序整理/);

    await click('#execute-plan');
    await browser.waitUntil(async () => await read('#plan-dialog', 'open') === false, { timeout: 30000 });
    await click('#all-files-button');
    await browser.waitUntil(async () => ((await read('#file-list', 'text')) || '').includes('alpha.txt'), { timeout: 30000 });
    await browser.waitUntil(async () => await read('#file-count', 'text') === '7 项', { timeout: 30000 });

    await undoLatestOperation();
    await browser.waitUntil(async () => await read('#file-count', 'text') === '7 项', { timeout: 30000 });
    await browser.waitUntil(async () => ((await read('#file-list', 'text')) || '').includes('alpha.txt'));
  });

  it('多文件移入受控废纸篓后可从历史恢复', async () => {
    await waitForIdleJobs();
    assert.equal(await pickRows(['old.txt', 'notes.json']), 2);
    await click('#trash-selected');
    await browser.waitUntil(async () => await read('#plan-dialog', 'open') === true);
    assert.equal(await read('#plan-list', 'count'), 2);

    await click('#execute-plan');
    await browser.waitUntil(async () => await read('#plan-dialog', 'open') === false, { timeout: 30000 });
    await click('#all-files-button');
    await browser.waitUntil(async () => await read('#file-count', 'text') === '5 项', { timeout: 30000 });
    const listText = await read('#file-list', 'text');
    assert.ok(!listText.includes('old.txt'), '废纸篓后 old.txt 不应可见');
    assert.ok(!listText.includes('notes.json'), '废纸篓后 notes.json 不应可见');

    await undoLatestOperation();
    await click('#all-files-button');
    await browser.waitUntil(async () => await read('#file-count', 'text') === '7 项', { timeout: 30000 });
    assert.ok(((await read('#file-list', 'text')) || '').includes('old.txt'), '撤销后 old.txt 应恢复');
  });
});
