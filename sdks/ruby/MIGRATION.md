# Migrating from Algolia to Flapjack (Ruby)

## Gem Change

```ruby
# Before (Algolia)
gem 'algolia'

# After (Flapjack)
gem 'flapjack-search'
```

RubyGems returned `0.1.0.pre.beta.1` on 2026-06-03. Treat the public gem as prerelease until a stable SDK release is published.

## Require Change

```ruby
# Before
require 'algolia'

# After
require 'flapjack'
```

## Namespace Change

```ruby
# Before
client = Algolia::SearchClient.create(app_id, api_key)

# After
client = Flapjack::SearchClient.create(app_id, api_key)
```

## Error Classes

| Algolia | Flapjack |
|---------|----------|
| `Algolia::AlgoliaError` | `Flapjack::FlapjackError` |
| `Algolia::AlgoliaHttpError` | `Flapjack::FlapjackHttpError` |
| `Algolia::AlgoliaUnreachableHostError` | `Flapjack::FlapjackUnreachableHostError` |

## Self-Hosted Setup

For self-hosted Flapjack servers in production or staging, configure custom hosts over HTTPS:

```ruby
require 'flapjack'

hosts = [
  Flapjack::Transport::StatefulHost.new(
    'search.example.com',
    protocol: 'https://',
    accept: CallType::READ | CallType::WRITE
  )
]

config = Flapjack::Configuration.new('app-id', 'api-key', hosts, 'Search')
client = Flapjack::SearchClient.create_with_config(config)
```

For local development:

```ruby
hosts = [
  Flapjack::Transport::StatefulHost.new(
    '127.0.0.1',
    protocol: 'http://',
    port: 7700,
    accept: CallType::READ | CallType::WRITE
  )
]
```

## Expected Compatibility Surface

Verify these against the exact gem version you install; the public RubyGems package is prerelease as of the 2026-06-03 registry probe.

- Core search/write method names are intended to track the Algolia Ruby SDK shape.
- `x-algolia-*` wire headers, search parameters, and response formats remain the compatibility target.
- Model structure and frontend compatibility should be checked against the stable SDK release when one exists.
