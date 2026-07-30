import { mkdirSync, rmSync, utimesSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { artifacts, library } from './paths.mjs';

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
