# ACME Material Lifecycle Receipt

Date: 2026-08-03
Final HEAD verified: `6bfb485e06cb75e6871c583b1f02882ec16d715d`
Lane: `aug03_11am_3_acme_material_lifecycle`
Stage 4 edit scope: this receipt only.

## Publication Contract For L7

The ACME publication owner is `engine/flapjack-ssl/src/manager.rs::SslManager`.
The config owner is `engine/flapjack-ssl/src/config.rs::SslConfig::from_env`.

Visible material names are exactly:

- `fullchain.pem`
- `privkey.pem`

`FLAPJACK_ACME_MATERIAL_DIR` overrides the material directory. Without that
override, `SslConfig::resolve_material_dir` resolves
`${FLAPJACK_DATA_DIR:-./data}/ssl/acme`.

`SslManager::write_certificate_files_to_dir` stages both PEMs into one
same-parent generation before publication. The generation directory is
owner-private (`0o700`). The private key file is created private (`0o600` on
Unix). Both files are written and fsynced inside the generation, then the
generation directory and its parent are synced before the visible target changes.

On Unix, publication is not two independent file renames:

- First publication installs the visible material path as a symlink to the
  staged generation.
- Replacement of a symlink material path creates a staged sibling symlink, fsyncs
  the parent, then atomically renames the staged link over the visible material
  link.
- Replacement of an existing real material directory routes `fullchain.pem` and
  `privkey.pem` through one `current` symlink inside that directory, then flips
  `current` to the new generation with one atomic symlink replacement.
- The live generation and the immediately previous complete generation are
  retained so a reader that resolved the previous target before the swap can
  still open both files. Older owner-scoped generations are retired.

On non-Unix platforms, first publication is supported after syncing file
contents. Replacement of an existing material directory fails before visible
mutation instead of pretending directory replacement is atomic.

L7's consumer rule: detect publication by observing a changed resolved
generation target, then load `fullchain.pem` and `privkey.pem` through that same
resolved generation. Do not independently resolve the certificate and key paths
as two separate publications.

## Historical Contract Evidence

Stage 1 red evidence is from
`session_handoffs/stage_01/s17_stage_review_closed-three-red-guard-gaps.md`:
`cargo test -p flapjack-ssl --lib --no-fail-fast` exited red with
`15 passed; 9 failed`. The nine behavioral assertion failures were:

- `acme::tests::requested_dns_value_constructs_dns_identifier`
- `manager::tests::renewal_publication_wiring_preserves_issued_key_and_resolved_path`
- `manager::tests::certificate_path_defaults_under_flapjack_data_dir`
- `manager::tests::publication_persists_certificate_and_parseable_private_key`
- `manager::tests::published_private_key_is_owner_private`
- `manager::tests::publication_replaces_existing_real_material_directory_as_pair`
- `tests::production_ssl_source_contains_no_nginx_reload_assumption`
- `manager::tests::publication_commits_complete_generation_without_mutating_visible_pair`
- `manager::tests::publication_fault_injection_rejects_both_sequential_write_orders`

Stage 2 green evidence is from
`session_handoffs/stage_02/s34_stage_review_fixed-lifecycle-boundary-defects.md`:
`cargo test -p flapjack-ssl --lib --no-fail-fast` passed with `45 passed`.

Stage 3 added the rustls crypto-provider guard. Stage 3 and this Stage 4 final
suite are therefore distinct from the Stage 2 denominator: the current
`flapjack-ssl` library suite is `46 passed; 0 failed`.

## Durable Pebble Evidence

Durable committed Stage 3 evidence lives under
`engine/loadtest/results/20260803-acme-pebble-kat-evidence/`:

- `README.md`
- `summary_dns.txt`
- `summary_ip.txt`
- `assertions_dns.txt`
- `assertions_ip.txt`

That evidence records the reachability choice: `flapjack-server` runs on the
host and Pebble reaches it through the host-gateway address resolved from inside
Docker. The DNS and IP arms both passed `16/16` checks. The DNS SAN was
`DNS:flapjack-kat.test`; the IP SAN was `IPAddress:192.168.5.2`. The assertions
also pin generation mode `700`, key mode `600`, key/certificate match,
Pebble-root verification, HTTP-01 route reachability and token retirement, and
run-scoped zero-container teardown.

## Fresh Final-HEAD Evidence

Fresh DNS KAT:

```bash
cd engine && ./_dev/s/manual-tests/acme_pebble_kat.sh --identifier dns > /tmp/l3_kat_final_dns.txt 2>&1
```

Result: exit `0`, `verdict=PASS`, `checks=16`, `failures=0`.
Fresh run directory:
`engine/loadtest/results/20260803T224719Z-acme-pebble-kat-dns`.

Required DNS assertions from that run:

- `leaf_subject_alt_name = DNS:flapjack-kat.test`
- `generation_dir_mode = 700`
- `private_key_mode = 600`
- `chain_verifies_to_pebble_root .../leaf.pem: OK`
- `teardown_leaves_no_pebble_containers docker ps -a filter project=flapjack-acme-kat-dns-47683 empty`

