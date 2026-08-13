export const PRODUCTS = Object.freeze([
  { objectID: 'product_1', name: 'Alpha Laptop', brand: 'Acme', sortOrder: 1 },
  { objectID: 'product_2', name: 'Beta Laptop', brand: 'Acme', sortOrder: 3 },
  { objectID: 'product_3', name: 'Gamma Phone', brand: 'Nova', sortOrder: 2 },
  { objectID: 'product_4', name: 'Delta Phone', brand: 'Nova', sortOrder: 4 },
  { objectID: 'product_5', name: 'Epsilon Tablet', brand: 'Zenith', sortOrder: 5 },
]);

export const INDEX_SETTINGS = Object.freeze({
  searchableAttributes: ['name'],
  attributesForFaceting: ['brand'],
  customRanking: ['asc(sortOrder)'],
  paginationLimitedTo: 100,
});

export const FIRST_PAGE_NAMES = Object.freeze(['Alpha Laptop', 'Gamma Phone']);
export const SECOND_PAGE_NAMES = Object.freeze(['Beta Laptop', 'Delta Phone']);
export const LAPTOP_NAMES = Object.freeze(['Alpha Laptop', 'Beta Laptop']);
export const NOVA_NAMES = Object.freeze(['Gamma Phone', 'Delta Phone']);
