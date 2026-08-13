# SDK outbound publication receipt

Date: 2026-08-09

## Purpose

This receipt closes the in-repository `aug08_9pm_2_sdk` evidence lane without performing new SDK publication work. It reconciles the `SDK-1` clauses from the live owner-repository evidence under `engine/docs2/4_EVIDENCE/aug08_9pm_2_sdk/`.

## Per-clause verdicts

| Clause | Verdict | Evidence |
| --- | --- | --- |
| `(a)` Go publication | Done for `github.com/flapjackhq/flapjack-search-go/v4` `v4.0.1`. | `go_publish_evidence/go_get_latest.log` shows clean-room `go get` resolved `v4.0.1`; `go_publish_evidence/final_validation_2.log` proves `@latest` chose `v4.0.1`, plain versions omit `v4.0.0`, and `-retracted` still lists `v4.0.0`; `go_publish_evidence/final_validation_3.log` records two observed Flapjack hosts and zero Algolia host attempts. |
| `(b)` live owner source scans | Done for the scanned owner branches. | The final `leak_scan_after.json` zero-hit scan used the widened runtime-host discriminator and covered Go 349 files on 1 branch, Python 759 files on 1 branch, and Ruby 1457 files on 2 branches. |
| `(c)` advisories | Done. | `advisory_index.json` records three published GitHub Security Advisories: Go `GHSA-jc2w-7wq6-r5w7`, Python `GHSA-jhcc-64c6-pfq2`, and Ruby `GHSA-q67x-w5fw-5mw2`. |
| `(d)` registry packages | Blocked for PyPI and RubyGems. | `registry_channel_disposition.md` records no fixed PyPI or RubyGems package live. `registry_credential_probe.md` records missing `/Users/stuart/.pypirc`, missing `/Users/stuart/.gem/credentials`, absent `TWINE_*`, and absent `GEM_HOST_*`; `release_workflow_disposition.md` records that inherited Algolia-owned release workflows were removed and no replacement Flapjack-owned publish path was proven. |

## Source and tag references

These rows record the first source-fix heads captured during Stage 5 plus the published
Go tag. They are not the later final reviewed refs used for the Stage 6 advisories
and the zero-hit owner scan.

| Channel | Before ref | Stage 5 source-fix ref or tag | Evidence file |
| --- | --- | --- | --- |
| Go source owner | `b5c668476ffe7653790d455baa55297d83fa3b9f` | `ae6aecf2fe92d0eaf107bcc0dea9cf4c397bc5d7` | `head_before_go.txt`, `go_reviewed_sha.txt`, `go_head_after_sources.txt` |
| Go `v4.0.1` tag | n/a | `ae6aecf2fe92d0eaf107bcc0dea9cf4c397bc5d7` | `go_publish_evidence/go_tag_v4_0_1_sha.txt` |
| Python source owner | `23e5bf3a65070fd7d9995fbc17dcd11070fa9edf` | `1665b7aad95782a1704f6c7fa5c4b3735ce5f04f` | `head_before_python.txt`, `python_head_after_sources.txt` |
| Ruby `main` source owner | `ad1e0ec7c393fbd3be9fc11bba9747c7ee815aa3` | `860cfd5cda25159619b697f18b708dbc65e4105f` | `head_before_ruby.txt`, `ruby_head_after_sources.txt` |
| Ruby non-main source owner | `ad1e0ec7c393fbd3be9fc11bba9747c7ee815aa3` | `4d4a884ffa0d6cb4216cee893823c89009df14d3` | `head_before_ruby.txt`, `ruby_nonmain_head_after_sources.txt` |

The final reviewed refs after Stage 5 cleanup and workflow removal, and the refs
named by the published Stage 6 advisories, are: Go `ae6aecf2fe92d0eaf107bcc0dea9cf4c397bc5d7`,
Python `b162fd04b2e98624c007e0d9b4bb7454347a49c6`, Ruby main `066acbfe2e22a0ad10b8d95cb953ae3c43c786f5`,
and Ruby non-main cleanup `4698a5df279c3c78b88653e58fbc0a3878fdb4ba` (`stage5_validation_summary.md`,
`advisory_index.json`).

## Scan denominators

Initial inventory in `leak_scan.json` measured 23 owner-source hits across 4 remote branches and 2568 Git tree files:

| Owner repository | Branches | Files | Text files | Hits |
| --- | ---: | ---: | ---: | ---: |
| `flapjackhq/flapjack-search-go` | 1 | 348 | 348 | 1 |
| `flapjackhq/flapjack-search-python` | 1 | 760 | 760 | 10 |
| `flapjackhq/flapjack-search-ruby` | 2 | 1460 | 1460 | 12 |

Published artifact inventory in `published_artifact_scan.json` measured 16 current-package hits across 1470 extracted files:

| Artifact channel | Version | Files | Text files | Hits | SHA-256 |
| --- | --- | ---: | ---: | ---: | --- |
| PyPI `flapjack-search` | `1.0.0` | 751 | 751 | 10 | `8a6cbc7a1996cddcbfbe9310709af311905285c3511ece692ed8c3f351b12683` |
| RubyGems `flapjack-search` | `0.1.0.pre.beta.1` | 719 | 719 | 6 | `0bd9e77aaebf05b9497ec11b17f8114eba17d15d4e3fe219e2da1418a84a6ab3` |

Final owner-source inventory in `leak_scan_after.json` is non-vacuous and zero-hit with the widened runtime-host discriminator:

| Owner repository | Branches | Files | Text files | Hits |
| --- | ---: | ---: | ---: | ---: |
| `flapjackhq/flapjack-search-go` | 1 | 349 | 349 | 0 |
| `flapjackhq/flapjack-search-python` | 1 | 759 | 759 | 0 |
| `flapjackhq/flapjack-search-ruby` | 2 | 1457 | 1457 | 0 |

## Go publication correction

Go clause `(a)` is done because `v4.0.1` is the clean-room `@latest` version and the counting requester observed zero Algolia host attempts. The correction is narrower than the old `SDK-1` wording: Go retraction removes `v4.0.0` from `@latest` and from plain `go list -m -versions`, and marks it under `go list -m -versions -retracted`; it does not hard-refuse an explicit `v4.0.0`.

## Remaining open work

`SDK-1` stays open only for clause `(d)`: PyPI `flapjack-search` `1.0.0` and RubyGems `flapjack-search` `0.1.0.pre.beta.1` remain the live registry packages, and no credential or Flapjack-owned publish path was proven in this lane. Because clause `(d)` remains blocked, the `jul16_3pm_1_sdk_outbound_safety` ledger anchor must remain in `ROADMAP.md`.
