/**
 * Local instance configuration for development and testing.
 * Reads port/host settings from flapjack.local.conf for per-clone isolation.
 */
import fs from 'node:fs';
import path from 'node:path';
import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

const REPO_ROOT = path.resolve(__dirname, '..', '..');
const LOCAL_CONFIG_PATH = path.join(REPO_ROOT, 'flapjack.local.conf');
const MULTI_INSTANCE_STATE_DIR = path.join(process.env.TMPDIR || '/tmp', 'flapjack-multi-instance');
const DEFAULT_BACKEND_DATA_DIR = path.join(REPO_ROOT, 'engine', 'data');

const DEFAULTS = {
  host: '127.0.0.1',
  backendPort: 7700,
  dashboardPort: 5177,
  adminKey: 'fj_devtestadminkey000000',
} as const;

const LOOPBACK_HOSTS = new Set(['127.0.0.1', 'localhost', '::1', '[::1]']);
const LOCAL_BINDABLE_HOSTS = new Set(['127.0.0.1', 'localhost', '::1', '0.0.0.0', '::']);

interface ResolveAdminKeyOptions {
  loopbackProcessAdminKey?: string;
  processListOutput?: string;
}

export interface LocalInstanceConfig {
  host: string;
  /**
   * Backend target host the dashboard/Playwright layer connects to. Distinct from
   * `host`, which stays the dashboard/Vite bind host that builds `dashboardBaseUrl`.
   */
  backendHost: string;
  backendPort: number;
  dashboardPort: number;
  adminKey: string;
  backendBaseUrl: string;
  backendDataDir: string;
  dashboardBaseUrl: string;
  configPath: string;
  loadedFromFile: boolean;
}

/** Parses a shell-style config file into key-value pairs. */
export function parseLocalConfigFile(contents: string): Record<string, string> {
  const parsed: Record<string, string> = {};
  for (const rawLine of contents.split('\n')) {
    const line = rawLine.trim();
    if (!line || line.startsWith('#')) {
      continue;
    }
    const assignment = line.startsWith('export ') ? line.slice('export '.length).trim() : line;
    const equalsAt = assignment.indexOf('=');
    if (equalsAt <= 0) {
      continue;
    }
    const key = assignment.slice(0, equalsAt).trim();
    let value = assignment.slice(equalsAt + 1).trim();
    if (!key) {
      continue;
    }
    value = parseShellAssignmentValue(value);
    parsed[key] = value;
  }
  return parsed;
}

/** Strips quotes and inline comments from a shell assignment right-hand side. */
function parseShellAssignmentValue(value: string): string {
  let parsedValue = '';
  let activeQuote: '"' | "'" | null = null;
  let trailingUnquotedWhitespace = 0;

  for (let index = 0; index < value.length; index += 1) {
    const char = value[index];

    if (activeQuote === "'") {
      if (char === "'") {
        activeQuote = null;
      } else {
        parsedValue += char;
      }
      continue;
    }

    if (activeQuote === '"') {
      if (char === '"' && !hasOddBackslashRunBeforeIndex(value, index)) {
        activeQuote = null;
      } else {
        parsedValue += char;
      }
      continue;
    }

    if (char === '#' && /\s/.test(value[index - 1] ?? '')) {
      return stripTrailingUnquotedWhitespace(parsedValue, trailingUnquotedWhitespace);
    }

    if (char === '"' || char === "'") {
      activeQuote = char;
      trailingUnquotedWhitespace = 0;
      continue;
    }

    parsedValue += char;
    if (/\s/.test(char)) {
      trailingUnquotedWhitespace += 1;
      continue;
    }

    trailingUnquotedWhitespace = 0;
  }

  if (activeQuote) {
    // Keep malformed quoted assignments untouched instead of guessing where they end.
    return value;
  }

  return stripTrailingUnquotedWhitespace(parsedValue, trailingUnquotedWhitespace);
}

function hasOddBackslashRunBeforeIndex(value: string, index: number): boolean {
  let backslashCount = 0;
  for (let cursor = index - 1; cursor >= 0 && value[cursor] === '\\'; cursor -= 1) {
    backslashCount += 1;
  }

  return backslashCount % 2 === 1;
}

function stripTrailingUnquotedWhitespace(value: string, whitespaceCount: number): string {
  if (whitespaceCount === 0) {
    return value;
  }

  return value.slice(0, value.length - whitespaceCount);
}

function parsePort(raw: string | undefined, fallback: number): number {
  if (!raw) {
    return fallback;
  }
  const parsed = Number(raw);
  if (!Number.isInteger(parsed) || parsed <= 0 || parsed > 65535) {
    return fallback;
  }
  return parsed;
}

