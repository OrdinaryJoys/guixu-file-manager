import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

export const here = dirname(fileURLToPath(import.meta.url));
export const repository = resolve(here, '..');
export const artifacts = resolve(here, '.artifacts');
export const appBinary = resolve(repository, 'target/debug/guixu-desktop');
export const library = resolve(artifacts, 'library');
export const screenshotPath = resolve(artifacts, 'screenshots/native-library-settings.png');
