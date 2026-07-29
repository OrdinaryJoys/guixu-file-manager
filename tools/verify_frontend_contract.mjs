import { readFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(fileURLToPath(new URL('..', import.meta.url)));
const [html, js, css, rust, tauriConfig, launcher] = await Promise.all([
  readFile(resolve(root, 'apps/desktop/web/index.html'), 'utf8'),
  readFile(resolve(root, 'apps/desktop/web/app.js'), 'utf8'),
  readFile(resolve(root, 'apps/desktop/web/app.css'), 'utf8'),
  readFile(resolve(root, 'apps/desktop/src-tauri/src/lib.rs'), 'utf8'),
  readFile(resolve(root, 'apps/desktop/src-tauri/tauri.conf.json'), 'utf8'),
  readFile(resolve(root, 'start.command'), 'utf8'),
]);

const failures = [];
const check = (condition, message) => { if (!condition) failures.push(message); };
const unique = (values) => [...new Set(values)];

const htmlIds = [...html.matchAll(/\bid="([^"]+)"/g)].map((match) => match[1]);
const duplicateIds = unique(htmlIds.filter((id, index) => htmlIds.indexOf(id) !== index));
check(duplicateIds.length === 0, `HTML 存在重复 id：${duplicateIds.join(', ')}`);

const referencedIds = unique([
  ...js.matchAll(/querySelector(?:All)?\(\s*['"]#([^'"\s>+~.[\]]+)/g),
].map((match) => match[1]));
const missingIds = referencedIds.filter((id) => !htmlIds.includes(id));
check(missingIds.length === 0, `app.js 引用了不存在的 DOM id：${missingIds.join(', ')}`);

const settingsTargets = unique([...html.matchAll(/data-settings-target="([^"]+)"/g)].map((match) => match[1]));
const missingTargets = settingsTargets.filter((id) => !htmlIds.includes(id));
check(missingTargets.length === 0, `设置页目标不存在：${missingTargets.join(', ')}`);

const invokedCommands = unique([
  ...[...js.matchAll(/\binvoke\(\s*['"]([a-z0-9_]+)['"]/g)].map((match) => match[1]),
  'execute_organize_plan',
  'execute_rename_plan',
  'execute_copy_plan',
  'execute_move_plan',
  'execute_trash_plan',
]);
const handlerBlock = rust.match(/generate_handler!\[([\s\S]*?)\]\)/)?.[1] || '';
const registeredCommands = unique(handlerBlock.match(/[a-z][a-z0-9_]*/g) || []);
const missingCommands = invokedCommands.filter((command) => !registeredCommands.includes(command));
check(missingCommands.length === 0, `前端命令未在 Tauri 注册：${missingCommands.join(', ')}`);

const declaredVariables = unique([...css.matchAll(/--([a-z0-9-]+)\s*:/g)].map((match) => match[1]));
const usedVariables = unique([...css.matchAll(/var\(--([a-z0-9-]+)/g)].map((match) => match[1]));
const missingVariables = usedVariables.filter((name) => !declaredVariables.includes(name));
check(missingVariables.length === 0, `CSS 使用了未声明令牌：${missingVariables.join(', ')}`);
const selfReferences = unique([...css.matchAll(/--([a-z0-9-]+)\s*:\s*var\(--\1\)/g)].map((match) => match[1]));
check(selfReferences.length === 0, `CSS 令牌发生自引用：${selfReferences.join(', ')}`);
const cssBraceDelta = [...css].reduce((depth, character) => {
  if (character === '{') return depth + 1;
  if (character === '}') return depth - 1;
  return depth;
}, 0);
check(cssBraceDelta === 0, `CSS 大括号不平衡：净差 ${cssBraceDelta}`);

const config = JSON.parse(tauriConfig);
check(config.build?.frontendDist === '../web', 'Tauri frontendDist 必须指向唯一前端 ../web');
check(launcher.includes('apps/desktop/web'), 'start.command 必须启动与 Tauri 相同的前端目录');
check(launcher.includes('?demo=1'), '浏览器启动器必须显式启用 demo 适配层');

if (failures.length) {
  console.error(failures.map((failure) => `- ${failure}`).join('\n'));
  process.exitCode = 1;
} else {
  console.log(`前端契约验证通过：${htmlIds.length} 个 DOM id、${invokedCommands.length} 个 IPC 命令、${declaredVariables.length} 个 CSS 令牌。`);
}
