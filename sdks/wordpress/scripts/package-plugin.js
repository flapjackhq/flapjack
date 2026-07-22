import { execFileSync } from 'node:child_process';
import {
  chmodSync,
  copyFileSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  rmSync,
  statSync,
  utimesSync,
} from 'node:fs';
import { dirname, join, relative, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

const scriptsDirectory = dirname(fileURLToPath(import.meta.url));
const wordpressDirectory = resolve(scriptsDirectory, '..');
const phpSdkDirectory = resolve(wordpressDirectory, '../php');
const distDirectory = join(wordpressDirectory, 'dist');
const stagingDirectory = join(distDirectory, '.package-staging');
const packagedPluginDirectory = join(stagingDirectory, 'flapjack-search');
const archivePath = join(distDirectory, 'flapjack-search.zip');
const fixedTimestamp = new Date(1980, 0, 1, 0, 0, 0);

function globExpression(pattern) {
  let expression = '';
  for (let index = 0; index < pattern.length; index += 1) {
    const character = pattern[index];
    if (character === '*' && pattern[index + 1] === '*') {
      expression += '.*';
      index += 1;
    } else if (character === '*') {
      expression += '[^/]*';
    } else if (character === '?') {
      expression += '[^/]';
    } else {
      expression += character.replace(/[|\\{}()[\]^$+?.]/g, '\\$&');
    }
  }
  return expression;
}

function readIgnoreRules(ignoreFileName, ignoredPatterns = new Set()) {
  return readFileSync(join(wordpressDirectory, ignoreFileName), 'utf8')
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter((line) => line && !line.startsWith('#') && !ignoredPatterns.has(line))
    .map((line) => {
      if (line.startsWith('!')) {
        throw new Error(`Unsupported negated ${ignoreFileName} rule: ${line}`);
      }
      const anchored = line.startsWith('/');
      const pattern = line.replace(/^\//, '').replace(/\/$/, '');
      const prefix = anchored || pattern.includes('/') ? '^' : '(?:^|/)';
      return new RegExp(`${prefix}${globExpression(pattern)}(?:/.*)?$`);
    });
}

function copyDirectory(sourceDirectory, destinationDirectory, shouldIgnore = () => false) {
  mkdirSync(destinationDirectory, { recursive: true });
  const entries = readdirSync(sourceDirectory, { withFileTypes: true })
    .sort((left, right) => left.name.localeCompare(right.name));

  for (const entry of entries) {
    const sourcePath = join(sourceDirectory, entry.name);
    const relativePath = relative(sourceDirectory, sourcePath).split(sep).join('/');
    if (shouldIgnore(relativePath, entry)) {
      continue;
    }
    const destinationPath = join(destinationDirectory, entry.name);
    if (entry.isSymbolicLink()) {
      throw new Error(`Refusing to package source symlink: ${sourcePath}`);
    }
    if (entry.isDirectory()) {
      copyDirectory(sourcePath, destinationPath, (childPath, childEntry) =>
        shouldIgnore(`${relativePath}/${childPath}`, childEntry));
    } else if (entry.isFile()) {
      copyFileSync(sourcePath, destinationPath);
    }
  }
}

function stagePackageSources() {
  const sourceIgnoreRules = readIgnoreRules('.distignore');
  // The package intentionally includes built frontend assets in assets/vendor/.
  // Root PHP vendor is still excluded below and rebuilt by Composer with no-dev.
  const localArtifactIgnoreRules = readIgnoreRules('.gitignore', new Set(['vendor/']));
  const ignoreRules = [...sourceIgnoreRules, ...localArtifactIgnoreRules];
  const sourceIsIgnored = (relativePath) =>
    relativePath === 'vendor'
    || relativePath.startsWith('vendor/')
    || ignoreRules.some((rule) => rule.test(relativePath));
  const phpSdkPackageRoots = new Set(['LICENSE', 'MIGRATION.md', 'README.md', 'composer.json', 'lib']);

  copyDirectory(wordpressDirectory, packagedPluginDirectory, sourceIsIgnored);
  // The mirror CI lanes intentionally track no composer.lock. Packaging uses
  // the same policy: Composer resolves a transient lock in staging, and the
  // build-only Composer files are removed before the ZIP is created.
  copyFileSync(join(wordpressDirectory, 'composer.json'), join(packagedPluginDirectory, 'composer.json'));
  copyDirectory(phpSdkDirectory, join(stagingDirectory, 'php'), (relativePath) =>
    !phpSdkPackageRoots.has(relativePath.split('/')[0]));
}

function installProductionDependencies() {
  const containerScript = String.raw`
cleanup_permissions() { chown -R "$HOST_UID:$HOST_GID" /workspace; }
trap cleanup_permissions EXIT
export DEBIAN_FRONTEND=noninteractive
apt-get update >/dev/null
apt-get install -y --no-install-recommends unzip >/dev/null
php -r 'copy("https://composer.github.io/installer.sig", "/tmp/composer.sig"); copy("https://getcomposer.org/installer", "/tmp/composer-setup.php");'
expected=$(cat /tmp/composer.sig)
actual=$(php -r 'echo hash_file("sha384", "/tmp/composer-setup.php");')
test "$expected" = "$actual"
php /tmp/composer-setup.php --install-dir=/tmp --filename=composer --quiet
COMPOSER_MIRROR_PATH_REPOS=1 php /tmp/composer install --working-dir=/workspace/flapjack-search --no-interaction --no-dev --prefer-dist --optimize-autoloader --quiet
test ! -L /workspace/flapjack-search/vendor/flapjackhq/flapjack-search-php
rm -f /workspace/flapjack-search/composer.json /workspace/flapjack-search/composer.lock
`;

  execFileSync('docker', [
    'run', '--rm',
    '-e', `HOST_UID=${process.getuid()}`,
    '-e', `HOST_GID=${process.getgid()}`,
    '-v', `${stagingDirectory}:/workspace`,
    '-w', '/workspace/flapjack-search',
    'php:8.1-cli', 'sh', '-eu', '-c', containerScript,
  ], { stdio: 'inherit' });
}

function normalizeTree(path) {
  const metadata = statSync(path);
  const paths = [path];
  if (metadata.isDirectory()) {
    const entries = readdirSync(path, { withFileTypes: true })
      .sort((left, right) => left.name.localeCompare(right.name));
    for (const entry of entries) {
      const childPath = join(path, entry.name);
      if (entry.isSymbolicLink()) {
        throw new Error(`Refusing to package generated symlink: ${childPath}`);
      }
      paths.push(...normalizeTree(childPath));
    }
  }
  const isExecutable = (metadata.mode & 0o111) !== 0;
  const mode = metadata.isDirectory() || isExecutable ? 0o755 : 0o644;
  chmodSync(path, mode);
  utimesSync(path, fixedTimestamp, fixedTimestamp);
  return paths;
}

function createArchive() {
  const manifest = normalizeTree(packagedPluginDirectory)
    .map((path) => relative(stagingDirectory, path).split(sep).join('/') + (statSync(path).isDirectory() ? '/' : ''))
    .sort()
    .join('\n');
  execFileSync('zip', ['-X', '-q', archivePath, '-@'], {
    cwd: stagingDirectory,
    input: `${manifest}\n`,
    stdio: ['pipe', 'inherit', 'inherit'],
  });
}

rmSync(distDirectory, { recursive: true, force: true });
mkdirSync(stagingDirectory, { recursive: true });
try {
  stagePackageSources();
  installProductionDependencies();
  createArchive();
  console.log(`Packaged ${relative(wordpressDirectory, archivePath)}`);
} finally {
  rmSync(stagingDirectory, { recursive: true, force: true });
}
