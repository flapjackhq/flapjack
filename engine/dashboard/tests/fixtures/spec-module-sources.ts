import fs from 'node:fs';
import path from 'node:path';

export interface ModuleSource {
  filePath: string;
  source: string;
}

export type ModuleSourceResolver = (
  specifier: string,
  containingFilePath: string,
) => ModuleSource | undefined;

const MODULE_FILE_EXTENSIONS = ['.ts', '.tsx'];

/**
 * Reads the source of a relatively imported module so structural spec analysis can inspect
 * helper bodies wherever they live. Package imports have no analyzable source here, so they
 * resolve to `undefined` and are left alone.
 */
export const resolveRelativeModuleSource: ModuleSourceResolver = (
  specifier,
  containingFilePath,
) => {
  if (!specifier.startsWith('.')) {
    return undefined;
  }

  const importedPath = path.resolve(path.dirname(containingFilePath), specifier);
  const candidates = [
    importedPath,
    ...MODULE_FILE_EXTENSIONS.map((extension) => `${importedPath}${extension}`),
    ...MODULE_FILE_EXTENSIONS.map((extension) => path.join(importedPath, `index${extension}`)),
  ];
  const filePath = candidates.find(
    (candidate) => fs.existsSync(candidate) && fs.statSync(candidate).isFile(),
  );

  return filePath ? { filePath, source: fs.readFileSync(filePath, 'utf8') } : undefined;
};
