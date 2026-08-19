# SH-KEYS — API key interaction shell

`ApiKeyShell` is the portable presentation and interaction boundary for API-key collections. It
owns loading, error, empty and filtered-empty presentation; exact index filtering; create, copy,
and removal intents; and temporary copy feedback.

Hosts own data acquisition, filter persistence, key-domain fields, creation forms, confirmation
policy, deletion identifiers, mutation state, and focus return. An empty `indexNames` list means a
key is unrestricted and therefore remains visible under every index filter.

`opaqueId` and `copyText` are sensitive interaction metadata. The shell may use them in memory to
identify an item or invoke the supplied clipboard callback, but must not place them in DOM IDs,
test IDs, URLs, logs, errors, or other diagnostics. Host details are rendered through the supplied
snippet and remain escaped by Svelte.

Hosts select whether this section owns a level-one or level-two heading and may hold all shell
controls disabled until host interaction wiring is ready. Disabled interaction is fail-closed: no
create, filter, copy, or removal callback fires.

The supported populated state is registered in `ApiKeyShell.stories.ts`. Loading, retry, empty,
filtering, clipboard success/failure, removal intent, and escaping are owned by
`component-tests/ApiKeyShell.test.ts`.
