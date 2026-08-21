import instantsearch from 'instantsearch.js';
import { configure, hits, pagination, refinementList, searchBox } from 'instantsearch.js/es/widgets';

export function mountSearch({ root, searchClient, indexName, userToken, trackClick }) {
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
    configure({ hitsPerPage: 2, clickAnalytics: true, userToken }),
    searchBox({ container: '#searchbox', placeholder: 'Search products' }),
    refinementList({ container: '#brand', attribute: 'brand' }),
    hits({
      container: '#hits',
      templates: {
        item(hit, { html }) {
          return html`<button data-testid="hit" type="button" data-object-id=${hit.objectID} data-query-id=${hit.__queryID} data-position=${hit.__position}><span data-testid="hit_name">${hit.name}</span></button>`;
        },
        empty: 'No matching products',
      },
    }),
    pagination({ container: '#pagination' }),
  ]);
  root.addEventListener('click', (event) => {
    const hit = event.target.closest('[data-testid="hit"]');
    if (!hit) return;
    trackClick({
      objectID: hit.dataset.objectId,
      __queryID: hit.dataset.queryId,
      __position: Number(hit.dataset.position),
    });
  });
  search.start();
}
