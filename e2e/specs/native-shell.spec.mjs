import { screenshotPath } from '../wdio.conf.mjs';
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
  return browser.execute((target) => document.querySelector(target)?.click(), selector);
}

describe('归序原生窗口', () => {
  it('连接本地核心并保持无资料库安全初始态', async () => {
    await browser.waitUntil(async () => !((await read('#runtime', 'text')) || '').includes('正在连接'));
    assert.match(await read('#runtime', 'text'), /0\.1\.0/);
    assert.equal(await read('#choose-folder', 'disabled'), false);
    assert.equal(await read('#duplicates-button', 'disabled'), true);
    assert.equal(await read('#similar-texts-button', 'disabled'), true);
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
