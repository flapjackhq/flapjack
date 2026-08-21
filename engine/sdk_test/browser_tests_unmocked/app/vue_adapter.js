import { createApp, h } from 'vue';
import {
  AisConfigure,
  AisHits,
  AisInstantSearch,
  AisPagination,
  AisRefinementList,
  AisSearchBox,
} from 'vue-instantsearch/vue3/es';

export function mountSearch({ root, searchClient, indexName, userToken, trackClick }) {
  const app = createApp({
    render() {
      return h(AisInstantSearch, { indexName, searchClient }, {
        default: () => h('section', { 'data-testid': 'search_ui' }, [
          h(AisConfigure, { hitsPerPage: 2, clickAnalytics: true, userToken }),
          h(AisSearchBox, { placeholder: 'Search products' }),
          h(AisRefinementList, { attribute: 'brand' }),
          h(AisHits, {}, {
            default: ({ items }) => h('ol', items.map((item) => {
              return h('li', { key: item.objectID },
                h('button', { 'data-testid': 'hit', type: 'button', onClick: () => trackClick(item) },
                  h('span', { 'data-testid': 'hit_name' }, item.name)));
            })),
          }),
          h(AisPagination),
        ]),
      });
    },
  });
  app.mount(root);
}
