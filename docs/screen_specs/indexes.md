# Index List

## Task

Choose an index to search and confirm its exact record count and stored byte count.

## Layout

1. The `Indexes` heading names the screen.
2. A table preserves transport order and shows Name, Entries, and Data size.
3. Each index name opens basic Search for that index.

## State contract

### Loading

- `Loading indexes...` is announced while one list request is pending.

### Error

- A fixed safe error and `Retry loading indexes` are shown. There is no automatic retry and an error is never presented as an empty list.

### Empty

- `No indexes yet.` is shown only after a successful empty response.

### Populated

- Name, entry count, and literal byte count come directly from the normalized transport contract. Host-only metadata is not invented.

## Navigation

- Route: `/dashboard/`
- Entry: successful standalone authentication.
- Index name: opens basic Search locally without making a search request.

## Acceptance criteria

- Given a successful response, each exact normalized index value is visible in transport order.
- Given a failed response, retry makes exactly one new list request.
- Selecting an index focuses the Search query input and makes zero search requests.

## Edge cases

- Empty and failed responses remain visibly distinct.
- This first shared slice is read-only; create, delete, and managed-only metadata are outside its contract.
