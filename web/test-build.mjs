import { resolve } from 'node:path';
import { pathToFileURL, fileURLToPath } from 'node:url';
const root = fileURLToPath(new URL('../', import.meta.url));
export const buildFile = name => pathToFileURL(resolve(root, process.env.PICKAXE_BROWSER_BUILD ?? 'dist/web', name));
export const buildModule = name => import(buildFile(name));
