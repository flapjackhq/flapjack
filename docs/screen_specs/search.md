# Basic Search

## Task

Submit a query against one selected index and inspect exact read-only results.

## Layout

1. The heading names the selected index and `Back to indexes` returns to the loaded list.
2. A labelled Query input and Search button submit an explicit search.
3. Results show exact total count, engine processing time, safe result cards, and basic page controls.
   Each card has an explicit `Open details` disclosure for its raw JSON.
4. When the host supplies both preview-analytics behavior and copy, a default-off analytics
   control appears. Standalone copy is `Track Analytics`. A host that supplies only one half
   exposes no control and sends no analytics fields.
5. When the host supplies semantic-search discovery, query-capable embedders produce a native
   semantic-ratio control and an explicit query-embedder selector. Raw provider configuration
   and credentials are never rendered.

## State contract

### Ready

- The query input is focused. No search has run and no no-results claim is shown.

### Searching

- `Searching...` is announced and duplicate submission is disabled. Prior results remain visible during replacement searches.

### Error

- A fixed safe error and `Retry search` are shown. The draft, last committed request, and prior results remain; retry repeats the committed request once.

### Results

- An explicit blank query is valid and browses all records.
- Count, processing time, hits, and page limits come from the normalized response. Displayed pages are one-based; transport pages are zero-based.
- A card title uses the first non-empty string from `title`, `name`, and `objectID`, then
  falls back to the absolute one-based `Result N` position.
- Raw hit JSON is hidden until that card's native `Open details` disclosure is activated.
  Opening one card does not open its siblings, and a successful replacement search closes
  prior disclosures. The control reads `Close details` while open.
- A successful zero-hit response shows `No results.`

### Preview analytics

- Availability is host-composed. Without both the capability and its typed copy, searches omit
  `analytics`, `clickAnalytics`, and any user token.
- Available but Off sends `analytics=false`, omits `clickAnalytics`, and creates or sends no token.
- On applies only to subsequent searches, which send `analytics=true`, `clickAnalytics=true`,
  and a host-private session token. The returned exact 32-character hexadecimal `queryID`
  correlates an explicit result open.
- Every toggle transition and search start invalidates prior correlation. Retained hits remain
  inspectable while a replacement is pending or after it fails, but cannot emit an event with a
  stale query ID.
- Only a genuine closed-to-open details transition records an event. It uses the hit's non-empty
  `objectID` and the response-owned absolute one-based position. Closing or a redundant toggle
  records nothing; reopening is a new explicit open.
- `Recorded result open.` appears only after delivery is acknowledged. Missing correlation or
  object identity produces fixed guidance. Delivery failure shows `Result open was not recorded.`
  and never exposes a host error or token. A later toggle, search, or open supersedes pending
  feedback from an older attempt.

### Semantic search

- Capability absence preserves Basic Search and performs no discovery request. An engine that
  reports vector search unavailable is treated the same way and its index settings are not read.
- Discovery is read-only and does not block keyword Search. While a supplied capability is
  pending, unavailable, or failed, Search explicitly sends `mode=keywordSearch`. A fixed safe
  configuration error has an explicit Retry; no provider response or configuration detail is
  exposed.
- Only `openAi` and `rest` embedders can embed queries in every vector-enabled build.
  `fastEmbed` is available only when the engine reports local vector search; `userProvided`
  remains document-vector-only. Names are sorted.
- The default ratio is `0.5` and is labelled `Balanced`, matching the current standalone
  dashboard. An explicit Search at zero sends `mode=keywordSearch` with no `hybrid` member. A
  positive ratio sends `mode=neuralSearch` with the exact committed ratio and an explicit embedder
  name, including when there is only one choice.
- Control changes never search. Search snapshots the current semantic draft; paging and Retry
  preserve that committed ratio and embedder even if the visible controls later change.
- A normalized semantic fallback keeps results usable and shows only
  `Semantic search was unavailable; keyword results are shown.` The backend reason is never
  exposed.

## Navigation

- Routes: choose an index from `Index List` at `/dashboard/`, or open the standalone deep link
  `/dashboard/index/:indexName`. Legal dots in the index name remain part of the decoded index
  identity rather than being treated as an asset extension.
- Back: the aggregate view returns to its already-loaded list without refetching. The standalone
  deep route uses a base-aware `Back to indexes` link to `/dashboard/`.

## Acceptance criteria

- Button and Enter submission each make one request with the selected index, draft query, page `0`, and `20` hits per page.
- Previous and Next use the last committed query and only the response's `page` and `nbPages`.
- A changed query resets the request page to `0`.
- Result titles and hit JSON are rendered as text, never interpreted as HTML.
- The analytics Off and On request bodies, response query ID, event query ID, object ID, and
  absolute position remain exact across the transport boundary.
- Semantic discovery, zero-ratio keyword requests, positive-ratio neural requests, committed
  paging/Retry, and fixed fallback disclosure remain exact across the same transport boundary.

## Edge cases

- A full page does not imply another page when `nbPages` says there is none.
- Facets, filters, highlighting, analytics tags, record mutation, and search-as-you-type are
  outside this slice.

## Automated coverage

- The supported ready state is registered in
  `engine/console/src/lib/features/IndexSearch.stories.ts`.
- Component behavior and interaction are owned by
  `engine/console/component-tests/IndexListSearch.test.ts`.
- Real-engine accessibility, keyboard interaction, and desktop/390px viewport proof are owned by
  `engine/console/browser-tests-unmocked/smoke/index-list-search.spec.ts`.
