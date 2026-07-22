<?php
/**
 * Tests for PostSyncHooks.
 *
 * @package Flapjack\WordPress\Tests\Unit\Indexing
 */

declare(strict_types=1);

namespace Flapjack\WordPress\Indexing {
    function error_log( string $message ): bool {
        $GLOBALS['flapjack_error_log_messages'][] = $message;
        return true;
    }
}

namespace Flapjack\WordPress\Tests\Unit\Indexing {

use PHPUnit\Framework\TestCase;
use PHPUnit\Framework\MockObject\MockObject;
use Flapjack\WordPress\Indexing\IndexManager;
use Flapjack\WordPress\Indexing\PostSyncHooks;
use Flapjack\WordPress\Status\FailureReporter;
use Flapjack\WordPress\Tests\Traits\MakesTestPosts;

class PostSyncHooksTest extends TestCase {

    use MakesTestPosts;

    private IndexManager&MockObject $index_manager;
    private PostSyncHooks $post_sync;

    protected function setUp(): void {
        wp_stubs_reset();
        $GLOBALS['flapjack_error_log_messages'] = [];

        // Plugin must be configured for hooks to fire.
        update_option( 'flapjack_app_id', 'test-id' );
        update_option( 'flapjack_api_key', 'test-key' );

        $this->index_manager = $this->createMock( IndexManager::class );
        $this->post_sync     = new PostSyncHooks( $this->index_manager );
    }

    public function test_register_adds_hooks(): void {
        global $wp_actions;
        $this->post_sync->register();

        $hook_names = array_keys( $wp_actions );
        $this->assertContains( 'save_post', $hook_names );
        $this->assertContains( 'before_delete_post', $hook_names );
        $this->assertContains( 'trashed_post', $hook_names );
        $this->assertContains( 'untrashed_post', $hook_names );
        $this->assertContains( 'transition_post_status', $hook_names );
    }

    public function test_on_save_post_indexes_published_post(): void {
        $post = $this->make_post( [ 'ID' => 10, 'post_status' => 'publish' ] );

        $this->index_manager
            ->expects( $this->once() )
            ->method( 'index_post' )
            ->with( $post );

        $this->post_sync->on_save_post( 10, $post );
    }

    public function test_on_save_post_skips_during_autosave(): void {
        define( 'DOING_AUTOSAVE', true );
        $post = $this->make_post();

        $this->index_manager
            ->expects( $this->never() )
            ->method( 'index_post' );

        $this->post_sync->on_save_post( 1, $post );
    }

    public function test_on_delete_post_removes_from_index(): void {
        $this->index_manager
            ->expects( $this->once() )
            ->method( 'delete_post' )
            ->with( 42 );

        $this->post_sync->on_delete_post( 42 );
    }

    public function test_on_trash_post_removes_from_index(): void {
        $this->index_manager
            ->expects( $this->once() )
            ->method( 'delete_post' )
            ->with( 42 );

        $this->post_sync->on_trash_post( 42 );
    }

    public function test_on_untrash_post_reindexes(): void {
        global $wp_posts_store;
        $post = $this->make_post( [ 'ID' => 42, 'post_status' => 'publish' ] );
        $wp_posts_store[42] = $post;

        $this->index_manager
            ->expects( $this->once() )
            ->method( 'index_post' )
            ->with( $post );

        $this->post_sync->on_untrash_post( 42 );
    }

    public function test_status_transition_from_publish_to_draft_deletes(): void {
        $post = $this->make_post( [ 'ID' => 42 ] );

        $this->index_manager
            ->expects( $this->once() )
            ->method( 'delete_post' )
            ->with( 42 );

        $this->post_sync->on_status_transition( 'draft', 'publish', $post );
    }

    public function test_status_transition_from_draft_to_publish_indexes(): void {
        $post = $this->make_post( [ 'ID' => 42, 'post_status' => 'publish' ] );

        $this->index_manager
            ->expects( $this->once() )
            ->method( 'index_post' )
            ->with( $post );

        $this->post_sync->on_status_transition( 'publish', 'draft', $post );
    }

    public function test_status_transition_publish_to_publish_does_nothing(): void {
        $post = $this->make_post();

        // Neither delete nor index should be called for publish->publish
        // (save_post handles this case).
        $this->index_manager->expects( $this->never() )->method( 'delete_post' );
        $this->index_manager->expects( $this->never() )->method( 'index_post' );

        $this->post_sync->on_status_transition( 'publish', 'publish', $post );
    }

