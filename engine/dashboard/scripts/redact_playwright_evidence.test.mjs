/* @vitest-environment node */
import { execFile } from 'node:child_process';
import { mkdtemp, readFile, rm, stat, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import { afterEach, describe, expect, it } from 'vitest';
import RedactPlaywrightJsonReporter, {
  redactSensitiveReportValues,
} from './redact_playwright_evidence.mjs';

const temporaryDirectories = [];
const execFileAsync = promisify(execFile);

afterEach(async () => {
  await Promise.all(temporaryDirectories.splice(0).map((directory) => (
    rm(directory, { recursive: true, force: true })
  )));
});

describe('redactSensitiveReportValues', () => {
  it('redacts sensitive fields and request headers while preserving unrelated evidence', () => {
    const report = {
      config: {
        metadata: {
          token: 'metadata-token',
          diagnostic: 'credential metadata-token',
          label: 'keep-me',
        },
      },
      suites: [{
        error: {
          message: [
            'Call log:',
            '  - x-algolia-api-key: live-admin-key',
            '  - authorization: Bearer live-token',
            '  - x-auth-token: live-auth-token',
            '  - content-type: application/json',
            '  - post data: {"credentials":"body-secret","query":"laptop"}',
          ].join('\n'),
        },
        password: 'body-password',
      }],
    };

    expect(redactSensitiveReportValues(report, ['metadata-token'])).toEqual({
      config: {
        metadata: {
          token: '<redacted>',
          diagnostic: 'credential <redacted>',
          label: 'keep-me',
        },
      },
      suites: [{
        error: {
          message: [
            'Call log:',
            '  - x-algolia-api-key: <redacted>',
            '  - authorization: <redacted>',
            '  - x-auth-token: <redacted>',
            '  - content-type: application/json',
            '  - post data: {"credentials":"<redacted>","query":"laptop"}',
          ].join('\n'),
        },
        password: '<redacted>',
      }],
    });
  });
});

describe('RedactPlaywrightJsonReporter', () => {
  it('atomically rewrites the JSON report with owner-only permissions', async () => {
    const directory = await mkdtemp(join(tmpdir(), 'flapjack-playwright-redaction-'));
    temporaryDirectories.push(directory);
    const reportPath = join(directory, 'results.json');
    await writeFile(reportPath, JSON.stringify({
      apiKey: 'live-key',
      message: 'detached credential: environment-secret',
      status: 'passed',
    }), {
      mode: 0o644,
    });

    const reporter = new RedactPlaywrightJsonReporter({
      configDir: directory,
      environment: { PLAYWRIGHT_REDACTOR_TEST_TOKEN: 'environment-secret' },
      inputFile: 'results.json',
    });
    await reporter.onEnd();

    expect(reporter.version()).toBe('v2');
    expect(JSON.parse(await readFile(reportPath, 'utf8'))).toEqual({
      apiKey: '<redacted>',
      message: 'detached credential: <redacted>',
      status: 'passed',
    });
    expect((await stat(reportPath)).mode & 0o777).toBe(0o600);
    expect(reporter.printsToStdio()).toBe(false);
  });

  it('fails closed when the JSON reporter output is malformed', async () => {
    const directory = await mkdtemp(join(tmpdir(), 'flapjack-playwright-redaction-'));
    temporaryDirectories.push(directory);
    const reportPath = join(directory, 'results.json');
    await writeFile(reportPath, '{malformed');

    const reporter = new RedactPlaywrightJsonReporter({
      configDir: directory,
      environment: {},
      inputFile: 'results.json',
    });

    await expect(reporter.onEnd()).rejects.toThrow();
    expect(await readFile(reportPath, 'utf8')).toBe('{malformed');
  });

  it('redacts a retained transcript using every value from an explicit secret file', async () => {
    const directory = await mkdtemp(join(tmpdir(), 'flapjack-playwright-redaction-'));
    temporaryDirectories.push(directory);
    const transcriptPath = join(directory, 'playwright.log');
    const secretPath = join(directory, '.env.secret');
    await writeFile(secretPath, 'OPAQUE_VALUE=detached-secret\n');
    await writeFile(
      transcriptPath,
      'x-algolia-api-key: live-admin-key\ndiagnostic: detached-secret\nstatus: failed\n',
    );

    await execFileAsync(process.execPath, [
      fileURLToPath(new URL('./redact_playwright_evidence.mjs', import.meta.url)),
      '--secret-file',
      secretPath,
      transcriptPath,
    ]);

    expect(await readFile(transcriptPath, 'utf8')).toBe(
      'x-algolia-api-key: <redacted>\ndiagnostic: <redacted>\nstatus: failed\n',
    );
    expect((await stat(transcriptPath)).mode & 0o777).toBe(0o600);
  });
});
