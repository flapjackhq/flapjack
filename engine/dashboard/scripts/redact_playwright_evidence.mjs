import { execFileSync } from 'node:child_process';
import { chmod, lstat, readFile, readdir, rename, unlink, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { parse as parseDotenv } from 'dotenv';

const REDACTED_VALUE = '<redacted>';
const NAMED_VALUE_LINE_PATTERN = /^([ \t]*(?:-[ \t]*)?)([a-z0-9_-]+)([ \t]*:[ \t]*)[^\r\n]*/gim;
const QUOTED_FIELD_PATTERN = /(["'])([^"']+)\1(\s*:\s*)(["'])([^"']*)\4/g;

function isSensitiveFieldName(fieldName) {
  const normalized = fieldName.toLowerCase().replaceAll(/[^a-z0-9]/g, '');
  return normalized === 'authorization'
    || normalized === 'cookie'
    || normalized === 'setcookie'
    || normalized.endsWith('appid')
    || normalized.includes('secret')
    || normalized.endsWith('apikey')
    || normalized.endsWith('adminkey')
    || normalized.endsWith('accesskey')
    || normalized.endsWith('privatekey')
    || normalized.endsWith('credential')
    || normalized.endsWith('credentials')
    || normalized.endsWith('password')
    || normalized.endsWith('token');
}

function redactNamedValues(value) {
  return value
    .replace(NAMED_VALUE_LINE_PATTERN, (line, prefix, fieldName, separator) => (
      isSensitiveFieldName(fieldName) ? `${prefix}${fieldName}${separator}${REDACTED_VALUE}` : line
    ))
    .replace(
      QUOTED_FIELD_PATTERN,
      (field, fieldQuote, fieldName, separator, valueQuote) => (
        isSensitiveFieldName(fieldName)
          ? `${fieldQuote}${fieldName}${fieldQuote}${separator}${valueQuote}${REDACTED_VALUE}${valueQuote}`
          : field
      ),
    );
}

export function redactSensitiveString(value, sensitiveValues = []) {
  const uniqueSensitiveValues = [...new Set(sensitiveValues)].sort(
    (left, right) => right.length - left.length,
  );
  const structurallyRedacted = redactNamedValues(value);

  return uniqueSensitiveValues.reduce(
    (redacted, sensitiveValue) => redacted.replaceAll(sensitiveValue, REDACTED_VALUE),
    structurallyRedacted,
  );
}

function collectSensitiveEnvironmentValues(environment, additionalValues) {
  const environmentValues = Object.entries(environment)
    .filter(([fieldName]) => isSensitiveFieldName(fieldName))
    .map(([, fieldValue]) => fieldValue);

  return [...new Set([...environmentValues, ...additionalValues]
    .filter((fieldValue) => typeof fieldValue === 'string' && fieldValue.length >= 8))]
    .sort((left, right) => right.length - left.length);
}

function redactValue(value, sensitiveValues, context) {
  if (typeof value === 'string') {
    return redactSensitiveString(value, sensitiveValues);
  }
  if (Array.isArray(value)) {
    return value.map((item) => redactValue(item, sensitiveValues, context));
  }
  if (!value || typeof value !== 'object') {
    return value;
  }

  const sensitivePair = typeof value.name === 'string'
    && (isSensitiveFieldName(value.name) || context === 'cookies');
  return Object.fromEntries(Object.entries(value).map(([fieldName, fieldValue]) => [
    fieldName,
    isSensitiveFieldName(fieldName) || (sensitivePair && fieldName === 'value')
      ? REDACTED_VALUE : redactValue(fieldValue, sensitiveValues, fieldName),
  ]));
}

export function redactSensitiveReportValues(value, sensitiveValues = []) {
  const uniqueSensitiveValues = [...new Set(sensitiveValues)].sort(
    (left, right) => right.length - left.length,
  );
  return redactValue(value, uniqueSensitiveValues);
}

// Python owns only ZIP transport. Redaction stays in this module, and archive
// members never become filesystem paths. Preserve names used by trace references.
const ZIP_TRANSPORT = `
import base64,io,json,sys,zipfile
try:
 p=json.load(sys.stdin)
 with zipfile.ZipFile(io.BytesIO(base64.b64decode(p['archive']))) as z:
  names=z.namelist()
  if len(names)!=len(set(names)): raise ValueError()
  if p['operation']=='read':
   print(json.dumps({'entries':[{'name':n,'data':base64.b64encode(z.read(n)).decode()} for n in names],'comment':base64.b64encode(z.comment).decode()}))
  else:
   entries={e['name']:base64.b64decode(e['data']) for e in p['entries']}
   if set(entries)!=set(names): raise ValueError()
   output=io.BytesIO()
   with zipfile.ZipFile(output,'w') as cleaned:
    cleaned.comment=base64.b64decode(p['comment'])
    for info in z.infolist(): cleaned.writestr(info,entries[info.filename])
   print(json.dumps(base64.b64encode(output.getvalue()).decode()))
except Exception:
 print('ZIP artifact transport refused',file=sys.stderr)
 sys.exit(2)
`;

function exchangeZip(payload) {
  try {
    return JSON.parse(execFileSync('python3', ['-c', ZIP_TRANSPORT], {
      input: JSON.stringify(payload), encoding: 'utf8', stdio: 'pipe', maxBuffer: 64 * 1024 * 1024,
    }));
  } catch {
    throw new Error('ZIP artifact could not be sanitized');
  }
}

function redactZip(archive, sensitiveValues) {
  const encoded = archive.toString('base64');
  const contents = exchangeZip({ operation: 'read', archive: encoded });
  const entries = contents.entries.map(({ name, data }) => ({
    name,
    data: redactArtifactBytes(Buffer.from(data, 'base64'), name, sensitiveValues).toString('base64'),
  }));
  const comment = redactArtifactBytes(Buffer.from(contents.comment, 'base64'), 'comment.txt', sensitiveValues).toString('base64');
  return Buffer.from(exchangeZip({ operation: 'write', archive: encoded, entries, comment }), 'base64');
}

function redactArtifactBytes(original, name, sensitiveValues) {
  if (name.endsWith('.zip')) return redactZip(original, sensitiveValues);
  if (/\.(?:png|jpe?g|webp|gif|woff2?|wasm|js|css)$/.test(name)) {
    if (sensitiveValues.some((value) => original.includes(Buffer.from(value)))) {
      throw new Error('Static artifact contains sensitive data');
    }
    return original;
  }
  let text;
  try {
    text = new TextDecoder('utf8', { fatal: true }).decode(original);
  } catch {
    if (sensitiveValues.some((value) => original.includes(Buffer.from(value)))) {
      throw new Error('Binary artifact contains sensitive data');
    }
    return original;
  }
  if (name.endsWith('.html')) {
    text = text.replace(/data:application\/zip;base64,([A-Za-z0-9+/=]+)/g, (_, encoded) => (
      `data:application/zip;base64,${redactZip(Buffer.from(encoded, 'base64'), sensitiveValues).toString('base64')}`
    ));
    // The HTML also carries static viewer code; keep its token definitions intact.
    for (const value of sensitiveValues) text = text.replaceAll(value, REDACTED_VALUE);
    return Buffer.from(text);
  }
  if (/\.(?:trace|network|jsonl)$/.test(name)) {
    try {
      return Buffer.from(text.split('\n').map((line) => (
        line ? JSON.stringify(redactSensitiveReportValues(JSON.parse(line), sensitiveValues)) : line
      )).join('\n'));
    } catch {
      throw new Error('Structured trace artifact is malformed');
    }
  }
  let parsed;
  try {
    parsed = JSON.parse(text);
  } catch {
    if (name.endsWith('.json')) throw new Error('JSON artifact is malformed');
    return Buffer.from(redactSensitiveString(text, sensitiveValues));
  }
  return Buffer.from(JSON.stringify(redactSensitiveReportValues(parsed, sensitiveValues), null, 2));
}

async function rewriteEvidenceFile(inputFile, content) {
  const temporaryFile = `${inputFile}.${process.pid}.tmp`;

  try {
    await writeFile(temporaryFile, content, { flag: 'wx', mode: 0o600 });
    await rename(temporaryFile, inputFile);
    await chmod(inputFile, 0o600);
  } finally {
    await unlink(temporaryFile).catch(() => {});
  }
}

export async function redactEvidenceFile(inputFile, options = {}) {
  const resolvedInputFile = resolve(inputFile);
  const original = await readFile(resolvedInputFile);
  const sensitiveValues = collectSensitiveEnvironmentValues(
    options.environment ?? process.env,
    options.sensitiveValues ?? [],
  );
  const redacted = options.requireJson
    ? JSON.stringify(redactSensitiveReportValues(JSON.parse(original.toString('utf8')), sensitiveValues), null, 2)
    : redactArtifactBytes(original, resolvedInputFile, sensitiveValues);

  await rewriteEvidenceFile(resolvedInputFile, redacted);
}

export async function redactArtifactDirectory(root, options = {}) {
  const info = await lstat(root);
  if (info.isSymbolicLink()) throw new Error('Artifact links are not accepted');
  if (!info.isDirectory()) {
    await redactEvidenceFile(root, options);
    return;
  }
  for (const entry of await readdir(root)) {
    await redactArtifactDirectory(join(root, entry), options);
  }
}

export default class RedactPlaywrightJsonReporter {
  constructor(options) {
    if (!options?.inputFile) {
      throw new Error('redacting JSON reporter requires inputFile');
    }
    this.inputFile = resolve(options.configDir ?? process.cwd(), options.inputFile);
    this.environment = options.environment ?? process.env;
  }

  version() {
    return 'v2';
  }

  printsToStdio() {
    return false;
  }

  async onEnd() {
    await redactEvidenceFile(this.inputFile, {
      environment: this.environment,
      requireJson: true,
    });
  }
}

async function runCli(args) {
  const files = [];
  const artifactRoots = [];
  let secretFile;

  for (let index = 0; index < args.length; index += 1) {
    if (args[index] === '--secret-file') {
      secretFile = args[index + 1];
      index += 1;
    } else if (args[index] === '--artifact-root') {
      artifactRoots.push(args[index + 1]);
      index += 1;
    } else {
      files.push(args[index]);
    }
  }
  if (files.length === 0 && artifactRoots.length === 0) {
    throw new Error('usage: redact_playwright_evidence.mjs [--secret-file path] <file> [...]');
  }

  let sensitiveValues = [];
  if (secretFile) {
    sensitiveValues = Object.values(parseDotenv(await readFile(resolve(secretFile))));
  }
  for (const file of files) {
    await redactEvidenceFile(file, { sensitiveValues });
  }
  for (const root of artifactRoots) {
    await redactArtifactDirectory(resolve(root), { sensitiveValues });
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  runCli(process.argv.slice(2)).catch((error) => {
    process.stderr.write(`redact_playwright_evidence: ${error.message}\n`);
    process.exitCode = 1;
  });
}
