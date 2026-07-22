<?php
/**
 * Tests for FailureReporter — the single durable, sanitized failure store.
 *
 * @package Flapjack\WordPress\Tests\Unit\Status
 */

declare(strict_types=1);

namespace Flapjack\WordPress\Tests\Unit\Status;

use PHPUnit\Framework\TestCase;
use Flapjack\WordPress\Status\FailureReporter;
use Flapjack\FlapjackSearch\Exceptions\NotFoundException;

class FailureReporterTest extends TestCase {

    protected function setUp(): void {
        wp_stubs_reset();
    }

    public function test_latest_is_null_before_any_failure(): void {
        $this->assertNull( FailureReporter::latest() );
    }

    public function test_record_writes_bounded_record_with_exact_fields(): void {
        $before = time();

        FailureReporter::record(
            new NotFoundException( 'Object 42 was not there', 404 ),
            [
                'operation'  => 'delete_post',
                'source'     => 'index_manager',
                'post_id'    => 42,
                'index_name' => 'wp_posts',
            ]
        );

        $record = FailureReporter::latest();
        $this->assertIsArray( $record );

        $this->assertSame( 'delete_post', $record['operation'] );
        $this->assertSame( 'index_manager', $record['source'] );
        $this->assertSame( 42, $record['post_id'] );
        $this->assertSame( 'wp_posts', $record['index_name'] );
        $this->assertSame( NotFoundException::class, $record['exception_class'] );
        $this->assertSame( 404, $record['status_code'] );
        $this->assertSame( 'Object 42 was not there', $record['message'] );

        $this->assertIsInt( $record['occurred_at'] );
        $this->assertGreaterThanOrEqual( $before, $record['occurred_at'] );
        $this->assertLessThanOrEqual( time(), $record['occurred_at'] );
    }

    public function test_record_omits_optional_fields_when_absent(): void {
        FailureReporter::record(
            new \RuntimeException( 'Connection refused' ),
            [ 'operation' => 'index_post', 'source' => 'post_sync' ]
        );

        $record = FailureReporter::latest();
        $this->assertArrayNotHasKey( 'post_id', $record );
        $this->assertArrayNotHasKey( 'index_name', $record );
        // RuntimeException code defaults to 0 — not an HTTP status.
        $this->assertArrayNotHasKey( 'status_code', $record );
    }

    public function test_record_redacts_key_value_secret(): void {
        FailureReporter::record(
            new \RuntimeException( 'Auth failed with api_key=SuperSecretKey123 at host' ),
            [ 'operation' => 'index_post', 'source' => 'post_sync' ]
        );

        $message = FailureReporter::latest()['message'];
        $this->assertStringNotContainsString( 'SuperSecretKey123', $message );
        $this->assertStringContainsString( '[redacted]', $message );
    }

    public function test_record_redacts_authorization_scheme_and_credential(): void {
        FailureReporter::record(
            new \RuntimeException( 'Authorization: Bearer ShortSecret123 rejected' ),
            [ 'operation' => 'index_post', 'source' => 'post_sync' ]
        );

        $message = FailureReporter::latest()['message'];
        $this->assertSame( 'Authorization: [redacted] rejected', $message );
        $this->assertStringNotContainsString( 'Bearer', $message );
        $this->assertStringNotContainsString( 'ShortSecret123', $message );
    }

    public function test_record_redacts_long_token(): void {
        $token = str_repeat( 'a1b2c3d4', 5 ); // 40-char alphanumeric token.

        FailureReporter::record(
            new \RuntimeException( "Rejected token {$token} by upstream" ),
            [ 'operation' => 'index_post', 'source' => 'post_sync' ]
        );

        $message = FailureReporter::latest()['message'];
        $this->assertStringNotContainsString( $token, $message );
        $this->assertStringContainsString( '[redacted]', $message );
    }

    public function test_record_strips_html_from_message(): void {
        FailureReporter::record(
            new \RuntimeException( '<script>alert(1)</script>Indexing broke' ),
            [ 'operation' => 'index_post', 'source' => 'post_sync' ]
        );

        $message = FailureReporter::latest()['message'];
        $this->assertStringNotContainsString( '<script>', $message );
        $this->assertStringNotContainsString( '</script>', $message );
        $this->assertStringContainsString( 'Indexing broke', $message );
    }

    public function test_record_truncates_long_message(): void {
        FailureReporter::record(
            new \RuntimeException( str_repeat( 'x', 5000 ) ),
            [ 'operation' => 'index_post', 'source' => 'post_sync' ]
        );

        $message = FailureReporter::latest()['message'];
        // 500 kept chars plus a single-character ellipsis.
        $this->assertLessThanOrEqual( 501, mb_strlen( $message ) );
    }

    public function test_repeated_failures_retain_only_the_latest(): void {
        FailureReporter::record(
            new \RuntimeException( 'first failure' ),
            [ 'operation' => 'index_post', 'source' => 'post_sync', 'post_id' => 1 ]
        );
        FailureReporter::record(
            new \RuntimeException( 'second failure' ),
            [ 'operation' => 'delete_post', 'source' => 'post_sync', 'post_id' => 2 ]
        );

        $record = FailureReporter::latest();
        $this->assertSame( 'second failure', $record['message'] );
        $this->assertSame( 'delete_post', $record['operation'] );
        $this->assertSame( 2, $record['post_id'] );
    }

    public function test_status_code_only_kept_for_http_range_codes(): void {
        // getCode() of 23 (e.g. a system errno) is not an HTTP status.
        FailureReporter::record(
            new \RuntimeException( 'errno failure', 23 ),
            [ 'operation' => 'index_post', 'source' => 'post_sync' ]
        );
        $this->assertArrayNotHasKey( 'status_code', FailureReporter::latest() );

        FailureReporter::record(
            new \RuntimeException( 'server error', 500 ),
            [ 'operation' => 'index_post', 'source' => 'post_sync' ]
        );
        $this->assertSame( 500, FailureReporter::latest()['status_code'] );
    }

    public function test_clear_removes_the_record(): void {
        FailureReporter::record(
            new \RuntimeException( 'boom' ),
            [ 'operation' => 'index_post', 'source' => 'post_sync' ]
        );
        $this->assertNotNull( FailureReporter::latest() );

        FailureReporter::clear();
        $this->assertNull( FailureReporter::latest() );
    }
}
