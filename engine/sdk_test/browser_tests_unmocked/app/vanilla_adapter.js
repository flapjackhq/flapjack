import instantsearch from 'instantsearch.js';
import { configure, hits, pagination, refinementList, searchBox } from 'instantsearch.js/es/widgets';

export function mountSearch({ root, searchClient, indexName }) {
  root.innerHTML = `
    <section data-testid="search_ui">
      <div id="searchbox"></div>
      <div id="brand"></div>
      <div id="hits"></div>
      <div id="pagination"></div>
    </section>
  `;

  const search = instantsearch({ indexName, searchClient });
  search.addWidgets([
    configure({ hitsPerPage: 2 }),
    searchBox({ container: '#searchbox', placeholder: 'Search products' }),
    refinementList({ container: '#brand', attribute: 'brand' }),
    hits({
      container: '#hits',
      templates: {
        item(hit, { html }) {
          return html`<article data-testid="hit"><span data-testid="hit_name">${hit.name}</span></article>`;
        },
        empty: 'No matching products',
      },
    }),
    pagination({ container: '#pagination' }),
  ]);
  search.start();
}
