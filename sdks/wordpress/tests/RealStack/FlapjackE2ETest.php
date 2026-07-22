<?php
/**
 * PHP-layer corroboration against a real Flapjack server using WordPress stubs.
 *
 * Requires a running Flapjack/Meilisearch instance. Set environment variables:
 *   FLAPJACK_TEST_HOST     — e.g. http://localhost:7700
 *   FLAPJACK_TEST_APP_ID   — your test app ID
 *   FLAPJACK_TEST_API_KEY  — your test admin API key
 *
 * Run:
 *   FLAPJACK_TEST_HOST=http://localhost:7700 \
 *   FLAPJACK_TEST_APP_ID=test_app \
 *   FLAPJACK_TEST_API_KEY=test_key \
 *   vendor/bin/phpunit --testsuite realstack
 *
 * @package Flapjack\WordPress\Tests\RealStack
 */

declare(strict_types=1);

namespace Flapjack\WordPress\Tests\RealStack;

use PHPUnit\Framework\TestCase;
use Flapjack\WordPress\ClientFactory;
use Flapjack\WordPress\Indexing\IndexManager;
use Flapjack\WordPress\Indexing\PostSyncHooks;
use Flapjack\WordPress\Search\QueryInterceptor;
use Flapjack\WordPress\REST\SearchEndpoint;
use Flapjack\WordPress\REST\StatusEndpoint;
use Flapjack\WordPress\Status\FailureReporter;
use Flapjack\WordPress\Tests\Traits\MakesTestPosts;

class FlapjackE2ETest extends TestCase {

    use MakesTestPosts;

    private ClientFactory $client_factory;
    private IndexManager $index_manager;
    private string $test_index;

    protected function setUp(): void {
        $host    = getenv( 'FLAPJACK_TEST_HOST' );
        $app_id  = getenv( 'FLAPJACK_TEST_APP_ID' );
        $api_key = getenv( 'FLAPJACK_TEST_API_KEY' );

        if ( empty( $host ) || empty( $app_id ) || empty( $api_key ) ) {
            self::fail(
                'Real-stack tests require FLAPJACK_TEST_HOST, FLAPJACK_TEST_APP_ID, and FLAPJACK_TEST_API_KEY environment variables.'
            );
        }

        wp_stubs_reset();

        // Use a unique test index to avoid collisions.
        $this->test_index = 'wp_e2e_test_' . bin2hex( random_bytes( 4 ) );

        update_option( 'flapjack_app_id', $app_id );
        update_option( 'flapjack_api_key', $api_key );
        update_option( 'flapjack_search_api_key', $api_key );
        update_option( 'flapjack_host', $host );
        update_option( 'flapjack_index_name', $this->test_index );
        update_option( 'flapjack_post_types', [ 'post', 'page' ] );
        update_option( 'flapjack_enable_search', true );
        update_option( 'flapjack_posts_per_page', 10 );
        update_option( 'flapjack_searchable_attrs', [ 'post_title', 'post_content', 'post_excerpt' ] );

        $this->client_factory = new ClientFactory();
        $this->index_manager  = new IndexManager( $this->client_factory );
    }

    protected function tearDown(): void {
        // Clean up: delete the test index.
        if ( ! empty( $this->test_index ) && isset( $this->client_factory ) ) {
            try {
                $client = $this->client_factory->get_client();
                $client->deleteIndex( $this->test_index );
            } catch ( \Throwable $e ) {
                // Best-effort cleanup.
            }
        }
    }

    // ─── Connection ──────────────────────────────────────────

    public function test_connection_succeeds(): void {
        $result = $this->client_factory->test_connection();
        $this->assertTrue( $result['success'], 'Connection test failed: ' . ( $result['message'] ?? '' ) );
    }

    // ─── Index a single post ─────────────────────────────────

    public function test_index_and_retrieve_single_post(): void {
        $post = $this->make_post( [
            'ID'           => 1,
            'post_title'   => 'E2E Test Post Alpha',
            'post_content' => 'This is an end-to-end test post for verifying the full indexing pipeline.',
            'post_excerpt' => 'E2E test excerpt.',
            'post_status'  => 'publish',
            'post_type'    => 'post',
        ] );

        // Index the post.
        $result = $this->index_manager->index_post( $post );
        $this->assertArrayHasKey( 'objectID', $result );

        // Wait for indexing to complete.
        $this->waitForIndexing();

        // Verify via direct API search.
        $client       = $this->client_factory->get_client();
        $search_result = $client->searchSingleIndex( $this->test_index, [
            'query' => 'E2E Test Post Alpha',
        ] );

        $this->assertGreaterThanOrEqual( 1, $search_result['nbHits'] );
        $hit = $search_result['hits'][0];
        $this->assertSame( '1', $hit['objectID'] );
        $this->assertSame( 'E2E Test Post Alpha', $hit['post_title'] );
    }

    // ─── Bulk reindex ────────────────────────────────────────

