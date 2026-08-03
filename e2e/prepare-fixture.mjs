// CLI 入口：npm run prepare:fixture 时重建扩展资料库（7 文件，含子目录）。
// 必须在 app 启动前执行（spec 内不得再删库，避免破坏已打开的 SQLite）。
import { prepareFixture } from './fixture.mjs';

prepareFixture({ extended: true });
