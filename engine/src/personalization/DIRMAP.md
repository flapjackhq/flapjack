<!-- [scrai:start] -->
## personalization

| File | Summary |
| --- | --- |
| mod.rs | Personalization profile computation.



Design notes:

- Build per-user affinities from insight events and indexed document facets.

- Use the strategy weights with raw score formula:

  raw_score = interaction_count * event_score * facet_score.

- Only include events from the last 90 days.

- Normalize per user so the strongest affinity is exactly 20 and all others

  are scaled relative to that strongest signal. |
| profile.rs | Personalization profile computation and storage. |
<!-- [scrai:end] -->
