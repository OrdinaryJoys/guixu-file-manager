import { mkdirSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const repository = resolve(here, '..');
const artifacts = resolve(here, '.artifacts');
const appBinary = resolve(repository, 'target/debug/guixu-desktop');

mkdirSync(resolve(artifacts, 'runtime'), { recursive: true });
mkdirSync(resolve(artifacts, 'screenshots'), { recursive: true });
process.env.GUIXU_DATA_DIR = resolve(artifacts, 'runtime');

export const config = {
  runner: 'local',
  specs: [resolve(here, 'specs/**/*.spec.mjs')],
  maxInstances: 1,
  services: [['@wdio/tauri-service', {
    appBinaryPath: appBinary,
    driverProvider: 'embedded',
    embeddedPort: 4445,
    startTimeout: 60000,
    statusPollTimeout: 5000,
  }]],
  capabilities: [{
    browserName: 'tauri',
    'tauri:options': { application: appBinary },
  }],
  logLevel: 'warn',
  waitforTimeout: 10000,
  connectionRetryTimeout: 90000,
  connectionRetryCount: 1,
  framework: 'mocha',
  reporters: ['spec'],
  mochaOpts: { ui: 'bdd', timeout: 60000 },
};

export const screenshotPath = resolve(artifacts, 'screenshots/native-settings.png');
