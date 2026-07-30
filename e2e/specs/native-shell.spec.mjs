import { cleanupScreenshotPath, screenshotPath } from '../paths.mjs';
import assert from 'node:assert/strict';

async function read(selector, property) {
  return browser.execute((target, key) => {
    const element = document.querySelector(target);
    if (!element) return null;
    if (key === 'text') return element.textContent.trim();
    if (key === 'open') return element.hasAttribute('open');
    if (key === 'displayed') return !element.hidden && getComputedStyle(element).display !== 'none';
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
    await waitForAppReady();
  });

  it('连接本地核心并完成真实资料库扫描', async () => {
    const runtime = await read('#runtime', 'text');
    assert.equal(typeof runtime, 'string');
    assert.match(runtime, /0\.1\.0/);
    await browser.waitUntil(async () => await read('#library-name', 'text') === 'E2E Fixture');
    await browser.waitUntil(async () => await read('#file-count', 'text') === '4 项', { timeout: 20000 });
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
    await browser.waitUntil(async () => await read('#file-count', 'text') === '4 项');
    await browser.execute(() => document.querySelector('#file-list .file-select').click());
    await click('#organize-selected');
    await browser.waitUntil(async () => await read('#plan-dialog', 'open') === true);
    assert.equal(await browser.execute(() => document.querySelectorAll('#plan-list .plan-item').length), 1);
    await click('#cancel-plan');
    await browser.waitUntil(async () => await read('#plan-dialog', 'open') === false);

    await click('#cleanup-suggestions-button');
    await browser.waitUntil(async () => ((await read('#cleanup-summary', 'text')) || '').startsWith('扫描 4 个文件'));
    assert.match(await read('#cleanup-summary', 'text'), /发现 1 个候选/);
    assert.match(await read('#cleanup-list', 'text'), /notes\.json/);
    await browser.saveScreenshot(cleanupScreenshotPath);
    await click('#close-cleanup');
    await browser.waitUntil(async () => await read('#cleanup-dialog', 'open') === false);
  });

  it('重复与相似内容后台算法回填真实报告', async () => {
    await click('#duplicates-button');
    await browser.waitUntil(async () => ((await read('#duplicates-summary', 'text')) || '').startsWith('扫描 4 个文件'), { timeout: 30000 });
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
    await browser.waitUntil(async () => await read('#file-count', 'text') === '4 项');
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
  });
});