function resolveConfiguredPort(fallback: number, ...values: Array<string | undefined>): number {
  return parsePort(pickConfiguredValue(...values), fallback);
}

function parseHttpOrigin(raw: string | undefined): string | null {
  if (!raw) {
    return null;
  }
  try {
    const parsed = new URL(raw);
    if (parsed.protocol !== 'http:' && parsed.protocol !== 'https:') {
      return null;
    }
    return parsed.origin;
  } catch {
    return null;
  }
}

/** Wraps a bare IPv6 host in brackets so it is safe to embed in a URL authority. */
function bracketIpv6Host(host: string): string {
  if (host.includes(':') && !host.startsWith('[') && !host.endsWith(']')) {
    return `[${host}]`;
  }
  return host;
}

/** Builds an `http://host:port` origin string, with optional browser-safe bind-address substitution. */
function formatHttpOrigin(
  host: string,
  port: number,
  options: { browserSafe?: boolean } = {},
): string {
  let urlHost = host;

  // Browsers cannot navigate to unspecified bind addresses, so expose a loopback URL instead.
  if (options.browserSafe) {
    if (host === '0.0.0.0') {
      urlHost = '127.0.0.1';
    } else if (host === '::') {
      urlHost = '[::1]';
    }
  }

  return `http://${bracketIpv6Host(urlHost)}:${port}`;
}

function pickConfiguredValue(...values: Array<string | undefined>): string | undefined {
  for (const value of values) {
    if (value) {
      return value;
    }
  }

  return undefined;
}

/** Reads and parses `flapjack.local.conf` from disk, returning key-value pairs. */
function readLocalConfigValues(
  configPath: string = LOCAL_CONFIG_PATH,
): { fileValues: Record<string, string>; loadedFromFile: boolean } {
  if (!fs.existsSync(configPath)) {
    return { fileValues: {}, loadedFromFile: false };
  }

  try {
    const contents = fs.readFileSync(configPath, 'utf8');
    return {
      fileValues: parseLocalConfigFile(contents),
      loadedFromFile: true,
    };
  } catch {
    return { fileValues: {}, loadedFromFile: false };
  }
}

function currentUid(): number | null {
  return typeof process.getuid === 'function' ? process.getuid() : null;
}

function hasUnsafeWritePermissions(mode: number): boolean {
  return (mode & 0o022) !== 0;
}

function isSecureOwnedPath(stats: fs.Stats, expectedUid: number | null): boolean {
  if (expectedUid !== null && typeof stats.uid === 'number' && stats.uid !== expectedUid) {
    return false;
  }

  if (process.platform !== 'win32' && hasUnsafeWritePermissions(stats.mode)) {
    return false;
  }

  return true;
}

/** Return paths to .meta files in the state directory, filtering out symlinks and insecurely-owned entries. */
function secureTrackedMetaFiles(stateDir: string): string[] {
  const expectedUid = currentUid();

  try {
    const stateDirStats = fs.lstatSync(stateDir);
    if (!stateDirStats.isDirectory() || stateDirStats.isSymbolicLink()) {
      return [];
    }
    if (!isSecureOwnedPath(stateDirStats, expectedUid)) {
      return [];
    }

    const realStateDir = fs.realpathSync(stateDir);
    return fs.readdirSync(realStateDir, { withFileTypes: true })
      .filter((entry) => entry.isFile() && entry.name.endsWith('.meta'))
      .map((entry) => path.join(realStateDir, entry.name))
      .filter((entryPath) => {
        try {
          const entryStats = fs.lstatSync(entryPath);
          if (!entryStats.isFile() || entryStats.isSymbolicLink()) {
            return false;
          }
          return isSecureOwnedPath(entryStats, expectedUid);
        } catch {
          return false;
        }
      });
  } catch {
    return [];
  }
}

/** Resolve and validate a raw data directory path, returning null if invalid or non-existent. */
function normalizeTrackedDataDir(rawPath: string | undefined): string | null {
  if (!rawPath || !path.isAbsolute(rawPath)) {
    return null;
  }

  try {
    const resolved = fs.realpathSync(rawPath);
    const stats = fs.statSync(resolved);
    if (!stats.isDirectory()) {
      return null;
    }
    return resolved;
  } catch {
    return null;
  }
}

interface HostAndPort {
  hostname: string;
  port: string;
}

function parseHostAndPort(value: string): HostAndPort | null {
  try {
    const parsed = new URL(value);
    return {
      hostname: parsed.hostname,
      port: parsed.port,
    };
  } catch {
    return null;
  }
}

function isLoopbackBackendUrl(backendBaseUrl: string): boolean {
  try {
    return LOOPBACK_HOSTS.has(new URL(backendBaseUrl).hostname);
  } catch {
    return false;
  }
}

