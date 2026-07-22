<?php
/**
 * WordPress hooks for real-time post sync to Flapjack index.
 *
 * @package Flapjack\WordPress\Indexing
 */

declare(strict_types=1);

namespace Flapjack\WordPress\Indexing;

use Flapjack\WordPress\ClientFactory;
use Flapjack\WordPress\Status\FailureReporter;

class PostSyncHooks {

    private IndexManager $index_manager;
    private ClientFactory $client_factory;

    public function __construct( IndexManager $index_manager, ?ClientFactory $client_factory = null ) {
        $this->index_manager  = $index_manager;
        $this->client_factory = $client_factory ?? new ClientFactory();
    }

    /**
     * Register hooks for post lifecycle events.
     */
    public function register(): void {
        // Post save/update — fires after a post is saved.
        add_action( 'save_post', [ $this, 'on_save_post' ], 10, 2 );

        // Post delete — fires before a post is deleted from the DB.
        add_action( 'before_delete_post', [ $this, 'on_delete_post' ] );

        // Post trashed.
        add_action( 'trashed_post', [ $this, 'on_trash_post' ] );

        // Post un-trashed / restored.
        add_action( 'untrashed_post', [ $this, 'on_untrash_post' ] );

        // Transition between statuses (publish → draft, etc.).
        add_action( 'transition_post_status', [ $this, 'on_status_transition' ], 10, 3 );
    }

    /**
     * Handle post save.
     *
     * @param int      $post_id
     * @param \WP_Post $post
     */
    public function on_save_post( int $post_id, \WP_Post $post ): void {
        // Skip autosaves.
        if ( defined( 'DOING_AUTOSAVE' ) && DOING_AUTOSAVE ) {
            return;
        }

        // Skip revisions.
        if ( wp_is_post_revision( $post_id ) ) {
            return;
        }

        $this->sync_post( $post );
    }

    /**
     * Handle post deletion.
     *
     * @param int $post_id
     */
    public function on_delete_post( int $post_id ): void {
        $this->safe_delete( $post_id );
    }

    /**
     * Handle post trashing.
     *
     * @param int $post_id
     */
    public function on_trash_post( int $post_id ): void {
        $this->safe_delete( $post_id );
    }

    /**
     * Handle post un-trashing.
     *
     * @param int $post_id
     */
    public function on_untrash_post( int $post_id ): void {
        $post = get_post( $post_id );
        if ( $post instanceof \WP_Post ) {
            $this->sync_post( $post );
        }
    }

    /**
     * Handle status transitions.
     *
     * @param string   $new_status
     * @param string   $old_status
     * @param \WP_Post $post
     */
    public function on_status_transition( string $new_status, string $old_status, \WP_Post $post ): void {
        // If transitioning away from publish, remove from index.
        if ( 'publish' === $old_status && 'publish' !== $new_status ) {
            $this->safe_delete( $post->ID );
            return;
        }

        // If transitioning to publish, index it.
        if ( 'publish' === $new_status && 'publish' !== $old_status ) {
            $this->sync_post( $post );
        }
    }

    /**
     * Sync a post to the index (index or delete based on eligibility).
     */
    private function sync_post( \WP_Post $post ): void {
        if ( ! $this->is_configured() ) {
            return;
        }

        try {
            $this->index_manager->index_post( $post );
        } catch ( \Throwable $e ) {
            // Persist the failure durably so it stays visible in status/admin
            // instead of being observable only through WP_DEBUG. The post save
            // flow must not break, so we swallow after recording.
            FailureReporter::record( $e, [
                'operation'  => 'index_post',
                'source'     => 'post_sync',
                'post_id'    => $post->ID,
                'index_name' => $this->client_factory->get_index_name(),
            ] );

            $this->log_latest_failure( sprintf( '[Flapjack Search] Failed to sync post %d: ', $post->ID ) );
        }
    }

    /**
     * Safely delete a post from the index.
     */
    private function safe_delete( int $post_id ): void {
        if ( ! $this->is_configured() ) {
            return;
        }

        try {
            $this->index_manager->delete_post( $post_id );
        } catch ( \Throwable $e ) {
            // Persist the failure durably so a delete that fails during a
            // WordPress delete/trash flow stays visible in status/admin. The
            // delete flow must not break, so we swallow after recording.
            FailureReporter::record( $e, [
                'operation'  => 'delete_post',
                'source'     => 'post_sync',
                'post_id'    => $post_id,
                'index_name' => $this->client_factory->get_index_name(),
            ] );

            $this->log_latest_failure( sprintf( '[Flapjack Search] Failed to delete post %d from index: ', $post_id ) );
        }
    }

    /**
     * Log the canonical sanitized failure message when WordPress debug logging
     * is enabled.
     */
    private function log_latest_failure( string $prefix ): void {
        if ( ! defined( 'WP_DEBUG' ) || ! WP_DEBUG ) {
            return;
        }

        $failure = FailureReporter::latest();
        $message = is_array( $failure ) && isset( $failure['message'] )
            ? (string) $failure['message']
            : 'Indexing failed.';

        error_log( $prefix . $message );
    }

    /**
     * Check if the plugin is configured.
     */
    private function is_configured(): bool {
        return $this->client_factory->is_configured();
    }
}
