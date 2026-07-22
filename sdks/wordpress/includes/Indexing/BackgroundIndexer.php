<?php
/**
 * Background reindexing via Action Scheduler (with WP-Cron fallback).
 *
 * Handles batched background reindexing for large sites, with progress
 * tracking via transients. Prefers Action Scheduler (bundled with
 * WooCommerce) and falls back to wp_schedule_single_event().
 *
 * @package Flapjack\WordPress\Indexing
 */

declare(strict_types=1);

namespace Flapjack\WordPress\Indexing;

use Flapjack\WordPress\ClientFactory;
use Flapjack\WordPress\Status\FailureReporter;

class BackgroundIndexer {

    public const PROGRESS_TRANSIENT = 'flapjack_reindex_progress';
    public const BATCH_HOOK         = 'flapjack_background_reindex_batch';
    public const GROUP              = 'flapjack-search';
    public const BATCH_SIZE         = 200;

    private IndexManager $index_manager;
    private ClientFactory $client_factory;

    public function __construct( ClientFactory $client_factory ) {
        // IndexManager owns all index writes (temp-index rebuild + atomic move);
        // BackgroundIndexer only schedules batches and tracks progress.
        $this->client_factory = $client_factory;
        $this->index_manager  = new IndexManager( $client_factory );
    }

    /**
     * Register WordPress hooks for batch processing.
     */
    public function register(): void {
        add_action( self::BATCH_HOOK, [ $this, 'process_batch' ], 10, 1 );
    }

    /**
     * Check if Action Scheduler is available.
     */
    public static function is_action_scheduler_available(): bool {
        return function_exists( 'as_schedule_single_action' )
            && function_exists( 'as_unschedule_all_actions' );
    }

    /**
     * Start a background reindex.
     *
     * Counts all indexable posts, stores initial progress, and schedules
     * the first batch.
     *
     * @return array{status: string, total_posts: int, method: string}
     */
    public function start_reindex(): array {
        // Don't start if already in progress.
        $existing = $this->get_progress();
        if ( $existing && 'in_progress' === ( $existing['status'] ?? '' ) ) {
            return $existing;
        }

        $post_types = (array) get_option( 'flapjack_post_types', [ 'post', 'page' ] );
        $total      = $this->count_indexable_posts( $post_types );
        $method     = self::is_action_scheduler_available() ? 'action_scheduler' : 'wp_cron';

        $progress = [
            'status'       => 'in_progress',
            'total_posts'  => $total,
            'processed'    => 0,
            'current_page' => 1,
            'total_pages'  => $total > 0 ? (int) ceil( $total / self::BATCH_SIZE ) : 0,
            'batches_done' => 0,
            'started_at'   => time(),
            'completed_at' => null,
            'error'        => null,
            'method'       => $method,
        ];

        set_transient( self::PROGRESS_TRANSIENT, $progress, HOUR_IN_SECONDS );

        if ( $total > 0 ) {
            // Initialize the canonical rebuild state (temp index) that every
            // batch appends to and the final batch publishes. IndexManager owns
            // this state; BackgroundIndexer only tracks scheduling and progress.
            $this->index_manager->begin_rebuild();
            $this->schedule_batch( 1, $method );
        } else {
            // No posts to index — mark complete immediately.
            $progress['status']       = 'complete';
            $progress['completed_at'] = time();
            set_transient( self::PROGRESS_TRANSIENT, $progress, HOUR_IN_SECONDS );
        }

        return $progress;
    }

    /**
     * Process a single batch of posts.
     *
     * @param int $page The batch page number (1-indexed).
     */
    public function process_batch( int $page ): void {
        $progress = $this->get_progress();
        if ( ! $progress || 'in_progress' !== ( $progress['status'] ?? '' ) ) {
            return;
        }

        try {
            // IndexManager owns all reindex enumeration (stable keyset seek) and
            // temp-index writes; BackgroundIndexer only drives one batch per
            // request and tracks progress.
            $result = $this->index_manager->index_rebuild_batch( self::BATCH_SIZE );

            // Update progress.
            $progress['processed']    += $result['processed'];
            $progress['current_page']  = $page;
            $progress['batches_done']++;

            if ( $result['done'] ) {
                // Final batch — atomically move the temp index over the live
                // index (settings applied to the temp index before the move).
                $this->index_manager->publish_rebuild();

                $progress['status']       = 'complete';
                $progress['completed_at'] = time();
                set_transient( self::PROGRESS_TRANSIENT, $progress, HOUR_IN_SECONDS );
            } else {
                set_transient( self::PROGRESS_TRANSIENT, $progress, HOUR_IN_SECONDS );
                $this->schedule_batch( $page + 1, $progress['method'] );
            }
        } catch ( \Throwable $e ) {
            // Persist the failure durably through the shared reporter, then reuse
            // its sanitized message for the operator-visible progress error so we
            // never expose a raw upstream message (potentially containing secrets
            // or unbounded text) in the progress transient. IndexManager still
            // owns rebuild state; a failed batch never publishes over the live
            // index (publish_rebuild self-aborts).
            FailureReporter::record( $e, [
                'operation'  => 'reindex_batch',
                'source'     => 'background_reindex',
                'index_name' => $this->client_factory->get_index_name(),
            ] );

            $progress['status'] = 'failed';
            $progress['error']  = FailureReporter::latest()['message'] ?? '';
            set_transient( self::PROGRESS_TRANSIENT, $progress, HOUR_IN_SECONDS );
        }
    }

    /**
     * Get the current reindex progress.
     *
     * @return array|null Progress data or null if no reindex in progress/recent.
     */
    public function get_progress(): ?array {
        $progress = get_transient( self::PROGRESS_TRANSIENT );
        return is_array( $progress ) ? $progress : null;
    }

    /**
     * Cancel an in-progress reindex.
     *
     * @return bool Whether a reindex was cancelled.
     */
    public function cancel_reindex(): bool {
        $progress = $this->get_progress();
        if ( ! $progress || 'in_progress' !== ( $progress['status'] ?? '' ) ) {
            return false;
        }

        // Unschedule pending batches.
        if ( self::is_action_scheduler_available() ) {
            as_unschedule_all_actions( self::BATCH_HOOK, null, self::GROUP );
        } else {
            wp_clear_scheduled_hook( self::BATCH_HOOK );
        }

        $progress['status'] = 'cancelled';
        set_transient( self::PROGRESS_TRANSIENT, $progress, HOUR_IN_SECONDS );

        return true;
    }

    /**
     * Schedule a batch for processing.
     *
     * @param int    $page   The page number to process.
     * @param string $method 'action_scheduler' or 'wp_cron'.
     */
    private function schedule_batch( int $page, string $method ): void {
        if ( 'action_scheduler' === $method && self::is_action_scheduler_available() ) {
            as_schedule_single_action( time(), self::BATCH_HOOK, [ $page ], self::GROUP );
        } else {
            wp_schedule_single_event( time(), self::BATCH_HOOK, [ $page ] );
        }
    }

    /**
     * Count total indexable posts.
     *
     * @param string[] $post_types Post types to count.
     * @return int Total number of published posts of the given types.
     */
    private function count_indexable_posts( array $post_types ): int {
        $total = 0;
        foreach ( $post_types as $post_type ) {
            $counts = wp_count_posts( $post_type );
            $total += (int) ( $counts->publish ?? 0 );
        }
        return $total;
    }
}