function parseBindAddress(rawBindAddress: string | undefined): HostAndPort | null {
  if (!rawBindAddress) {
    return null;
  }

  const trimmed = rawBindAddress.trim();
  const portSeparator = trimmed.lastIndexOf(':');
  if (portSeparator <= 0 || portSeparator === trimmed.length - 1) {
    return null;
  }

  let hostname = trimmed.slice(0, portSeparator);
  const port = trimmed.slice(portSeparator + 1);
  if (!hostname || !port) {
    return null;
  }

  if (hostname.startsWith('[') && hostname.endsWith(']')) {
    hostname = hostname.slice(1, -1);
  }

  return { hostname, port };
}

function normalizeTrackedHost(hostname: string): string {
  return LOCAL_BINDABLE_HOSTS.has(hostname) ? 'loopback' : hostname.toLowerCase();
}

function trackedBindAddressMatchesBackend(
  trackedBindAddress: string | undefined,
  backendBaseUrl: string,
): boolean {
  const backendAddress = parseHostAndPort(backendBaseUrl);
  const trackedAddress = parseBindAddress(trackedBindAddress);
  if (!backendAddress || !trackedAddress) {
    return false;
  }

  return (
    backendAddress.port === trackedAddress.port
    && normalizeTrackedHost(backendAddress.hostname) === normalizeTrackedHost(trackedAddress.hostname)
  );
}

export function findTrackedBackendDataDir(
  backendBaseUrl: string,
  stateDir: string = MULTI_INSTANCE_STATE_DIR,
): string | null {
  if (!parseHostAndPort(backendBaseUrl)) {
    return null;
  }

  try {
    for (const metaPath of secureTrackedMetaFiles(stateDir)) {
      const contents = fs.readFileSync(metaPath, 'utf8');
      const values = parseLocalConfigFile(contents);
      if (!trackedBindAddressMatchesBackend(values.bind_addr, backendBaseUrl)) {
        continue;
      }

      const trackedDataDir = normalizeTrackedDataDir(values.data_dir);
      if (trackedDataDir) {
        return trackedDataDir;
      }
    }
  } catch {
    return null;
  }

  return null;
}

/** Resolves the backend data directory from env vars, config file, multi-instance state, or the default path. */
export function resolveBackendDataDir(
  fileValues: Record<string, string>,
  backendBaseUrl: string,
  stateDir: string = MULTI_INSTANCE_STATE_DIR,
): string {
  return (
    pickConfiguredValue(
      process.env.FLAPJACK_DATA_DIR,
      process.env.FJ_DATA_DIR,
      fileValues.FLAPJACK_DATA_DIR,
      fileValues.FJ_DATA_DIR,
    )
    || findTrackedBackendDataDir(backendBaseUrl, stateDir)
    || DEFAULT_BACKEND_DATA_DIR
  );
}

/** Returns the configured admin key, falling back to the dev default for loopback hosts. */
export function resolveAdminKey(
  configuredAdminKey: string | undefined,
  backendBaseUrl: string,
  options: ResolveAdminKeyOptions = {},
): string {
  let hostname: string;
  try {
    hostname = new URL(backendBaseUrl).hostname;
  } catch {
    return DEFAULTS.adminKey;
  }

  const isLoopbackBackend = LOOPBACK_HOSTS.has(hostname);
  const loopbackProcessAdminKey = (
    configuredAdminKey === DEFAULTS.adminKey
      ? (
        options.loopbackProcessAdminKey
        || findLoopbackProcessAdminKey(backendBaseUrl, options.processListOutput)
      )
      : undefined
  );

  if (configuredAdminKey) {
    if (
      isLoopbackBackend
      && configuredAdminKey === DEFAULTS.adminKey
      && loopbackProcessAdminKey
    ) {
      return loopbackProcessAdminKey;
    }
    return configuredAdminKey;
  }

  if (LOOPBACK_HOSTS.has(hostname)) {
    return DEFAULTS.adminKey;
  }

  throw new Error(
    `FJ_TEST_ADMIN_KEY must be set when using a non-loopback backend URL: ${backendBaseUrl}`,
  );
}

function readProcessListOutput(): string | null {
  try {
    return execFileSync('ps', ['eww', '-axo', 'command'], {
      encoding: 'utf8',
      maxBuffer: 10 * 1024 * 1024,
    });
  } catch {
    return null;
  }
}

function processLineTargetsPort(processLine: string, port: number): boolean {
  if (
    processLine.includes(`--port ${port}`)
    || processLine.includes(`--port=${port}`)
  ) {
    return true;
  }

  const bindAddressPattern = new RegExp(`--bind-addr(?:=|\\s+)\\S*:${port}(?:\\s|$)`);
  return bindAddressPattern.test(processLine);
}

