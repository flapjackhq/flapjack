<?php
/**
 * Proves every synchronous full-reindex entrypoint delegates to the single
 * IndexManager owner instead of driving the search client, the atomic-swap
 * internals, or background scheduling directly.
 *
 * @package Flapjack\WordPress\Tests\Unit\Indexing
 */

declare(strict_types=1);

namespace Flapjack\WordPress\Tests\Unit\Indexing;

use PHPUnit\Framework\TestCase;
use Flapjack\WordPress\CLI\Commands;
use Flapjack\WordPress\Admin\SettingsPage;
use Flapjack\WordPress\REST\IndexEndpoint;

class ReindexEntrypointsTest extends TestCase {

    /**
     * The CLI, synchronous admin AJAX, and REST reindex handlers.
     *
     * @return array<string, array{0: class-string, 1: string}>
     */
    public static function entrypointProvider(): array {
        return [
            'CLI'        => [ Commands::class, 'reindex' ],
            'admin AJAX' => [ SettingsPage::class, 'ajax_reindex' ],
            'REST'       => [ IndexEndpoint::class, 'handle_reindex' ],
        ];
    }

    /**
     * @dataProvider entrypointProvider
     */
    public function test_all_entrypoints_delegate_to_index_manager( string $class, string $method ): void {
        $source = $this->method_source( $class, $method );

        $this->assertMatchesRegularExpression(
            '/->\s*reindex_all\s*\(/',
            $source,
            sprintf( '%s::%s must delegate the full reindex to IndexManager::reindex_all().', $class, $method )
        );

        // The entrypoint must not reach around the owner: no direct client
        // writes, no atomic-swap internals, and no background scheduling.
        foreach ( [ 'reindex_atomic', 'saveObjects', 'operationIndex', 'setSettings', 'BackgroundIndexer' ] as $forbidden ) {
            $this->assertStringNotContainsString(
                $forbidden,
                $source,
                sprintf( '%s::%s must not reference %s directly — it belongs to IndexManager.', $class, $method, $forbidden )
            );
        }
    }

    /**
     * Read the exact source lines of a single method.
     */
    private function method_source( string $class, string $method ): string {
        $reflection = new \ReflectionMethod( $class, $method );
        $file       = $reflection->getFileName();
        $this->assertNotFalse( $file, "Unable to resolve source file for {$class}." );

        $lines = file( $file );
        $this->assertNotFalse( $lines, "Unable to read source file for {$class}." );

        $start = $reflection->getStartLine() - 1;
        $length = $reflection->getEndLine() - $reflection->getStartLine() + 1;

        return implode( '', array_slice( $lines, $start, $length ) );
    }
}
