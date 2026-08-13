import { describe, expect, it } from 'vitest';
import { migrationWriteActionName } from './migration-preview.fixture';

describe('migrationWriteActionName', () => {
  it('matches the shipped quoted legacy control and the post-preview submit control', () => {
    const target = 'fj_e2e.migrate+(target)[1]';
    const name = migrationWriteActionName('Algolia', target);

    expect(name.test(`Migrate from Algolia "${target}"`)).toBe(true);
    expect(name.test('Submit migration')).toBe(true);
  });

  it('does not let regex syntax in an index name match another target or provider', () => {
    const name = migrationWriteActionName('Meilisearch', 'products.*');

    expect(name.test('Migrate from Meilisearch "products-2026"')).toBe(false);
    expect(name.test('Migrate from Typesense "products.*"')).toBe(false);
    expect(name.test('Migrate from Meilisearch "products.*" trailing')).toBe(false);
  });
});