/** Finds FLAPJACK_ADMIN_KEY from a loopback flapjack process bound to backendBaseUrl. */
export function findLoopbackProcessAdminKey(
  backendBaseUrl: string,
  processListOutput?: string,
): string | undefined {
  let parsed: URL;
  try {
    parsed = new URL(backendBaseUrl);
  } catch {
    return undefined;
  }

  if (!LOOPBACK_HOSTS.has(parsed.hostname)) {
    return undefined;
  }

  const port = Number(parsed.port || (parsed.protocol === 'https:' ? 443 : 80));
  if (!Number.isInteger(port) || port <= 0) {
    return undefined;
  }

  const processOutput = processListOutput ?? readProcessListOutput();
  if (!processOutput) {
    return undefined;
  }

  for (const processLine of processOutput.split('\n')) {
    if (!processLine.includes('flapjack') || !processLineTargetsPort(processLine, port)) {
      continue;
    }
    const keyMatch = processLine.match(/FLAPJACK_ADMIN_KEY=([^\s]+)/);
    if (keyMatch?.[1]) {
      return keyMatch[1];
    }
  }

  return undefined;
}

/**
 * TODO: Document getLocalInstanceConfig.
 */
interface BackendEndpoint {
  backendBaseUrl: string;
  backendHost: string;
  backendPort: number;
}

/**
 * Resolves the effective backend endpoint exactly once so its URL, host, and port
 * cannot disagree. A valid `FLAPJACK_BACKEND_URL` (file-first, env-second) owns all
 * three halves; otherwise the endpoint falls back to the dashboard-bind `host` plus
 * the configured `FJ_BACKEND_PORT`.
 */
function resolveBackendEndpoint(
  fileValues: Record<string, string>,
  host: string,
): BackendEndpoint {
  const selectedUrl = [
    fileValues.FLAPJACK_BACKEND_URL,
    process.env.FLAPJACK_BACKEND_URL,
  ].find((value) => parseHttpOrigin(value) !== null);
  const backendBaseUrl = parseHttpOrigin(selectedUrl);
  if (backendBaseUrl !== null) {
    // `selectedUrl` is guaranteed parseable here — parseHttpOrigin returned non-null.
    const parsed = new URL(selectedUrl as string);
    return {
      backendBaseUrl,
      backendHost: bracketIpv6Host(parsed.hostname),
      backendPort: parsed.port
        ? Number(parsed.port)
        : (parsed.protocol === 'https:' ? 443 : 80),
    };
  }
  const backendPort = resolveConfiguredPort(
    DEFAULTS.backendPort,
    fileValues.FJ_BACKEND_PORT,
    process.env.FJ_BACKEND_PORT,
  );
  return {
    backendBaseUrl: formatHttpOrigin(host, backendPort),
    backendHost: host,
    backendPort,
  };
}

export function getLocalInstanceConfig(): LocalInstanceConfig {
  const { fileValues, loadedFromFile } = readLocalConfigValues();
  const host = pickConfiguredValue(fileValues.FJ_HOST, process.env.FJ_HOST) || DEFAULTS.host;
  const dashboardPort = resolveConfiguredPort(
    DEFAULTS.dashboardPort,
    fileValues.FJ_DASHBOARD_PORT,
    process.env.FJ_DASHBOARD_PORT,
    process.env.FLAPJACK_DASHBOARD_PORT,
  );
  const { backendBaseUrl, backendHost, backendPort } = resolveBackendEndpoint(fileValues, host);
  // Only reuse the runtime server admin key for loopback backends. Remote
  // dashboard targets must opt in with FJ_TEST_ADMIN_KEY so a local secret
  // is never sent to an arbitrary non-local URL by default.
  const configuredAdminKey = pickConfiguredValue(
    fileValues.FJ_TEST_ADMIN_KEY,
    process.env.FJ_TEST_ADMIN_KEY,
    isLoopbackBackendUrl(backendBaseUrl) ? process.env.FLAPJACK_ADMIN_KEY : undefined,
  );
  const adminKey = resolveAdminKey(
    configuredAdminKey,
    backendBaseUrl,
  );
  const backendDataDir = resolveBackendDataDir(fileValues, backendBaseUrl);
  const dashboardBaseUrl = formatHttpOrigin(host, dashboardPort, { browserSafe: true });

  return {
    host,
    backendHost,
    backendPort,
    dashboardPort,
    adminKey,
    backendBaseUrl,
    backendDataDir,
    dashboardBaseUrl,
    configPath: LOCAL_CONFIG_PATH,
    loadedFromFile,
  };
}
