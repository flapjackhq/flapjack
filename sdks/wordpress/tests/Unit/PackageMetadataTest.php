<?php
/**
 * Tests for public package metadata.
 *
 * @package Flapjack\WordPress\Tests\Unit
 */

declare(strict_types=1);

namespace Flapjack\WordPress\Tests\Unit;

use PHPUnit\Framework\TestCase;

class PackageMetadataTest extends TestCase {

	public function test_php_floor_is_consistent_across_declarations(): void {
		$metadata = $this->read_package_metadata();

		$this->assertSame( '8.1', $metadata['composer_minimum'] );

		$header_matches_composer = version_compare( $metadata['header_floor'], $metadata['composer_minimum'], '==' );
		$readme_matches_composer = version_compare( $metadata['readme_floor'], $metadata['composer_minimum'], '==' );

		$this->assertTrue(
			$header_matches_composer && $readme_matches_composer,
			sprintf(
				'Advertised PHP floors must match composer.json=%s; flapjack-search.php=%s, readme.txt=%s.',
				$metadata['composer_minimum'],
				$metadata['header_floor'],
				$metadata['readme_floor']
			)
		);
	}

	public function test_advertised_floor_is_not_below_composer_minimum(): void {
		$metadata = $this->read_package_metadata();

		$header_meets_minimum = version_compare( $metadata['header_floor'], $metadata['composer_minimum'], '>=' );
		$readme_meets_minimum = version_compare( $metadata['readme_floor'], $metadata['composer_minimum'], '>=' );

		$this->assertTrue(
			$header_meets_minimum && $readme_meets_minimum,
			sprintf(
				'Advertised PHP floors must not be below composer.json=%s; flapjack-search.php=%s, readme.txt=%s.',
				$metadata['composer_minimum'],
				$metadata['header_floor'],
				$metadata['readme_floor']
			)
		);
	}

	public function test_plugin_header_declares_php_floor_with_canonical_spacing(): void {
		$metadata = $this->read_package_metadata();

		// This exact spacing pins the downstream L6 orchestration gate; it is not cosmetic.
		$this->assertStringContainsString( 'Requires PHP:      8.1', $metadata['plugin_contents'] );
	}

	public function test_readme_advertises_atomic_full_reindex_through_canonical_entrypoint(): void {
		$metadata = $this->read_package_metadata();

		$this->assertStringContainsString(
			'**Atomic full reindex** — Admin, WP-CLI, and REST rebuild into a temporary index and publish it over the live index only after the rebuild succeeds',
			$metadata['readme_contents'],
			'The readme must advertise the guarded atomic full-reindex behavior.'
		);
		$this->assertDoesNotMatchRegularExpression( '/zero[- ]downtime/i', $metadata['readme_contents'] );

		$method_match_count = preg_match(
			'/public function reindex_all\(\): array \{(?<body>.*?)^    \}/ms',
			$metadata['index_manager_contents'],
			$method_matches
		);
		$this->assertSame( 1, $method_match_count, 'IndexManager::reindex_all() must remain the canonical full-reindex entrypoint.' );
		foreach ( [
			'/\$this->begin_rebuild\(\);/',
			'/\$counts\s*=\s*\$this->build_temp_index\(\);/',
			'/\$this->publish_rebuild\(\);/',
		] as $required_call_pattern ) {
			$this->assertMatchesRegularExpression( $required_call_pattern, $method_matches['body'] );
		}
		$this->assertStringNotContainsString( 'reindex_atomic', $method_matches['body'] );
	}

	/**
	 * @return array{composer_minimum: string, header_floor: string, readme_floor: string, plugin_contents: string, readme_contents: string, index_manager_contents: string}
	 */
	private function read_package_metadata(): array {
		$plugin_root = dirname( __DIR__, 2 );
		$composer_contents = file_get_contents( $plugin_root . '/composer.json' );
		$plugin_contents = file_get_contents( $plugin_root . '/flapjack-search.php' );
		$readme_contents = file_get_contents( $plugin_root . '/readme.txt' );
		$index_manager_contents = file_get_contents( $plugin_root . '/includes/Indexing/IndexManager.php' );

		$this->assertNotFalse( $composer_contents, 'Unable to read composer.json.' );
		$this->assertNotFalse( $plugin_contents, 'Unable to read flapjack-search.php.' );
		$this->assertNotFalse( $readme_contents, 'Unable to read readme.txt.' );
		$this->assertNotFalse( $index_manager_contents, 'Unable to read IndexManager.php.' );

		$composer = json_decode( $composer_contents, true );
		$this->assertIsArray( $composer, 'composer.json must decode to an object.' );
		$this->assertArrayHasKey( 'require', $composer, 'composer.json must declare require.' );
		$this->assertIsArray( $composer['require'], 'composer.json require must be an object.' );
		$this->assertArrayHasKey( 'php', $composer['require'], 'composer.json must declare require.php.' );
		$this->assertIsString( $composer['require']['php'], 'composer.json require.php must be a string.' );

		$composer_match_count = preg_match( '/^\s*>=\s*(\d+(?:\.\d+){0,2})\s*$/', $composer['require']['php'], $composer_matches );
		$header_match_count = preg_match( '/^\s*\*\s*Requires PHP:\s*(\d+(?:\.\d+){0,2})\s*$/mi', $plugin_contents, $header_matches );
		$readme_match_count = preg_match( '/^\s*Requires PHP:\s*(\d+(?:\.\d+){0,2})\s*$/mi', $readme_contents, $readme_matches );

		$this->assertSame( 1, $composer_match_count, 'composer.json require.php must be a >= numeric version constraint.' );
		$this->assertSame( 1, $header_match_count, 'flapjack-search.php must contain a Requires PHP header.' );
		$this->assertSame( 1, $readme_match_count, 'readme.txt must contain a Requires PHP declaration.' );

		return [
			'composer_minimum'      => $composer_matches[1],
			'header_floor'           => $header_matches[1],
			'readme_floor'           => $readme_matches[1],
			'plugin_contents'        => $plugin_contents,
			'readme_contents'        => $readme_contents,
			'index_manager_contents' => $index_manager_contents,
		];
	}
}
