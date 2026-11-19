/* @vitest-environment node */
import { execFile, execFileSync } from 'node:child_process';
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
  it('redacts trace header pairs and escaped request-body credentials', () => {
    expect(redactSensitiveReportValues({
      headers: [{ name: 'Cookie', value: 'session-canary' }, { name: 'X-Algolia-API-Key', value: 'header-canary' }],
      postData: { text: JSON.stringify({ apiKey: 'body-canary', query: 'Espresso' }) },
      status: 502,
    })).toEqual({
      headers: [{ name: 'Cookie', value: '<redacted>' }, { name: 'X-Algolia-API-Key', value: '<redacted>' }],
      postData: { text: JSON.stringify({ apiKey: '<redacted>', query: 'Espresso' }) },
      status: 502,
    });
  });
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
  it('sanitizes a trace directory while preserving resource references and binary evidence', async () => {
    const directory = await mkdtemp(join(tmpdir(), 'flapjack-trace-redaction-'));
    temporaryDirectories.push(directory);
    const tracePath = join(directory, 'trace.zip');
    execFileSync('python3', ['-c', `
import json,sys,zipfile
with zipfile.ZipFile(sys.argv[1],'w',zipfile.ZIP_DEFLATED) as z:
 z.comment=b'trace-comment'
 z.writestr('trace.network',json.dumps({'snapshot':{'request':{'headers':[{'name':'Cookie','value':'session-canary'}],'cookies':[{'name':'session','value':'cookie-canary'}],'postData':{'_sha1':'body'}},'response':{'status':502}}})+'\\n')
 z.writestr('resources/body',json.dumps({'apiKey':'body-canary','query':'Espresso'}))
 z.writestr('resources/pixel.png',bytes([0,255,17,128]))
 z.writestr('assets/viewer.js','const parser = { token: "keep-static-token" };')
`, tracePath]);
    await execFileAsync(process.execPath, [
      fileURLToPath(new URL('./redact_playwright_evidence.mjs', import.meta.url)), '--artifact-root', directory,
    ]);
    const trace = JSON.parse(execFileSync('python3', ['-c', `
import base64,json,sys,zipfile
with zipfile.ZipFile(sys.argv[1]) as z:
 print(json.dumps({'names':z.namelist(),'network':json.loads(z.read('trace.network')),'body':json.loads(z.read('resources/body')),'pixel':base64.b64encode(z.read('resources/pixel.png')).decode(),'viewer':z.read('assets/viewer.js').decode(),'comment':z.comment.decode()}))
`, tracePath], { encoding: 'utf8' }));
    expect(trace).toEqual({
      names: ['trace.network', 'resources/body', 'resources/pixel.png', 'assets/viewer.js'],
      network: { snapshot: { request: {
        headers: [{ name: 'Cookie', value: '<redacted>' }],
        cookies: [{ name: 'session', value: '<redacted>' }],
        postData: { _sha1: 'body' },
      }, response: { status: 502 } } },
      body: { apiKey: '<redacted>', query: 'Espresso' },
      pixel: Buffer.from([0, 255, 17, 128]).toString('base64'),
      viewer: 'const parser = { token: "keep-static-token" };',
      comment: 'trace-comment',
    });
  });

  it('returns nonzero for an unreadable trace instead of admitting unsafe artifacts', async () => {
    const directory = await mkdtemp(join(tmpdir(), 'flapjack-malformed-trace-'));
    temporaryDirectories.push(directory);
    await writeFile(join(directory, 'trace.zip'), 'invalid ZIP containing fixture-canary');
    await expect(execFileAsync(process.execPath, [
      fileURLToPath(new URL('./redact_playwright_evidence.mjs', import.meta.url)), '--artifact-root', directory,
    ])).rejects.toMatchObject({ code: 1, stderr: expect.stringContaining('ZIP artifact could not be sanitized') });
  });

  it('redacts the ZIP embedded in an HTML report without losing its failure evidence', async () => {
    const directory = await mkdtemp(join(tmpdir(), 'flapjack-report-zip-'));
    temporaryDirectories.push(directory);
    const reportPath = join(directory, 'index.html');
    const encoded = execFileSync('python3', ['-c', `
import base64, io, json, zipfile
buffer=io.BytesIO()
with zipfile.ZipFile(buffer,'w',zipfile.ZIP_DEFLATED) as z:
 z.writestr('report.json',json.dumps({'apiKey':'fixture-secret-canary','status':'failed','expected':'9'}))
print(base64.b64encode(buffer.getvalue()).decode())
`], { encoding: 'utf8' }).trim();
    await writeFile(reportPath, `<script id="playwrightReportBase64" type="application/zip">data:application/zip;base64,${encoded}</script>`);
    await execFileAsync(process.execPath, [
      fileURLToPath(new URL('./redact_playwright_evidence.mjs', import.meta.url)), reportPath,
    ]);
    const sanitized = (await readFile(reportPath, 'utf8')).match(/base64,([^<]+)/)[1];
    const report = JSON.parse(execFileSync('python3', ['-c', `
import base64,io,sys,zipfile
with zipfile.ZipFile(io.BytesIO(base64.b64decode(sys.stdin.read()))) as z:
 print(z.read('report.json').decode())
`], { input: sanitized, encoding: 'utf8' }));
    expect(report).toEqual({ apiKey: '<redacted>', status: 'failed', expected: '9' });
  });
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
