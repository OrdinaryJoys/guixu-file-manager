import { mkdirSync } from 'node:fs';
import { resolve } from 'node:path';
import { appBinary, artifacts, here, library } from './paths.mjs';

mkdirSync(resolve(artifacts, 'runtime'), { recursive: true });
mkdirSync(resolve(artifacts, 'screenshots'), { recursive: true });
process.env.GUIXU_DATA_DIR = resolve(artifacts, 'runtime');
process.env.GUIXU_E2E_LIBRARY = library;

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