Fresh IP KAT:

```bash
cd engine && ./_dev/s/manual-tests/acme_pebble_kat.sh --identifier ip > /tmp/l3_kat_final_ip.txt 2>&1
```

Result: exit `0`, `verdict=PASS`, `checks=16`, `failures=0`.
Fresh run directory:
`engine/loadtest/results/20260803T224934Z-acme-pebble-kat-ip`.

Required IP assertions from that run:

- `leaf_subject_alt_name = IPAddress:192.168.5.2`
- `generation_dir_mode = 700`
- `private_key_mode = 600`
- `chain_verifies_to_pebble_root .../leaf.pem: OK`
- `teardown_leaves_no_pebble_containers docker ps -a filter project=flapjack-acme-kat-ip-50603 empty`

## Served Handshake Proof

The served proof used the fresh DNS KAT pair:

- certificate: `engine/loadtest/results/20260803T224719Z-acme-pebble-kat-dns/material/acme/fullchain.pem`
- key: `engine/loadtest/results/20260803T224719Z-acme-pebble-kat-dns/material/acme/privkey.pem`

The check launched exactly one static TLS listener from this session, PID
`10420`, on port `54943`, then terminated only that PID.

The captured handshake command was:

```bash
openssl s_client -connect 127.0.0.1:54943 -servername flapjack-kat.test -showcerts
```

Transcript: `/tmp/l3_acme_served_handshake.txt`.

The transcript includes `CONNECTED(00000003)`, a served certificate chain,
`New, TLSv1.3, Cipher is TLS_AES_256_GCM_SHA384`, and `DONE`, proving a completed
server handshake rather than an offline certificate parse.

The served leaf was extracted to `/tmp/l3_acme_served_leaf.pem` and verified
against the same run's issuing root:

```bash
openssl verify -CAfile engine/loadtest/results/20260803T224719Z-acme-pebble-kat-dns/pebble_issuing_root.pem \
  -untrusted /tmp/l3_acme_served_intermediates.pem \
  /tmp/l3_acme_served_leaf.pem
```

Result: `/tmp/l3_acme_served_leaf.pem: OK`.

Served leaf metadata:

- SHA-256 fingerprint:
  `37:0B:61:1A:B2:3B:AE:C9:75:E1:B1:86:19:3C:CC:22:90:66:1B:C0:FA:60:AB:87:18:A8:90:DD:C6:2C:42:85`
- Subject Alternative Name: `DNS:flapjack-kat.test`

No private key or generated admin key is copied into this receipt or into git.

## Final Rust Gates

All commands were checked through the validation-cache helper before execution
and recorded afterward for `matt_dir=.../aug03_11am_3_acme_material_lifecycle.md-db9050b0`.

```bash
(cd engine && timeout 600 cargo test -p flapjack-ssl --lib > /tmp/l3_stage4_unit.txt 2>&1)
```

Result: exit `0`;
`test result: ok. 46 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out`.
No timeout occurred, so no orphan sweep was required for this command.

```bash
(cd engine && cargo clippy -p flapjack-ssl)
```

Result: exit `0`.

```bash
(cd engine && cargo fmt --check)
```

Result: exit `0`.

## Clauses Closed By This Lane

This lane closes these four `SEC-W2A` clauses:

- Issued private keys are persisted with the certificate material.
- Certificate/key pairs are published atomically as one generation.
- Plaintext HTTP-01 reachability is preserved for issuance and challenge tokens
  are retired after issuance.
- The nginx-only `systemctl reload nginx` assumption is removed from the ACME
  renewal path.

This lane also provides real local-ACME known-answer evidence through Pebble for
the DNS and IP identifier arms. It does not close the whole ledger row.

## Clauses Still Open For L7

L7 still owns:

- The rustls reload contract for consuming changed ACME generations.
- Served rotation without process restart.

Owner files for L7 are:

- `engine/flapjack-http/src/tls_serve.rs`
- `engine/flapjack-http/src/server_init.rs`
- `engine/flapjack-http/src/background_tasks.rs`
- `engine/flapjack-http/src/router.rs`

L7 must wait for this lane to merge before dispatching against this publication
contract.

## Proposed Ledger Text For L11

L11 is the batch's sole ledger writer. Proposed `SEC-W2A` narrowing text:

`ACME material lifecycle is verified at 6bfb485e06cb75e6871c583b1f02882ec16d715d:
issued private keys persist beside full chains, fullchain/key pairs publish as a
single fsynced generation with owner-private permissions, HTTP-01 remains
reachable through the in-binary route and retires tokens after issuance, the
nginx reload assumption is removed, and Pebble DNS/IP known-answer tests plus a
static served-handshake proof pass. Remaining work is L7-owned rustls generation
reload and served rotation without restart.`

This is proposed text only. L11 applies any ledger edit and decides the final
row wording.
