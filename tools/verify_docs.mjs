import { existsSync, readdirSync, readFileSync, statSync } from 'node:fs';
import { dirname, extname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const repository = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const required = [
  'README.md',
  'docs/README.md',
  'docs/DEVELOPMENT_STATUS.md',
  'docs/MASTER_PLAN.md',
  'docs/TEST_STRATEGY_2026.md',
  'docs/CROSS_VALIDATION_2026.md',
];
const errors = [];

function markdownFiles(directory) {
  return readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const path = resolve(directory, entry.name);
    if (entry.isDirectory()) return markdownFiles(path);
    return extname(entry.name) === '.md' ? [path] : [];
  });
}

for (const path of required) {
  if (!existsSync(resolve(repository, path))) errors.push(`缺少必需文档：${path}`);
}

const files = [resolve(repository, 'README.md'), ...markdownFiles(resolve(repository, 'docs'))];
const linkPattern = /!?\[[^\]]*\]\(([^)]+)\)/g;
for (const file of files) {
  const content = readFileSync(file, 'utf8');
  for (const match of content.matchAll(linkPattern)) {
    let target = match[1].trim();
    if (target.startsWith('<') && target.endsWith('>')) target = target.slice(1, -1);
    target = target.split(/\s+["']/u, 1)[0];
    if (!target || target.startsWith('#') || /^(?:https?:|mailto:)/u.test(target)) continue;
    const local = decodeURIComponent(target.split('#', 1)[0]);
    const resolved = resolve(dirname(file), local);
    if (!existsSync(resolved)) {
      errors.push(`${file.slice(repository.length + 1)}：链接不存在 ${target}`);
    } else if (target.endsWith('/') && !statSync(resolved).isDirectory()) {
      errors.push(`${file.slice(repository.length + 1)}：目录链接指向文件 ${target}`);
    }
  }
}

const currentSources = required.map((path) => readFileSync(resolve(repository, path), 'utf8')).join('\n');
for (const marker of ['91 项', '7 项', '8 场景', '3/3 通过']) {
  if (!currentSources.includes(marker)) errors.push(`当前文档缺少验证基线：${marker}`);
}
if (/\b89\s*(?:项|个)\s*Rust/u.test(currentSources)) {
  errors.push('当前事实来源仍包含过期的 89/5 项基线');
}

if (errors.length > 0) {
  errors.forEach((error) => console.error(`文档验证失败：${error}`));
  process.exitCode = 1;
} else {
  console.log(`文档验证通过：${files.length} 个 Markdown 文件，本地链接与当前基线一致。`);
}
