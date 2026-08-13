import React, { createElement as h } from 'react';
import { createRoot } from 'react-dom/client';
import { Configure, Hits, InstantSearch, Pagination, RefinementList, SearchBox } from 'react-instantsearch';

export function mountSearch({ root, searchClient, indexName }) {
  function Hit({ hit }) {
    return h('article', { 'data-testid': 'hit' },
      h('span', { 'data-testid': 'hit_name' }, hit.name));
  }

  createRoot(root).render(
    h(InstantSearch, { indexName, searchClient },
      h('section', { 'data-testid': 'search_ui' },
        h(Configure, { hitsPerPage: 2 }),
        h(SearchBox, { placeholder: 'Search products' }),
        h(RefinementList, { attribute: 'brand' }),
        h(Hits, { hitComponent: Hit }),
        h(Pagination),
      )),
  );
}