    public function test_hooks_do_nothing_when_not_configured(): void {
        // Clear credentials.
        delete_option( 'flapjack_app_id' );
        delete_option( 'flapjack_api_key' );

        $post = $this->make_post( [ 'ID' => 42, 'post_status' => 'publish' ] );

        $this->index_manager->expects( $this->never() )->method( 'index_post' );
        $this->index_manager->expects( $this->never() )->method( 'delete_post' );

        $this->post_sync->on_save_post( 42, $post );
        $this->post_sync->on_delete_post( 42 );
        $this->post_sync->on_trash_post( 42 );
    }

    public function test_save_post_catches_exceptions_gracefully(): void {
        $post = $this->make_post( [ 'ID' => 42, 'post_status' => 'publish' ] );

        $this->index_manager
            ->method( 'index_post' )
            ->willThrowException( new \RuntimeException( 'API error' ) );

        // Should not throw — errors are logged, not propagated.
        $this->post_sync->on_save_post( 42, $post );
        $this->assertTrue( true ); // If we get here, no exception was thrown.
    }

    public function test_delete_catches_exceptions_gracefully(): void {
        $this->index_manager
            ->method( 'delete_post' )
            ->willThrowException( new \RuntimeException( 'API error' ) );

        $this->post_sync->on_delete_post( 42 );
        $this->assertTrue( true );
    }

    public function test_save_post_persists_exact_failure_metadata_when_index_throws(): void {
        $post = $this->make_post( [ 'ID' => 42, 'post_status' => 'publish' ] );

        $this->index_manager
            ->method( 'index_post' )
            ->willThrowException( new \RuntimeException( 'Upstream 500 error' ) );

        // Exercise the index path via a publish transition rather than
        // on_save_post — an earlier test defines DOING_AUTOSAVE process-wide,
        // which would short-circuit on_save_post.
        $this->post_sync->on_status_transition( 'publish', 'draft', $post );

        $failure = FailureReporter::latest();
        $this->assertNotNull( $failure );
        $this->assertSame( 'index_post', $failure['operation'] );
        $this->assertSame( 'post_sync', $failure['source'] );
        $this->assertSame( 42, $failure['post_id'] );
        $this->assertSame( 'wp_posts', $failure['index_name'] );
        $this->assertSame( 'Upstream 500 error', $failure['message'] );
        $this->assertIsInt( $failure['occurred_at'] );
    }

    public function test_delete_persists_exact_failure_metadata_when_delete_throws(): void {
        $this->index_manager
            ->method( 'delete_post' )
            ->willThrowException( new \RuntimeException( 'Connection refused' ) );

        $this->post_sync->on_delete_post( 99 );

        $failure = FailureReporter::latest();
        $this->assertNotNull( $failure );
        $this->assertSame( 'delete_post', $failure['operation'] );
        $this->assertSame( 'post_sync', $failure['source'] );
        $this->assertSame( 99, $failure['post_id'] );
        $this->assertSame( 'wp_posts', $failure['index_name'] );
        $this->assertSame( 'Connection refused', $failure['message'] );
    }

    public function test_debug_log_reuses_sanitized_failure_message(): void {
        if ( ! defined( 'WP_DEBUG' ) ) {
            define( 'WP_DEBUG', true );
        }

        $post = $this->make_post( [ 'ID' => 42, 'post_status' => 'publish' ] );
        $this->index_manager
            ->method( 'index_post' )
            ->willThrowException( new \RuntimeException( 'Authorization: Bearer ShortSecret123 rejected' ) );

        $this->post_sync->on_status_transition( 'publish', 'draft', $post );

        $this->assertSame(
            [ '[Flapjack Search] Failed to sync post 42: Authorization: [redacted] rejected' ],
            $GLOBALS['flapjack_error_log_messages']
        );
    }

    public function test_delete_debug_log_reuses_sanitized_failure_message(): void {
        if ( ! defined( 'WP_DEBUG' ) ) {
            define( 'WP_DEBUG', true );
        }

        $this->index_manager
            ->method( 'delete_post' )
            ->willThrowException( new \RuntimeException( 'Authorization: Bearer DeleteSecret456 rejected' ) );

        $this->post_sync->on_delete_post( 99 );

        $this->assertSame(
            [ '[Flapjack Search] Failed to delete post 99 from index: Authorization: [redacted] rejected' ],
            $GLOBALS['flapjack_error_log_messages']
        );
    }

    public function test_no_failure_recorded_when_sync_succeeds(): void {
        $post = $this->make_post( [ 'ID' => 7, 'post_status' => 'publish' ] );

        $this->index_manager->method( 'index_post' )->willReturn( [ 'objectID' => '7' ] );

        $this->post_sync->on_status_transition( 'publish', 'draft', $post );

        $this->assertNull( FailureReporter::latest() );
    }

}
}
