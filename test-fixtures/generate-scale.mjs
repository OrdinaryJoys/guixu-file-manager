// Q4 规模 fixture 生成器：固定种子、确定性输出，用于 10k/100k/1m 扫描与索引基准。
// 用法：node test-fixtures/generate-scale.mjs <输出目录> [文件数] [种子]
// 生成 manifest.json 记录参数，保证可复现；文件内容为确定性伪随机字节。
import { createHash } from 'node:crypto';
import { mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';

// SplitMix64：确定性 PRNG，避免 Math.random。
function splitmix64(seed) {
  let state = BigInt(seed) & 0xffffffffffffffffn;
  return () => {
    state = (state + 0x9e3779b97f4a7c15n) & 0xffffffffffffffffn;
    let z = state;
    z = ((z ^ (z >> 30n)) * 0xbf58476d1ce4e5b9n) & 0xffffffffffffffffn;
    z = ((z ^ (z >> 27n)) * 0x94d049bb133111ebn) & 0xffffffffffffffffn;
    return Number((z ^ (z >> 31n)) & 0xffffffffffffffffn);
  };
}

const NAME_PARTS = [
  '报告', '合同', '预算', '项目', '会议', '备忘', '设计', '数据',
  'alpha', 'beta', 'gamma', 'delta', 'report', 'contract', 'budget',
  'meeting', 'draft', 'final', '2026', 'Q1', 'Q2', 'backup', 'archive',
];
const EXTENSIONS = ['md', 'txt', 'json', 'pdf', 'csv', 'docx', 'xlsx', 'png', 'log', 'tmp'];

function pick(random, array) {
  return array[random() % array.length];
}

function randomSize(random, bucket) {
  // 分布模拟真实目录：大量小文件 + 少量大文件。
  // tiny 模式（第 4 参数）全部 < 2 KiB，供 100k/1m 磁盘受限场景。
  if (bucket === 'tiny') return 16 + (random() % 2048);
  const roll = random() % 100;
  if (roll < 70) return 16 + (random() % 2048);            // < 2 KiB
  if (roll < 90) return 2048 + (random() % 65536);         // 2 KiB - 64 KiB
  if (roll < 98) return 65536 + (random() % 1048576);      // 64 KiB - 1 MiB
  return 1048576 + (random() % 8388608);                   // 1 - 8 MiB
}

function randomPath(random, depth) {
  const parts = [];
  for (let level = 0; level < depth; level++) {
    parts.push(`${pick(random, NAME_PARTS)}-${random() % 50}`);
  }
  return parts.join('/');
}

function main() {
  const [outputDirArg, countArg, seedArg, modeArg] = process.argv.slice(2);
  const outputDir = resolve(outputDirArg ?? 'e2e/.artifacts/scale');
  const count = Number(countArg ?? 10_000);
  const seed = Number(seedArg ?? 20260803);
  const bucket = modeArg === 'tiny' ? 'tiny' : 'standard';

  const random = splitmix64(seed);
  const directorySet = new Set();

  rmSync(outputDir, { recursive: true, force: true });
  mkdirSync(outputDir, { recursive: true });

  let bytesWritten = 0;
  let filesCreated = 0;

  // 内容 = 基于文件名+块号的 SHA-256 派生，保证不同文件内容唯一。
  // （SplitMix64 的 `% 256` 低 8 位周期仅 256，会导致跨文件内容段重复。）
  function contentFor(name, block) {
    return createHash('sha256').update(`${name}#${block}`).digest();
  }

  for (let index = 0; index < count; index++) {
    // 目录深度 0-4，文件名含序号保证唯一。
    const depth = random() % 5;
    const directory = randomPath(random, depth);
    const name = `${pick(random, NAME_PARTS)}-${index}.${pick(random, EXTENSIONS)}`;
    const path = join(outputDir, directory, name);
    const parent = path.slice(0, path.lastIndexOf('/'));
    if (!directorySet.has(parent)) {
      mkdirSync(parent, { recursive: true });
      directorySet.add(parent);
    }
    const size = randomSize(random, bucket);
    let remaining = size;
    let block = 0;
    while (remaining > 0) {
      const chunk = Math.min(remaining, 32);
      writeFileSync(path, contentFor(name, block).subarray(0, chunk), { flag: 'a' });
      remaining -= chunk;
      block += 1;
      bytesWritten += chunk;
    }
    filesCreated += 1;
    if (filesCreated % 1000 === 0) {
      process.stderr.write(`  ${filesCreated}/${count} 文件\n`);
    }
  }

  const manifest = {
    generated_at: new Date().toISOString(),
    seed,
    file_count: filesCreated,
    directory_count: directorySet.size,
    total_bytes: bytesWritten,
    generator: 'test-fixtures/generate-scale.mjs',
    distribution: bucket === 'tiny' ? '100% <2KiB; depth 0-4' : '70% <2KiB, 20% 2-64KiB, 8% 64KiB-1MiB, 2% 1-8MiB; depth 0-4',
  };
  writeFileSync(join(outputDir, 'manifest.json'), JSON.stringify(manifest, null, 2));
  process.stdout.write(JSON.stringify(manifest, null, 2) + '\n');
}

main();
