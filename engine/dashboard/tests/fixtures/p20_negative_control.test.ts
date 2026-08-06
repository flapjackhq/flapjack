import fs from 'node:fs';
import path from 'node:path';
import type { APIRequestContext } from '@playwright/test';
import { describe, expect, it, vi } from 'vitest';
import {
  P20_TEXT_ONLY_CONTROL_TEST_TITLE,
  installTextOnlyNegativeControlCapability,
  isP20TextOnlyNegativeControl,
  waitForTextOnlyNegativeControlReadiness,
} from './p20_negative_control';

const VECTOR_SETTINGS_SPEC = path.resolve(
  __dirname,
  '../e2e-ui/full/vector-settings.spec.ts',
);

describe('P20 text-only negative control', () => {
  it('is disabled unless the control environment value is exactly 1', () => {
    expect(isP20TextOnlyNegativeControl({})).toBe(false);
    expect(isP20TextOnlyNegativeControl({ P20_TEXT_ONLY_NEGATIVE_CONTROL: 'true' })).toBe(
      false,
    );
    expect(isP20TextOnlyNegativeControl({ P20_TEXT_ONLY_NEGATIVE_CONTROL: '1' })).toBe(
      true,
    );
  });

  it('names the single P20 test the control may be scoped to', () => {
    const spec = fs.readFileSync(VECTOR_SETTINGS_SPEC, 'utf8');

    expect(spec).toContain(`test('${P20_TEXT_ONLY_CONTROL_TEST_TITLE}'`);
    expect(spec).toContain('testInfo.title === P20_TEXT_ONLY_CONTROL_TEST_TITLE');
  });

  it('overrides only the browser health capability payload', async () => {
    const fulfill = vi.fn();
    const route = vi.fn(
      async (
        _pattern: string,
        handler: (route: { fulfill: typeof fulfill }) => Promise<void>,
      ) => {
        await handler({ fulfill });
      },
    );

    await installTextOnlyNegativeControlCapability({ route });

    expect(route).toHaveBeenCalledWith('**/health', expect.any(Function));
    expect(fulfill).toHaveBeenCalledWith({
      status: 200,
      json: {
        status: 'ok',
        capabilities: { vectorSearch: true, vectorSearchLocal: false },
      },
    });
  });

  it('uses keyword indexing only as the text-only control readiness precondition', async () => {
    const request = {} as APIRequestContext;
    const waitForSearchability = vi.fn();

    await waitForTextOnlyNegativeControlReadiness({
      request,
      indexName: 'products',
      targetObjectId: 'semantic-chair',
      waitForSearchability,
    });

    expect(waitForSearchability).toHaveBeenCalledWith(
      request,
      'products',
      '',
      ['semantic-chair'],
      { hitsPerPage: 10, mode: 'keywordSearch' },
    );
  });
});
