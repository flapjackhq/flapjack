import { chmod, readFile, rename, unlink, writeFile } from 'node:fs/promises';
import { resolve } from 'node:path';
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

function redactValue(value, sensitiveValues) {
  if (typeof value === 'string') {
    return redactSensitiveString(value, sensitiveValues);
  }
  if (Array.isArray(value)) {
    return value.map((item) => redactValue(item, sensitiveValues));
  }
  if (!value || typeof value !== 'object') {
    return value;
  }

  return Object.fromEntries(Object.entries(value).map(([fieldName, fieldValue]) => [
    fieldName,
    isSensitiveFieldName(fieldName) ? REDACTED_VALUE : redactValue(fieldValue, sensitiveValues),
  ]));
}

export function redactSensitiveReportValues(value, sensitiveValues = []) {
  const uniqueSensitiveValues = [...new Set(sensitiveValues)].sort(
    (left, right) => right.length - left.length,
  );
  return redactValue(value, uniqueSensitiveValues);
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
  const original = await readFile(resolvedInputFile, 'utf8');
  const sensitiveValues = collectSensitiveEnvironmentValues(
    options.environment ?? process.env,
    options.sensitiveValues ?? [],
  );
  let redacted;

  try {
    const report = JSON.parse(original);
    redacted = JSON.stringify(redactSensitiveReportValues(report, sensitiveValues), null, 2);
  } catch (error) {
    if (options.requireJson) {
      throw error;
    }
    redacted = redactSensitiveString(original, sensitiveValues);
  }

  await rewriteEvidenceFile(resolvedInputFile, redacted);
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
  let secretFile;

  for (let index = 0; index < args.length; index += 1) {
    if (args[index] === '--secret-file') {
      secretFile = args[index + 1];
      index += 1;
    } else {
      files.push(args[index]);
    }
  }
  if (files.length === 0) {
    throw new Error('usage: redact_playwright_evidence.mjs [--secret-file path] <file> [...]');
  }

  let sensitiveValues = [];
  if (secretFile) {
    sensitiveValues = Object.values(parseDotenv(await readFile(resolve(secretFile))));
  }
  for (const file of files) {
    await redactEvidenceFile(file, { sensitiveValues });
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  runCli(process.argv.slice(2)).catch((error) => {
    process.stderr.write(`redact_playwright_evidence: ${error.message}\n`);
    process.exitCode = 1;
  });
}
