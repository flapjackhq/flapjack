# Private TODO (gitignored - only visible locally)

## URGENT: Rotate Leaked Credentials

These were in flapjack202511 git history. Rotate before this repo gets real traffic.

- [ ] AWS IAM key `AKIATDTCLTRFI4NSM5P6` - deactivate in IAM console, create new
- [ ] Algolia admin keys for app `9HEYZQZHL7` (US-East)
- [ ] Algolia admin keys for app `NUY9M4C8ES` (US-West)
- [ ] Typesense admin keys (2 clusters)
- [ ] Meilisearch API keys
- [ ] Cloudflare API token `sy2bXIz...` - revoke and regenerate
- [ ] Cloudflare Global API key `ee727...` - regenerate

## Repo Notes

- Source of truth: flapjack202511 (private). Never edit code here directly.
- Sync: `~/bin/sync-flapjack.sh` then review diff, commit, push.
- Tantivy dep uses `[patch.crates-io]` pointing to stuartcrobinson/tantivy branch nov19.
- CI auto-runs on push/PR. Release workflow is manual.
- Repos are under the `flapjackhq` GitHub org. Old stuartcrobinson URLs redirect automatically.
