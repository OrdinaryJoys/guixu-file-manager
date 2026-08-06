// Q2 进程级强杀矩阵：对写操作的关键持久化节点执行 kill -9，重启后验证收敛。
//
// 节点定义（对应 operations::fault_point）：
//   after_intent   意图日志落库后、任何文件移动前 → 重启应收敛为 not_started（文件未动）
//   after_publish  目标发布（文件已移动）后、索引同步前 → 重启应收敛为 completed（文件在目标）
//   无注入点       正常执行 → completed
//
// 前置：cargo build -p guixu-operations --features fault-injection --example kill_injection
// （脚本会自动执行该构建，避免 example 被无 feature 构建覆盖后注入点失效）
// 用法：node test-fixtures/kill-matrix.mjs
import { execSync, spawn } from 'node:child_process';
import { existsSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const REPOSITORY = join(process.cwd());
const BINARY = join(REPOSITORY, 'target/debug/examples/kill_injection');

// 构建 fault-injection 版 example：后续任何无 feature 构建都会覆盖它，每次运行前重建。
execSync('cargo build -p guixu-operations --features fault-injection --example kill_injection', {
  cwd: REPOSITORY,
  stdio: 'inherit',
});
const STAGES = ['after_intent', 'after_publish'];

function run(binary, args, env = {}) {
  return execSync(`"${binary}" ${args.join(' ')}`, { env: { ...process.env, ...env }, encoding: 'utf8' });
}

function setupFixture() {
  const dir = mkdtempSync(join(tmpdir(), 'guixu-kill-'));
  const root = join(dir, 'library');
  const db = join(dir, 'kill.sqlite3');
  mkdirSyncRecursive(root);
  writeFileSync(join(root, 'source.txt'), 'kill matrix fixture content for fault injection verification.');
  return { dir, root, db };
}

function mkdirSyncRecursive(path) {
  const parts = path.split('/');
  let current = parts[0] === '' ? '/' : '';
  for (const part of parts) {
    if (part === '') continue;
    current = join(current, part);
    if (!existsSync(current)) execSync(`mkdir -p "${current}"`);
  }
}

function killChild(child, timeoutMs = 15000) {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => { child.kill('SIGKILL'); reject(new Error('子进程未在注入点停止')); }, timeoutMs);
    child.once('exit', () => { clearTimeout(timer); resolve(); });
    child.kill('SIGKILL');
  });
}

async function main() {
  let passed = 0;
  const results = [];

  // 基线：无注入，正常执行。
  {
    const { dir, root, db } = setupFixture();
    run(BINARY, [db, root, 'execute']);
    const recover = run(BINARY, [db, root, 'recover']);
    const ok = recover.includes('status=completed') && recover.includes('source_exists=false') && recover.includes('target_exists=true');
    results.push({ stage: 'baseline(无注入)', ok, detail: recover.trim() });
    if (ok) passed += 1;
    rmSync(dir, { recursive: true, force: true });
  }

  for (const stage of STAGES) {
    const { dir, root, db } = setupFixture();
    const child = spawn(BINARY, [db, root, 'execute'], {
      env: { ...process.env, GUIXU_FAULT_POINT: stage },
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    // 等待子进程进入注入点（stderr 打印 KILL_EXECUTE 前是注入等待；等待 3 秒让其充分到达注入点）。
    await new Promise((resolve) => setTimeout(resolve, 3000));
    let killed = true;
    await killChild(child).catch((error) => {
      killed = false;
      results.push({ stage, ok: false, detail: error.message });
    });
    if (!killed) {
      // 注入点未命中（例如 example 被无 fault-injection 构建覆盖）：跳过 recover，避免重复记录。
      rmSync(dir, { recursive: true, force: true });
      continue;
    }
    const recover = run(BINARY, [db, root, 'recover']);

    // after_intent：意图已落库但文件未动 → 启动审计判定未开始，收敛为 failed（文件安全，无数据风险）。
    const ok = stage === 'after_intent'
      ? recover.includes('status=failed') && recover.includes('source_exists=true') && recover.includes('target_exists=false') && recover.includes('not_started:1')
      : recover.includes('status=completed') && recover.includes('source_exists=false') && recover.includes('target_exists=true');
    results.push({ stage, ok, detail: recover.trim() });
    if (ok) passed += 1;
    rmSync(dir, { recursive: true, force: true });
  }

  for (const result of results) {
    console.log(`${result.ok ? '✅' : '❌'} ${result.stage}: ${result.detail}`);
  }
  console.log(`\nkill-matrix: ${passed}/${results.length} 通过`);
  if (passed !== results.length) process.exit(1);
}

main().catch((error) => { console.error(error); process.exit(1); });
