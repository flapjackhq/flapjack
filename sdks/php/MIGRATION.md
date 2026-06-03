# Migration Guide: Algolia PHP SDK to Flapjack PHP SDK

This guide walks you through migrating from `algolia/algoliasearch-client-php` to `flapjackhq/flapjack-search-php`.

## 1. Install Flapjack SDK

```bash
composer remove algolia/algoliasearch-client-php
# Packagist returned no public versions for this package on 2026-06-03.
# Treat this guide as source-migration reference until a public Composer
# version is available.
```

## 2. Update Imports

Find and replace the namespace across your project:

```
# Old
use Algolia\AlgoliaSearch\...

# New
use Flapjack\FlapjackSearch\...
```

### Common import changes

```php
// Old
use Algolia\AlgoliaSearch\Api\SearchClient;
use Algolia\AlgoliaSearch\Configuration\SearchConfig;
use Algolia\AlgoliaSearch\Algolia;
use Algolia\AlgoliaSearch\Support\AlgoliaAgent;

// New
use Flapjack\FlapjackSearch\Api\SearchClient;
use Flapjack\FlapjackSearch\Configuration\SearchConfig;
use Flapjack\FlapjackSearch\Flapjack;
use Flapjack\FlapjackSearch\Support\FlapjackAgent;
```

## 3. Update Class References

| Old Class | New Class |
|-----------|-----------|
| `Algolia` | `Flapjack` |
| `AlgoliaAgent` | `FlapjackAgent` |
| `AlgoliaResponse` | `FlapjackResponse` |
| `Algolia::VERSION` | `Flapjack::VERSION` |
| `AlgoliaAgent::get()` | `FlapjackAgent::get()` |
| `AlgoliaAgent::addAlgoliaAgent()` | `FlapjackAgent::addFlapjackAgent()` |

## 4. Update Method Names

| Old Method | New Method |
|------------|-----------|
| `getAlgoliaApiKey()` | `getFlapjackApiKey()` |
| `setAlgoliaApiKey()` | `setFlapjackApiKey()` |
| `getAlgoliaAgent()` | `getFlapjackAgent()` |
| `setAlgoliaAgent()` | `setFlapjackAgent()` |

## 5. Environment Variables (Optional)

The SDK supports both old and new environment variables:

| Old Variable | New Variable |
|-------------|-------------|
| `ALGOLIA_APP_ID` | `FLAPJACK_APP_ID` |
| `ALGOLIA_API_KEY` | `FLAPJACK_API_KEY` |

The old `ALGOLIA_*` variables still work as fallbacks.

## 6. Self-Hosted Configuration (Optional)

If running a self-hosted Flapjack server in production or staging, prefer HTTPS:

```php
$config = SearchConfig::create('your-app-id', 'your-api-key');
$config->setFullHosts(['https://search.example.com']);
$client = SearchClient::createWithConfig($config);
```

For local development:

```php
$config = SearchConfig::create('your-app-id', 'your-api-key');
$config->setFullHosts(['http://127.0.0.1:7700']);
$client = SearchClient::createWithConfig($config);
```

## Expected Compatibility Surface

Verify these against the exact package version you install; no public Packagist version was available in the 2026-06-03 registry probe.

- Core search/write method names are intended to track the Algolia PHP SDK shape.
- Request/response formats and `x-algolia-*` wire headers remain the compatibility target.
- Exception classes, model classes, and HTTP transport behavior should be checked against the released package before production migration.

## Quick Regex for Bulk Migration

```bash
# In your project directory:
find . -name '*.php' -exec sed -i '' \
  -e 's/Algolia\\AlgoliaSearch/Flapjack\\FlapjackSearch/g' \
  -e 's/use Flapjack\\FlapjackSearch\\Algolia;/use Flapjack\\FlapjackSearch\\Flapjack;/g' \
  -e 's/Algolia::VERSION/Flapjack::VERSION/g' \
  -e 's/Algolia::getHttpClient/Flapjack::getHttpClient/g' \
  -e 's/AlgoliaAgent/FlapjackAgent/g' \
  -e 's/AlgoliaResponse/FlapjackResponse/g' \
  {} +
```