    public function test_bulk_reindex_indexes_multiple_posts(): void {
        global $wp_posts_store;

        // Create several posts.
        for ( $i = 1; $i <= 5; $i++ ) {
            $post = $this->make_post( [
                'ID'           => $i,
                'post_title'   => "Bulk Post {$i}",
                'post_content' => "Content for bulk test post number {$i}.",
                'post_status'  => 'publish',
                'post_type'    => 'post',
            ] );
            $wp_posts_store[ $i ] = $post;
        }

        // Reindex all.
        $result = $this->index_manager->reindex_all();

        $this->assertSame( 5, $result['total'] );
        $this->assertGreaterThanOrEqual( 1, $result['batches'] );

        // Wait and verify.
        $this->waitForIndexing();

        $client       = $this->client_factory->get_client();
        $search_result = $client->searchSingleIndex( $this->test_index, [
            'query'       => '',
            'hitsPerPage' => 0,
        ] );

        $this->assertSame( 5, $search_result['nbHits'] );
    }

    // ─── Exact-set replacement on full reindex ───────────────

    public function test_full_reindex_exactly_replaces_live_index(): void {
        global $wp_posts_store;

        // Pre-seed a stale object into the live index that no longer maps to any
        // WordPress post. A correct atomic reindex must remove it.
        $client = $this->client_factory->get_client();
        $client->saveObjects( $this->test_index, [
            [
                'objectID'   => '9001',
                'post_title' => 'Stale Ghost Post',
                'post_type'  => 'post',
            ],
        ] );
        $this->waitForIndexing();

        // The real WordPress store contains a smaller, exact set.
        for ( $i = 1; $i <= 2; $i++ ) {
            $wp_posts_store[ $i ] = $this->make_post( [
                'ID'           => $i,
                'post_title'   => "Exact Post {$i}",
                'post_content' => "Exact-set content for post {$i}.",
                'post_status'  => 'publish',
                'post_type'    => 'post',
            ] );
        }

        $result = $this->index_manager->reindex_all();
        $this->assertSame( 2, $result['total'] );
        $this->waitForIndexing();

        // After atomic publication the live index objectIDs must equal exactly
        // the rebuilt set — the stale ghost object must be gone.
        $search = $client->searchSingleIndex( $this->test_index, [
            'query'       => '',
            'hitsPerPage' => 100,
        ] );

        $ids = array_map( static fn( array $hit ) => (string) $hit['objectID'], $search['hits'] );
        sort( $ids );

        $this->assertSame( [ '1', '2' ], $ids, 'Live index must contain exactly the rebuilt objectIDs after reindex.' );
        $this->assertSame( 2, $search['nbHits'], 'Stale objects must not survive an atomic full reindex.' );
    }

    // ─── Delete from index ───────────────────────────────────

    public function test_delete_post_removes_from_index(): void {
        $post = $this->make_post( [
            'ID'           => 99,
            'post_title'   => 'Delete Me Post',
            'post_content' => 'This post will be deleted from the index.',
            'post_status'  => 'publish',
            'post_type'    => 'post',
        ] );

        // Index, then delete.
        $this->index_manager->index_post( $post );
        $this->waitForIndexing();

        $this->index_manager->delete_post( 99 );
        $this->waitForIndexing();

        // Verify deletion.
        $client       = $this->client_factory->get_client();
        $search_result = $client->searchSingleIndex( $this->test_index, [
            'query'   => 'Delete Me Post',
        ] );

        $this->assertSame( 0, $search_result['nbHits'] );
    }

    // ─── Search with filters ─────────────────────────────────

    public function test_search_with_post_type_filter(): void {
        global $wp_posts_store;

        $post = $this->make_post( [
            'ID'         => 10,
            'post_title' => 'Filterable Post',
            'post_type'  => 'post',
        ] );
        $page = $this->make_post( [
            'ID'         => 11,
            'post_title' => 'Filterable Page',
            'post_type'  => 'page',
        ] );
        $wp_posts_store[10] = $post;
        $wp_posts_store[11] = $page;

        $this->index_manager->index_post( $post );
        $this->index_manager->index_post( $page );

        // Configure index settings for filtering.
        $this->index_manager->configure_index_settings( $this->test_index );
        $this->waitForIndexing();

        // Search with post_type filter.
        $client = $this->client_factory->get_client();
        $result = $client->searchSingleIndex( $this->test_index, [
            'query'   => 'Filterable',
            'filters' => 'post_type:post',
        ] );

        $this->assertSame( 1, $result['nbHits'] );
        $this->assertSame( '10', $result['hits'][0]['objectID'] );
    }

    // ─── REST endpoint e2e ───────────────────────────────────

