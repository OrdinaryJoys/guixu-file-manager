// Q2：可复现的 E2E fixture 准备。每个 spec 在 before() 中调用以独立重置资料库。
import { mkdirSync, rmSync, utimesSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { artifacts, library } from './paths.mjs';

/**
 * 重建隔离资料库。默认模式与原有 4 文件基线一致（native-shell spec）；
 * extended 模式追加 docs/ 与 archive/ 子目录（多文件操作 spec）。
 */
export function prepareFixture({ extended = false } = {}) {
  rmSync(artifacts, { recursive: true, force: true });
  mkdirSync(resolve(artifacts, 'runtime'), { recursive: true });
  mkdirSync(resolve(artifacts, 'screenshots'), { recursive: true });
  mkdirSync(library, { recursive: true });

  const duplicateText = '项目计划包含预算、交付时间、验收标准、风险说明和负责人信息。This project plan contains budget delivery acceptance risk and owner details.';
  writeFileSync(resolve(library, 'project-plan.md'), duplicateText);
  writeFileSync(resolve(library, 'project-plan-copy.md'), duplicateText);
  writeFileSync(resolve(library, 'contract-final.txt'), 'Contract final version. Confidential customer agreement and delivery schedule for local preview verification.');
  const notesPath = resolve(library, 'notes.json');
  writeFileSync(notesPath, '{"topic":"local file manager","status":"verified","tags":["search","smart-folder"]}');
  const oldTimestamp = new Date(Date.now() - 400 * 24 * 60 * 60 * 1000);
  utimesSync(notesPath, oldTimestamp, oldTimestamp);

  if (extended) {
    mkdirSync(resolve(library, 'docs'), { recursive: true });
    mkdirSync(resolve(library, 'archive'), { recursive: true });
    // 内容刻意差异化：alpha/beta 不应成为相似文本候选（相似对固定为 project-plan 组）。
    writeFileSync(resolve(library, 'docs', 'alpha.txt'), 'Alpha document for multi-file operations. Inventory ledger with quarterly figures and procurement codes.');
    writeFileSync(resolve(library, 'docs', 'beta.txt'), 'Beta draft meeting minutes. Action items, attendees, parking lot and next review date.');
    writeFileSync(resolve(library, 'archive', 'old.txt'), 'Old archived file that should never be touched by the default flow.');
  }
}
