import { readFileSync } from 'node:fs';

const fixture = JSON.parse(readFileSync(
  new URL('../fixtures/official_client_contract.json', import.meta.url),
  'utf8',
));

export const PRODUCTS = Object.freeze(fixture.products);
export const INDEX_SETTINGS = Object.freeze(fixture.settings);
export const FIRST_PAGE_NAMES = Object.freeze(fixture.expected.firstPageNames);
export const SECOND_PAGE_NAMES = Object.freeze(fixture.expected.secondPageNames);
export const LAPTOP_NAMES = Object.freeze(fixture.expected.laptopNames);
export const NOVA_NAMES = Object.freeze(fixture.expected.novaNames);