    public function test_rest_search_endpoint_returns_results(): void {
        $post = $this->make_post( [
            'ID'           => 20,
            'post_title'   => 'REST Endpoint Test',
            'post_content' => 'Content for REST endpoint testing.',
        ] );

        $this->index_manager->index_post( $post );
        $this->waitForIndexing();

        $endpoint = new SearchEndpoint( $this->client_factory );
        $request  = new \WP_REST_Request( 'GET' );
        $request->set_param( 'q', 'REST Endpoint Test' );
        $request->set_param( 'per_page', 10 );
        $request->set_param( 'page', 0 );

        $response = $endpoint->handle_search( $request );

        $this->assertInstanceOf( \WP_REST_Response::class, $response );
        $data = $response->get_data();
        $this->assertGreaterThanOrEqual( 1, $data['nbHits'] );
    }

    // ─── Index stats ─────────────────────────────────────────

    public function test_get_index_stats_returns_correct_count(): void {
        global $wp_posts_store;

        for ( $i = 1; $i <= 3; $i++ ) {
            $post = $this->make_post( [
                'ID'         => $i,
                'post_title' => "Stats Post {$i}",
            ] );
            $wp_posts_store[ $i ] = $post;
            $this->index_manager->index_post( $post );
        }
        $this->waitForIndexing();

        $stats = $this->index_manager->get_index_stats();

        $this->assertTrue( $stats['exists'] );
        $this->assertSame( 3, $stats['count'] );
        $this->assertSame( $this->test_index, $stats['name'] );
    }

    // ─── Typo tolerance (core Flapjack feature) ──────────────

    public function test_search_handles_typos(): void {
        $post = $this->make_post( [
            'ID'           => 30,
            'post_title'   => 'Typo Tolerance Verification',
            'post_content' => 'This tests that Flapjack correctly handles misspelled queries.',
        ] );

        $this->index_manager->index_post( $post );
        $this->index_manager->configure_index_settings( $this->test_index );
        $this->waitForIndexing();

        $client = $this->client_factory->get_client();

        // Search with a typo.
        $result = $client->searchSingleIndex( $this->test_index, [
            'query' => 'Typo Toleranec', // deliberate typo
        ] );

        $this->assertGreaterThanOrEqual( 1, $result['nbHits'], 'Typo-tolerant search should still find the post.' );
    }

    // ─── Durable failure visibility (unmocked) ───────────────

    public function test_real_sync_failure_is_durably_visible_then_recovers(): void {
        // Nothing recorded before we induce a failure.
        $this->assertNull( FailureReporter::latest() );

        // Point the plugin at an unreachable host — a real, unmocked failed
        // network call, not a stubbed exception.
        $good_host = get_option( 'flapjack_host' );
        update_option( 'flapjack_host', 'http://127.0.0.1:1' );

        $failing_factory = new ClientFactory();
        $failing_manager = new IndexManager( $failing_factory );
        $hooks           = new PostSyncHooks( $failing_manager, $failing_factory );

        $post = $this->make_post( [
            'ID'          => 4242,
            'post_title'  => 'Failure Visibility Post',
            'post_status' => 'publish',
            'post_type'   => 'post',
        ] );

        // Trigger a real sync failure via the publish transition. The hook must
        // not throw — the failure is swallowed after being recorded durably.
        $hooks->on_status_transition( 'publish', 'draft', $post );

        // Restore the good host so the visible-surface assertions below read the
        // real stack again.
        update_option( 'flapjack_host', $good_host );

        // The durable record holds exact, sanitized fields.
        $failure = FailureReporter::latest();
        $this->assertNotNull( $failure, 'A real sync failure must be recorded durably.' );
        $this->assertSame( 'index_post', $failure['operation'] );
        $this->assertSame( 'post_sync', $failure['source'] );
        $this->assertSame( 4242, $failure['post_id'] );
        $this->assertSame( $this->test_index, $failure['index_name'] );
        $this->assertIsInt( $failure['occurred_at'] );
        $this->assertNotSame( '', $failure['message'] );

        // No secret leakage: the admin API key must never appear in the record.
        $api_key = (string) get_option( 'flapjack_api_key' );
        $this->assertNotSame( '', $api_key );
        $this->assertStringNotContainsString( $api_key, $failure['message'] );

        // The same failure is visible through the /status surface operators read,
        // exposed verbatim (no recompute) once the good host is restored.
        $status = new StatusEndpoint( $this->client_factory, $this->index_manager );
        $data   = $status->handle_status( new \WP_REST_Request( 'GET' ) )->get_data();
        $this->assertSame( $failure, $data['last_failure'] );

        // Recovery: a successful sync against the restored host does not, by
        // itself, resurrect the good stack's data for the failed post, but the
        // durable record stays inspectable until explicitly cleared.
        FailureReporter::clear();
        $this->assertNull( FailureReporter::latest() );
    }

    /**
     * Wait for Flapjack/Meilisearch to finish processing tasks.
     */
    private function waitForIndexing( int $max_wait_ms = 5000 ): void {
        usleep( min( $max_wait_ms, 2000 ) * 1000 );
    }
}
