import { createServer } from 'node:http';
import { existsSync, readFileSync, readdirSync } from 'node:fs';
import { dirname, extname, join, relative } from 'node:path';
import { fileURLToPath } from 'node:url';

const BASE = '/dashboard';
export const PAGE_PATHS = [
  `${BASE}/`,
  `${BASE}/index/catalog.v2`,
  `${BASE}/keys`,
  `${BASE}/security-sources`,
];

function read(path) {
  try {
    return readFileSync(path, 'utf8');
  } catch {
    return '';
  }
}

function filesWithExtension(directory, extension) {
  try {
    return readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
      const path = join(directory, entry.name);
      return entry.isDirectory()
        ? filesWithExtension(path, extension)
        : extname(entry.name) === extension
          ? [path]
          : [];
    });
  } catch {
    return [];
  }
}

function referencedAssets(html) {
  return [...html.matchAll(/(?:href|src)="([^"]+\.(?:css|js))"/g)].map((match) => match[1] ?? '');
}

function localAssetPath(url) {
  return url.startsWith(`${BASE}/`) ? url.slice(BASE.length + 1) : url.replace(/^\//, '');
}

export function validateConsoleBuild(buildDirectory) {
  const findings = [];
  const indexPath = join(buildDirectory, 'index.html');
  const html = read(indexPath);
  if (!html) {
    return ['missing compiled entry: index.html'];
  }
  if (!html.includes('kit.start') || !html.includes(`base: "${BASE}"`)) {
    findings.push('compiled entry lacks SvelteKit dashboard mount');
  }

  const assetUrls = referencedAssets(html);
  const javascriptPaths = filesWithExtension(buildDirectory, '.js');
  const stylesheetPaths = filesWithExtension(buildDirectory, '.css');
  if (!assetUrls.some((path) => path.endsWith('.js'))) findings.push('compiled entry lacks JavaScript asset');
  if (stylesheetPaths.length === 0) findings.push('compiled output lacks stylesheet asset');

  for (const url of assetUrls) {
    if (!url.startsWith(`${BASE}/`)) findings.push(`compiled asset lacks dashboard base: ${url}`);
    const path = localAssetPath(url);
    if (!existsSync(join(buildDirectory, path))) {
      findings.push(`missing compiled asset: ${path}`);
    }
  }

  const javascript = javascriptPaths.map(read).join('\n');
  if (!javascript.includes('data-console-host') || !javascript.includes('standalone')) {
    findings.push('compiled JavaScript lacks standalone host marker');
  }
  if (!javascript.includes('Flapjack Console')) {
    findings.push('compiled JavaScript lacks host heading');
  }

  const styles = stylesheetPaths.map(read).join('\n');
  if (!styles.includes('data-console-theme') || !styles.includes('--console-surface')) {
    findings.push('compiled stylesheet lacks semantic theme tokens');
  }

  return [...new Set(findings)].sort();
}

export async function probeConsoleBuild(buildDirectory) {
  const server = createServer((request, response) => {
    const pathname = new URL(request.url ?? '/', 'http://localhost').pathname;
    const isDashboardRoute = pathname === BASE || pathname.startsWith(`${BASE}/`);
    const isCompiledAsset = pathname.startsWith(`${BASE}/_app/`);
    const relativePath = isDashboardRoute && !isCompiledAsset ? 'index.html' : localAssetPath(pathname);
    const contents = read(join(buildDirectory, relativePath));
    response.writeHead(contents ? 200 : 404);
    response.end(contents);
  });

  await new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', resolve);
  });
  try {
    const address = server.address();
    if (!address || typeof address === 'string') throw new Error('preview server has no TCP address');
    const origin = `http://127.0.0.1:${address.port}`;
    const expectedHtml = read(join(buildDirectory, 'index.html'));
    let html = '';
    for (const pagePath of PAGE_PATHS) {
      const pageResponse = await fetch(`${origin}${pagePath}`);
      if (!pageResponse.ok) {
        throw new Error(`preview returned ${pageResponse.status} for ${pagePath}`);
      }
      const pageHtml = await pageResponse.text();
      if (pageHtml !== expectedHtml) {
        throw new Error(`preview returned a compiled asset for client route ${pagePath}`);
      }
      html ||= pageHtml;
    }
    const assets = [
      ...referencedAssets(html),
      ...filesWithExtension(buildDirectory, '.css').map(
        (path) => `${BASE}/${relative(buildDirectory, path)}`
      ),
    ];
    for (const asset of new Set(assets)) {
      const assetResponse = await fetch(`${origin}${asset}`);
      if (!assetResponse.ok) throw new Error(`preview returned ${assetResponse.status} for ${asset}`);
      const expected = read(join(buildDirectory, localAssetPath(asset)));
      if ((await assetResponse.text()) !== expected) {
        throw new Error(`preview returned fallback HTML for compiled asset ${asset}`);
      }
    }
  } finally {
    await new Promise((resolve, reject) => server.close((error) => (error ? reject(error) : resolve())));
  }
}

const scriptPath = fileURLToPath(import.meta.url);
if (process.argv[1] === scriptPath) {
  const buildDirectory = process.argv[2] ?? join(dirname(scriptPath), '..', 'dist');
  const findings = validateConsoleBuild(buildDirectory);
  if (findings.length > 0) {
    for (const finding of findings) console.error(finding);
    process.exit(1);
  }
  await probeConsoleBuild(buildDirectory);
}
