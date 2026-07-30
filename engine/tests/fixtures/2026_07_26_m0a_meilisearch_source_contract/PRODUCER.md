# Fixture producer

Flapjack is the canonical producer of these fixture payloads. A downstream consumer vendors byte-identical copies.

Any payload change requires regenerating the local `CHECKSUMS.txt` in the same commit and re-vendoring the updated payloads downstream.

## Harness distinction

`engine/tests/meilisearch_source_contract_kat.sh` verifies Flapjack's Meilisearch adapter contract against pinned fixtures and images.

A downstream consumer has a separate harness that seeds live source containers for a browser-console migration flow; that harness serves a different purpose and is not a replacement for this producer contract.
