<?php
/**
 * Tests for IndexManager.
 *
 * @package Flapjack\WordPress\Tests\Unit\Indexing
 */

declare(strict_types=1);

namespace Flapjack\WordPress\Tests\Unit\Indexing;

use PHPUnit\Framework\TestCase;
use PHPUnit\Framework\MockObject\MockObject;
use Flapjack\WordPress\ClientFactory;
use Flapjack\WordPress\Indexing\IndexManager;
use Flapjack\WordPress\Tests\Traits\MakesTestPosts;
use Flapjack\WordPress\Status\FailureReporter;
use Flapjack\FlapjackSearch\Api\SearchClient;
use Flapjack\FlapjackSearch\Exceptions\NotFoundException;

class IndexManagerTest extends TestCase {

    use MakesTestPosts;

    private ClientFactory&MockObject $client_factory;
    private SearchClient&MockObject $search_client;
    private IndexManager $index_manager;

    protected function setUp(): void {
        wp_stubs_reset();

        $this->search_client  = $this->createMock( SearchClient::class );
        $this->client_factory = $this->createMock( ClientFactory::class );

        $this->client_factory->method( 'get_client' )->willReturn( $this->search_client );
        $this->client_factory->method( 'get_index_name' )->willReturn( 'wp_posts' );

        $this->index_manager = new IndexManager( $this->client_factory );

        // Set default options.
        update_option( 'flapjack_post_types', [ 'post', 'page' ] );
        update_option( 'flapjack_searchable_attrs', [ 'post_title', 'post_content', 'post_excerpt' ] );
    }

    // ─── should_index_post ────────────────────────────────────

    public function test_should_index_published_post(): void {
        $post = $this->make_post( [ 'post_status' => 'publish', 'post_type' => 'post' ] );
        $this->assertTrue( $this->index_manager->should_index_post( $post ) );
    }

    public function test_should_not_index_draft_post(): void {
        $post = $this->make_post( [ 'post_status' => 'draft' ] );
        $this->assertFalse( $this->index_manager->should_index_post( $post ) );
    }

    public function test_should_not_index_password_protected_post(): void {
        $post = $this->make_post( [ 'post_status' => 'publish', 'post_password' => 'secret' ] );
        $this->assertFalse( $this->index_manager->should_index_post( $post ) );
    }

    public function test_should_not_index_unconfigured_post_type(): void {
        $post = $this->make_post( [ 'post_type' => 'custom_cpt', 'post_status' => 'publish' ] );
        $this->assertFalse( $this->index_manager->should_index_post( $post ) );
    }

    public function test_should_index_page(): void {
        $post = $this->make_post( [ 'post_type' => 'page', 'post_status' => 'publish' ] );
        $this->assertTrue( $this->index_manager->should_index_post( $post ) );
    }

    public function test_should_index_respects_filter(): void {
        $post = $this->make_post( [ 'post_status' => 'publish', 'post_type' => 'post' ] );

        // Add a filter that blocks indexing.
        add_filter( 'flapjack_should_index_post', function () {
            return false;
        } );

        $this->assertFalse( $this->index_manager->should_index_post( $post ) );
    }

    // ─── build_record ─────────────────────────────────────────

    public function test_build_record_contains_required_fields(): void {
        $post   = $this->make_post( [
            'ID'           => 42,
            'post_title'   => 'Test Post',
            'post_content' => 'Hello world content',
            'post_excerpt' => 'A short excerpt',
            'post_type'    => 'post',
            'post_status'  => 'publish',
        ] );
        $record = $this->index_manager->build_record( $post );

        $this->assertSame( '42', $record['objectID'] );
        $this->assertSame( 42, $record['post_id'] );
        $this->assertSame( 'Test Post', $record['post_title'] );
        $this->assertSame( 'A short excerpt', $record['post_excerpt'] );
        $this->assertSame( 'Hello world content', $record['post_content'] );
        $this->assertSame( 'post', $record['post_type'] );
        $this->assertSame( 'publish', $record['post_status'] );
        $this->assertArrayHasKey( 'permalink', $record );
        $this->assertArrayHasKey( 'author', $record );
        $this->assertArrayHasKey( 'post_date', $record );
        $this->assertArrayHasKey( 'post_modified', $record );
    }

    public function test_build_record_strips_html_from_content(): void {
        $post   = $this->make_post( [
            'post_content' => '<p>Hello <strong>world</strong></p><script>alert("xss")</script>',
        ] );
        $record = $this->index_manager->build_record( $post );

        $this->assertStringNotContainsString( '<p>', $record['post_content'] );
        $this->assertStringNotContainsString( '<script>', $record['post_content'] );
        $this->assertStringContainsString( 'Hello', $record['post_content'] );
        $this->assertStringContainsString( 'world', $record['post_content'] );
    }

    public function test_build_record_strips_shortcodes(): void {
        $post   = $this->make_post( [
            'post_content' => 'Before [gallery ids="1,2,3"] After',
        ] );
        $record = $this->index_manager->build_record( $post );

        $this->assertStringNotContainsString( '[gallery', $record['post_content'] );
        $this->assertStringContainsString( 'Before', $record['post_content'] );
        $this->assertStringContainsString( 'After', $record['post_content'] );
    }

    public function test_build_record_truncates_long_content(): void {
        $post   = $this->make_post( [
            'post_content' => str_repeat( 'a', 15000 ),
        ] );
        $record = $this->index_manager->build_record( $post );

        $this->assertLessThanOrEqual( 10000, mb_strlen( $record['post_content'] ) );
    }

    public function test_build_record_generates_excerpt_from_content(): void {
        $content = str_repeat( 'word ', 100 );
        $post    = $this->make_post( [
            'post_excerpt' => '',
            'post_content' => $content,
        ] );
        $record  = $this->index_manager->build_record( $post );

        $this->assertNotEmpty( $record['post_excerpt'] );
        $this->assertLessThanOrEqual( 303, mb_strlen( $record['post_excerpt'] ) ); // 300 + "..."
    }

    public function test_build_record_uses_existing_excerpt(): void {
        $post   = $this->make_post( [
            'post_excerpt' => 'My custom excerpt',
            'post_content' => 'Full content here',
        ] );
        $record = $this->index_manager->build_record( $post );

        $this->assertSame( 'My custom excerpt', $record['post_excerpt'] );
    }

    public function test_build_record_includes_author(): void {
        $post   = $this->make_post( [ 'post_author' => '5' ] );
        $record = $this->index_manager->build_record( $post );

        $this->assertIsArray( $record['author'] );
        $this->assertSame( 5, $record['author']['id'] );
        $this->assertSame( 'Test Author', $record['author']['name'] );
    }

    public function test_build_record_includes_menu_order(): void {
        $post   = $this->make_post( [ 'menu_order' => 5 ] );
        $record = $this->index_manager->build_record( $post );

        $this->assertSame( 5, $record['menu_order'] );
    }

    public function test_build_record_includes_comment_count(): void {
        $post   = $this->make_post( [ 'comment_count' => 10 ] );
        $record = $this->index_manager->build_record( $post );

        $this->assertSame( 10, $record['comment_count'] );
    }

    public function test_build_record_respects_filter(): void {
        $post = $this->make_post( [ 'ID' => 99, 'post_title' => 'Original' ] );

        add_filter( 'flapjack_post_record', function ( array $record, \WP_Post $post ) {
            $record['custom_field'] = 'custom_value';
            return $record;
        }, 10, 2 );

        $record = $this->index_manager->build_record( $post );
        $this->assertSame( 'custom_value', $record['custom_field'] );
    }

    public function test_build_record_includes_post_type_label(): void {
        $post   = $this->make_post( [ 'post_type' => 'post' ] );
        $record = $this->index_manager->build_record( $post );

        $this->assertSame( 'Post', $record['post_type_label'] );
    }

    // ─── index_post ───────────────────────────────────────────

    public function test_index_post_calls_add_or_update_object_with_explicit_id(): void {
        $post = $this->make_post( [ 'ID' => 42, 'post_status' => 'publish', 'post_type' => 'post' ] );

        $this->search_client
            ->expects( $this->once() )
            ->method( 'addOrUpdateObject' )
            ->with(
                'wp_posts',
                '42',
                $this->callback( fn( $record ) => $record['objectID'] === '42' )
            )
            ->willReturn( [ 'objectID' => '42', 'taskID' => 1 ] );

        $result = $this->index_manager->index_post( $post );
        $this->assertArrayHasKey( 'taskID', $result );
    }

    public function test_index_post_accepts_post_id(): void {
        global $wp_posts_store;
        $post = $this->make_post( [ 'ID' => 55, 'post_status' => 'publish', 'post_type' => 'post' ] );
        $wp_posts_store[55] = $post;

        $this->search_client
            ->expects( $this->once() )
            ->method( 'addOrUpdateObject' )
            ->with( 'wp_posts', '55', $this->isType( 'array' ) )
            ->willReturn( [ 'objectID' => '55', 'taskID' => 1 ] );

        $this->index_manager->index_post( 55 );
    }

    public function test_index_post_throws_for_invalid_post(): void {
        $this->expectException( \InvalidArgumentException::class );
        $this->index_manager->index_post( 999999 );
    }

    public function test_index_post_deletes_ineligible_post(): void {
        $post = $this->make_post( [ 'ID' => 42, 'post_status' => 'draft', 'post_type' => 'post' ] );

        $this->search_client
            ->expects( $this->once() )
            ->method( 'deleteObject' )
            ->with( 'wp_posts', '42' )
            ->willReturn( [ 'taskID' => 1 ] );

        $this->index_manager->index_post( $post );
    }

    // ─── delete_post ──────────────────────────────────────────

    public function test_delete_post_calls_delete_object(): void {
        $this->search_client
            ->expects( $this->once() )
            ->method( 'deleteObject' )
            ->with( 'wp_posts', '42' )
            ->willReturn( [ 'taskID' => 1 ] );

        $result = $this->index_manager->delete_post( 42 );
        $this->assertArrayHasKey( 'taskID', $result );
    }

    public function test_delete_post_treats_typed_not_found_as_idempotent_without_recording(): void {
        $this->search_client
            ->expects( $this->once() )
            ->method( 'deleteObject' )
            ->willThrowException( new NotFoundException( 'ObjectID does not exist', 404 ) );

        $result = $this->index_manager->delete_post( 999 );

        $this->assertTrue( $result['deleted'] );
        // A typed not-found is not a failure — nothing should be recorded.
        $this->assertNull( FailureReporter::latest() );
    }

    public function test_delete_post_records_and_rethrows_real_delete_failures(): void {
        $this->search_client
            ->expects( $this->once() )
            ->method( 'deleteObject' )
            ->willThrowException( new \RuntimeException( 'Connection refused' ) );

        try {
            $this->index_manager->delete_post( 42 );
            $this->fail( 'delete_post() must rethrow non-not-found failures.' );
        } catch ( \RuntimeException $e ) {
            $this->assertSame( 'Connection refused', $e->getMessage() );
        }

        $failure = FailureReporter::latest();
        $this->assertNotNull( $failure );
        $this->assertSame( 'delete_post', $failure['operation'] );
        $this->assertSame( 'index_manager', $failure['source'] );
        $this->assertSame( 42, $failure['post_id'] );
        $this->assertSame( 'wp_posts', $failure['index_name'] );
        $this->assertSame( 'Connection refused', $failure['message'] );
    }

    public function test_delete_post_does_not_treat_message_404_as_not_found(): void {
        // A generic exception whose message merely mentions 404 must be treated
        // as a real failure now — the old substring match would have swallowed it.
        $this->search_client
            ->expects( $this->once() )
            ->method( 'deleteObject' )
            ->willThrowException( new \RuntimeException( 'Upstream returned 404 for unrelated reasons' ) );

        $this->expectException( \RuntimeException::class );
        $this->index_manager->delete_post( 7 );
    }

    // ─── get_index_stats ──────────────────────────────────────

    public function test_get_index_stats_returns_stats_on_success(): void {
        $this->search_client
            ->method( 'getSettings' )
            ->willReturn( [] );

        $this->search_client
            ->method( 'searchSingleIndex' )
            ->willReturn( [ 'nbHits' => 150 ] );

        $stats = $this->index_manager->get_index_stats();

        $this->assertTrue( $stats['exists'] );
        $this->assertSame( 150, $stats['count'] );
        $this->assertSame( 'wp_posts', $stats['name'] );
    }

    public function test_get_index_stats_returns_defaults_on_failure(): void {
        $this->search_client
            ->method( 'getSettings' )
            ->willThrowException( new \RuntimeException( 'Index not found' ) );

        $stats = $this->index_manager->get_index_stats();

        $this->assertFalse( $stats['exists'] );
        $this->assertSame( 0, $stats['count'] );
    }

    // ─── configure_index_settings ─────────────────────────────

    public function test_configure_index_settings_sends_correct_settings(): void {
        $this->search_client
            ->expects( $this->once() )
            ->method( 'setSettings' )
            ->with(
                'wp_posts',
                $this->callback( function ( array $settings ) {
                    return isset( $settings['searchableAttributes'] )
                        && in_array( 'post_title', $settings['searchableAttributes'], true )
                        && in_array( 'post_content', $settings['searchableAttributes'], true )
                        && isset( $settings['attributesForFaceting'] )
                        && isset( $settings['customRanking'] );
                } )
            );

        $this->index_manager->configure_index_settings( 'wp_posts' );
    }

    public function test_configure_index_settings_respects_searchable_attrs_option(): void {
        update_option( 'flapjack_searchable_attrs', [ 'post_title' ] );

        $this->search_client
            ->expects( $this->once() )
            ->method( 'setSettings' )
            ->with(
                'wp_posts',
                $this->callback( function ( array $settings ) {
                    return $settings['searchableAttributes'] === [ 'post_title' ];
                } )
            );

        $this->index_manager->configure_index_settings( 'wp_posts' );
    }

    public function test_configure_index_settings_includes_author_when_selected(): void {
        update_option( 'flapjack_searchable_attrs', [ 'post_title', 'author' ] );

        $this->search_client
            ->expects( $this->once() )
            ->method( 'setSettings' )
            ->with(
                'wp_posts',
                $this->callback( function ( array $settings ) {
                    return in_array( 'author.name', $settings['searchableAttributes'], true );
                } )
            );

        $this->index_manager->configure_index_settings( 'wp_posts' );
    }

    public function test_configure_index_settings_respects_filter(): void {
        add_filter( 'flapjack_index_settings', function ( array $settings ) {
            $settings['customRanking'] = [ 'asc(menu_order)' ];
            return $settings;
        } );

        $this->search_client
            ->expects( $this->once() )
            ->method( 'setSettings' )
            ->with(
                'wp_posts',
                $this->callback( function ( array $settings ) {
                    return $settings['customRanking'] === [ 'asc(menu_order)' ];
                } )
            );

        $this->index_manager->configure_index_settings( 'wp_posts' );
    }

    // ─── reindex_all (atomic temp-index + move) ──────────────

    private const TEMP_INDEX_PATTERN = '/^wp_posts_tmp_\d+$/';

    public function test_reindex_all_builds_temp_index_and_moves_to_live(): void {
        global $wp_posts_store;

        // Create 3 published posts.
        for ( $i = 1; $i <= 3; $i++ ) {
            $wp_posts_store[ $i ] = $this->make_post( [
                'ID'          => $i,
                'post_title'  => "Post {$i}",
                'post_status' => 'publish',
                'post_type'   => 'post',
            ] );
        }

        // Records are saved only to a temp index — never a direct live write.
        $this->search_client
            ->expects( $this->once() )
            ->method( 'saveObjects' )
            ->with(
                $this->matchesRegularExpression( self::TEMP_INDEX_PATTERN ),
                $this->callback( function ( array $records ) {
                    return count( $records ) === 3
                        && $records[0]['objectID'] === '1'
                        && $records[2]['objectID'] === '3';
                } )
            )
            ->willReturn( [ 'objectIDs' => [ '1', '2', '3' ] ] );

        // Settings applied only to the temp index.
        $this->search_client
            ->expects( $this->once() )
            ->method( 'setSettings' )
            ->with( $this->matchesRegularExpression( self::TEMP_INDEX_PATTERN ), $this->isType( 'array' ) )
            ->willReturn( [ 'taskID' => 1 ] );

        // Publication is an atomic move of the temp index over the live index.
        $this->search_client
            ->expects( $this->once() )
            ->method( 'operationIndex' )
            ->with(
                $this->matchesRegularExpression( self::TEMP_INDEX_PATTERN ),
                $this->callback( function ( array $params ) {
                    return $params['operation'] === 'move'
                        && $params['destination'] === 'wp_posts';
                } )
            )
            ->willReturn( [ 'taskID' => 2 ] );

        $result = $this->index_manager->reindex_all();

        $this->assertSame( 3, $result['total'] );
        $this->assertSame( 1, $result['batches'] );
    }

    public function test_reindex_all_never_writes_directly_to_live_index(): void {
        global $wp_posts_store;

        $wp_posts_store[1] = $this->make_post( [
            'ID' => 1, 'post_status' => 'publish', 'post_type' => 'post',
        ] );

        // The with() index constraint fails the test if 'wp_posts' (live) is
        // ever passed to saveObjects.
        $this->search_client
            ->expects( $this->once() )
            ->method( 'saveObjects' )
            ->with( $this->matchesRegularExpression( self::TEMP_INDEX_PATTERN ), $this->isType( 'array' ) )
            ->willReturn( [ 'objectIDs' => [ '1' ] ] );

        $this->search_client->method( 'setSettings' )->willReturn( [ 'taskID' => 1 ] );
        $this->search_client->method( 'operationIndex' )->willReturn( [ 'taskID' => 2 ] );

        $this->index_manager->reindex_all();
    }

    public function test_reindex_all_returns_zero_for_no_posts(): void {
        // With no posts the temp index is still created (settings) and moved,
        // making replacement exact — an empty store yields an empty live index.
        $this->search_client
            ->expects( $this->never() )
            ->method( 'saveObjects' );

        $this->search_client
            ->expects( $this->once() )
            ->method( 'setSettings' )
            ->with( $this->matchesRegularExpression( self::TEMP_INDEX_PATTERN ), $this->isType( 'array' ) )
            ->willReturn( [ 'taskID' => 1 ] );

        $this->search_client
            ->expects( $this->once() )
            ->method( 'operationIndex' )
            ->willReturn( [ 'taskID' => 2 ] );

        $result = $this->index_manager->reindex_all();

        $this->assertSame( 0, $result['total'] );
        $this->assertSame( 0, $result['batches'] );
    }

    public function test_reindex_all_uses_configured_post_types(): void {
        global $wp_posts_store;

        update_option( 'flapjack_post_types', [ 'page' ] );

        // Create a post (should be excluded) and a page (should be included).
        $wp_posts_store[1] = $this->make_post( [
            'ID' => 1, 'post_status' => 'publish', 'post_type' => 'post',
        ] );
        $wp_posts_store[2] = $this->make_post( [
            'ID' => 2, 'post_status' => 'publish', 'post_type' => 'page',
        ] );

        $this->search_client
            ->expects( $this->once() )
            ->method( 'saveObjects' )
            ->with(
                $this->matchesRegularExpression( self::TEMP_INDEX_PATTERN ),
                $this->callback( function ( array $records ) {
                    // Only the page should be indexed.
                    return count( $records ) === 1 && $records[0]['post_type'] === 'page';
                } )
            )
            ->willReturn( [ 'objectIDs' => [ '2' ] ] );

        $this->search_client->method( 'setSettings' )->willReturn( [ 'taskID' => 1 ] );
        $this->search_client->method( 'operationIndex' )->willReturn( [ 'taskID' => 2 ] );

        $result = $this->index_manager->reindex_all();
        $this->assertSame( 1, $result['total'] );
    }

    // ─── reindex_all failure preservation ────────────────────

    public function test_reindex_all_leaves_live_intact_when_save_fails(): void {
        global $wp_posts_store;

        $wp_posts_store[1] = $this->make_post( [
            'ID' => 1, 'post_status' => 'publish', 'post_type' => 'post',
        ] );

        $this->search_client
            ->method( 'saveObjects' )
            ->willThrowException( new \RuntimeException( 'Batch upload failed' ) );

        // The live index must never be published, and any cleanup deleteIndex
        // must target only the orphaned temp index — never the live index.
        $this->search_client->expects( $this->never() )->method( 'operationIndex' );
        $this->search_client
            ->method( 'deleteIndex' )
            ->with( $this->matchesRegularExpression( self::TEMP_INDEX_PATTERN ) );

        $this->expectException( \RuntimeException::class );
        $this->expectExceptionMessage( 'Batch upload failed' );

        $this->index_manager->reindex_all();
    }

    public function test_reindex_all_leaves_live_intact_when_settings_fail(): void {
        global $wp_posts_store;

        $wp_posts_store[1] = $this->make_post( [
            'ID' => 1, 'post_status' => 'publish', 'post_type' => 'post',
        ] );

        $this->search_client->method( 'saveObjects' )->willReturn( [ 'objectIDs' => [ '1' ] ] );
        $this->search_client
            ->method( 'setSettings' )
            ->willThrowException( new \RuntimeException( 'Settings rejected' ) );

        // Settings failure happens before the move, so nothing is published; any
        // cleanup deleteIndex targets only the temp index, never the live index.
        $this->search_client->expects( $this->never() )->method( 'operationIndex' );
        $this->search_client
            ->method( 'deleteIndex' )
            ->with( $this->matchesRegularExpression( self::TEMP_INDEX_PATTERN ) );

        $this->expectException( \RuntimeException::class );
        $this->expectExceptionMessage( 'Settings rejected' );

        $this->index_manager->reindex_all();
    }

    // ─── cross-request rebuild helpers (background path) ─────

    public function test_begin_rebuild_persists_canonical_temp_index_state(): void {
        $temp = $this->index_manager->begin_rebuild();

        $this->assertMatchesRegularExpression( self::TEMP_INDEX_PATTERN, $temp );

        $state = get_transient( IndexManager::REBUILD_STATE_TRANSIENT );
        $this->assertIsArray( $state );
        $this->assertSame( $temp, $state['temp_index'] );
        $this->assertSame( 'wp_posts', $state['live_index'] );
    }

    public function test_append_records_to_rebuild_saves_to_temp_index(): void {
        $temp = $this->index_manager->begin_rebuild();

        $this->search_client
            ->expects( $this->once() )
            ->method( 'saveObjects' )
            ->with( $temp, $this->callback( fn( $records ) => count( $records ) === 2 ) )
            ->willReturn( [ 'objectIDs' => [ '1', '2' ] ] );

        $this->index_manager->append_records_to_rebuild( [
            [ 'objectID' => '1' ],
            [ 'objectID' => '2' ],
        ] );
    }

    public function test_append_records_to_rebuild_skips_empty_batch(): void {
        $this->index_manager->begin_rebuild();

        $this->search_client->expects( $this->never() )->method( 'saveObjects' );

        $this->index_manager->append_records_to_rebuild( [] );
    }

    public function test_publish_rebuild_moves_temp_to_live_and_clears_state(): void {
        $temp = $this->index_manager->begin_rebuild();

        $this->search_client
            ->expects( $this->once() )
            ->method( 'setSettings' )
            ->with( $temp, $this->isType( 'array' ) )
            ->willReturn( [ 'taskID' => 1 ] );

        $this->search_client
            ->expects( $this->once() )
            ->method( 'operationIndex' )
            ->with(
                $temp,
                $this->callback( fn( array $p ) => $p['operation'] === 'move' && $p['destination'] === 'wp_posts' )
            )
            ->willReturn( [ 'taskID' => 2 ] );

        $this->index_manager->publish_rebuild();

        $this->assertFalse( get_transient( IndexManager::REBUILD_STATE_TRANSIENT ) );
    }

    public function test_append_records_without_active_rebuild_throws(): void {
        $this->expectException( \RuntimeException::class );
        $this->index_manager->append_records_to_rebuild( [ [ 'objectID' => '1' ] ] );
    }

    public function test_publish_rebuild_without_active_rebuild_throws(): void {
        $this->expectException( \RuntimeException::class );
        $this->index_manager->publish_rebuild();
    }

    // ─── keyset enumeration ──────────────────────────────────

    public function test_index_rebuild_batch_enumerates_by_keyset_cursor(): void {
        global $wp_posts_store;

        $this->index_manager->begin_rebuild();
        for ( $i = 1; $i <= 3; $i++ ) {
            $wp_posts_store[ $i ] = $this->make_post( [
                'ID' => $i, 'post_status' => 'publish', 'post_type' => 'post',
            ] );
        }

        $saved = [];
        $this->search_client
            ->method( 'saveObjects' )
            ->willReturnCallback( function ( string $index, array $records ) use ( &$saved ) {
                $saved[] = array_map( fn( $r ) => $r['objectID'], $records );
                return [ 'objectIDs' => [] ];
            } );

        $first = $this->index_manager->index_rebuild_batch( 2 );
        $this->assertSame( 2, $first['processed'] );
        $this->assertFalse( $first['done'] );

        $second = $this->index_manager->index_rebuild_batch( 2 );
        $this->assertSame( 1, $second['processed'] );
        $this->assertTrue( $second['done'] );

        // The cursor advanced past the first page, so the two batches cover
        // disjoint ascending ID ranges — no overlap and no skipped posts.
        $this->assertSame( [ [ '1', '2' ], [ '3' ] ], $saved );

        $state = get_transient( IndexManager::REBUILD_STATE_TRANSIENT );
        $this->assertSame( 3, $state['cursor'] );
    }

    // ─── unique temp-index names ─────────────────────────────

    public function test_begin_rebuild_generates_unique_temp_index_within_same_second(): void {
        // Two rebuilds started back-to-back (same wall-clock second) must never
        // share a temp index name, or their writes would collide.
        $first  = $this->index_manager->begin_rebuild();
        $second = $this->index_manager->begin_rebuild();

        $this->assertMatchesRegularExpression( self::TEMP_INDEX_PATTERN, $first );
        $this->assertMatchesRegularExpression( self::TEMP_INDEX_PATTERN, $second );
        $this->assertNotSame( $first, $second );
    }

    // ─── abort cleanup (temp index only) ─────────────────────

    public function test_reindex_all_cleans_up_temp_index_when_build_fails(): void {
        global $wp_posts_store;

        $wp_posts_store[1] = $this->make_post( [
            'ID' => 1, 'post_status' => 'publish', 'post_type' => 'post',
        ] );

        $this->search_client
            ->method( 'saveObjects' )
            ->willThrowException( new \RuntimeException( 'Batch upload failed' ) );

        // A failed rebuild deletes exactly the orphaned temp index.
        $this->search_client
            ->expects( $this->once() )
            ->method( 'deleteIndex' )
            ->with( $this->matchesRegularExpression( self::TEMP_INDEX_PATTERN ) );
        $this->search_client->expects( $this->never() )->method( 'operationIndex' );

        $this->expectException( \RuntimeException::class );
        $this->expectExceptionMessage( 'Batch upload failed' );

        $this->index_manager->reindex_all();
    }

    public function test_reindex_all_cleanup_failure_does_not_mask_original_error(): void {
        global $wp_posts_store;

        $wp_posts_store[1] = $this->make_post( [
            'ID' => 1, 'post_status' => 'publish', 'post_type' => 'post',
        ] );

        $this->search_client
            ->method( 'saveObjects' )
            ->willThrowException( new \RuntimeException( 'Batch upload failed' ) );

        // Even when temp-index cleanup itself fails, the original rebuild error
        // is what surfaces — never the cleanup error.
        $this->search_client
            ->method( 'deleteIndex' )
            ->willThrowException( new \RuntimeException( 'cleanup boom' ) );

        $this->expectException( \RuntimeException::class );
        $this->expectExceptionMessage( 'Batch upload failed' );

        $this->index_manager->reindex_all();
    }

    // ─── mid-build mutation tracking (dirty IDs / tombstones) ─

    public function test_index_post_during_rebuild_writes_live_and_records_dirty_id(): void {
        $this->index_manager->begin_rebuild();

        $post = $this->make_post( [ 'ID' => 42, 'post_status' => 'publish', 'post_type' => 'post' ] );

        // The live single-post write still happens during a rebuild.
        $this->search_client
            ->expects( $this->once() )
            ->method( 'addOrUpdateObject' )
            ->with( 'wp_posts', '42', $this->isType( 'array' ) )
            ->willReturn( [ 'objectID' => '42', 'taskID' => 1 ] );

        $this->index_manager->index_post( $post );

        $state = get_transient( IndexManager::REBUILD_STATE_TRANSIENT );
        $this->assertContains( 42, $state['dirty_ids'] );
        $this->assertNotContains( 42, $state['tombstones'] );
    }

    public function test_delete_post_during_rebuild_deletes_live_and_records_tombstone(): void {
        $this->index_manager->begin_rebuild();

        $this->search_client
            ->expects( $this->once() )
            ->method( 'deleteObject' )
            ->with( 'wp_posts', '42' )
            ->willReturn( [ 'taskID' => 1 ] );

        $this->index_manager->delete_post( 42 );

        $state = get_transient( IndexManager::REBUILD_STATE_TRANSIENT );
        $this->assertContains( 42, $state['tombstones'] );
        $this->assertNotContains( 42, $state['dirty_ids'] );
    }

    public function test_index_post_unpublish_during_rebuild_records_tombstone(): void {
        $this->index_manager->begin_rebuild();

        // A draft post routes through delete_post — it should be tombstoned, not
        // marked dirty, so publication removes it from the temp index.
        $post = $this->make_post( [ 'ID' => 42, 'post_status' => 'draft', 'post_type' => 'post' ] );

        $this->search_client
            ->expects( $this->once() )
            ->method( 'deleteObject' )
            ->with( 'wp_posts', '42' )
            ->willReturn( [ 'taskID' => 1 ] );

        $this->index_manager->index_post( $post );

        $state = get_transient( IndexManager::REBUILD_STATE_TRANSIENT );
        $this->assertContains( 42, $state['tombstones'] );
        $this->assertNotContains( 42, $state['dirty_ids'] );
    }

    public function test_index_post_outside_rebuild_records_no_mutation_state(): void {
        $post = $this->make_post( [ 'ID' => 42, 'post_status' => 'publish', 'post_type' => 'post' ] );

        $this->search_client
            ->method( 'addOrUpdateObject' )
            ->willReturn( [ 'objectID' => '42', 'taskID' => 1 ] );

        $this->index_manager->index_post( $post );

        // With no rebuild in progress, no rebuild state is created.
        $this->assertFalse( get_transient( IndexManager::REBUILD_STATE_TRANSIENT ) );
    }

    public function test_dirty_then_delete_moves_id_from_dirty_to_tombstone(): void {
        $this->index_manager->begin_rebuild();

        $post = $this->make_post( [ 'ID' => 42, 'post_status' => 'publish', 'post_type' => 'post' ] );
        $this->search_client->method( 'addOrUpdateObject' )->willReturn( [ 'taskID' => 1 ] );
        $this->search_client->method( 'deleteObject' )->willReturn( [ 'taskID' => 1 ] );

        $this->index_manager->index_post( $post ); // dirty
        $this->index_manager->delete_post( 42 );   // then removed

        $state = get_transient( IndexManager::REBUILD_STATE_TRANSIENT );
        $this->assertContains( 42, $state['tombstones'] );
        $this->assertNotContains( 42, $state['dirty_ids'] );
    }

    // ─── mutation replay at publication ──────────────────────

    public function test_publish_rebuild_replays_dirty_ids_onto_temp_before_move(): void {
        global $wp_posts_store;

        $temp = $this->index_manager->begin_rebuild();

        // Post 5 was written live during the rebuild.
        $wp_posts_store[5] = $this->make_post( [ 'ID' => 5, 'post_status' => 'publish', 'post_type' => 'post' ] );
        $this->seed_mutation_state( $temp, [ 5 ], [] );

        // The dirty post's current record is replayed onto the temp index.
        $this->search_client
            ->expects( $this->once() )
            ->method( 'saveObjects' )
            ->with( $temp, $this->callback( fn( $records ) => count( $records ) === 1 && $records[0]['objectID'] === '5' ) )
            ->willReturn( [ 'objectIDs' => [ '5' ] ] );

        $this->search_client->method( 'setSettings' )->willReturn( [ 'taskID' => 1 ] );
        $this->search_client
            ->expects( $this->once() )
            ->method( 'operationIndex' )
            ->willReturn( [ 'taskID' => 2 ] );

        $this->index_manager->publish_rebuild();
    }

    public function test_publish_rebuild_removes_tombstoned_ids_from_temp_before_move(): void {
        $temp = $this->index_manager->begin_rebuild();
        $this->seed_mutation_state( $temp, [], [ 9 ] );

        $this->search_client
            ->expects( $this->once() )
            ->method( 'deleteObject' )
            ->with( $temp, '9' )
            ->willReturn( [ 'taskID' => 1 ] );

        $this->search_client->method( 'setSettings' )->willReturn( [ 'taskID' => 1 ] );
        $this->search_client
            ->expects( $this->once() )
            ->method( 'operationIndex' )
            ->willReturn( [ 'taskID' => 2 ] );

        $this->index_manager->publish_rebuild();
    }

    public function test_publish_rebuild_replays_dirty_id_as_delete_when_no_longer_indexable(): void {
        // Dirty ID 7 has no live post (deleted after being marked dirty) — the
        // replay must remove it from the temp index rather than re-add it.
        $temp = $this->index_manager->begin_rebuild();
        $this->seed_mutation_state( $temp, [ 7 ], [] );

        $this->search_client->expects( $this->never() )->method( 'saveObjects' );
        $this->search_client
            ->expects( $this->once() )
            ->method( 'deleteObject' )
            ->with( $temp, '7' )
            ->willReturn( [ 'taskID' => 1 ] );

        $this->search_client->method( 'setSettings' )->willReturn( [ 'taskID' => 1 ] );
        $this->search_client->method( 'operationIndex' )->willReturn( [ 'taskID' => 2 ] );

        $this->index_manager->publish_rebuild();
    }

    // ─── explicit abort on unreconcilable mutation state ─────

    public function test_publish_rebuild_aborts_move_when_mutations_over_limit(): void {
        $temp = $this->index_manager->begin_rebuild();

        // Simulate an overflowed mutation log — too many changes to trust a
        // cheap replay, so publication must refuse the move.
        set_transient( IndexManager::REBUILD_STATE_TRANSIENT, [
            'temp_index' => $temp,
            'live_index' => 'wp_posts',
            'dirty_ids'  => [],
            'tombstones' => [],
            'overflow'   => true,
        ], HOUR_IN_SECONDS );

        // The move is refused and the temp index is cleaned up.
        $this->search_client->expects( $this->never() )->method( 'operationIndex' );
        $this->search_client
            ->expects( $this->once() )
            ->method( 'deleteIndex' )
            ->with( $temp );

        $this->expectException( \RuntimeException::class );

        $this->index_manager->publish_rebuild();
    }

    public function test_mutation_log_flags_overflow_beyond_limit(): void {
        $temp = $this->index_manager->begin_rebuild();

        // Seed the log one short of the limit, then one more mutation tips it.
        set_transient( IndexManager::REBUILD_STATE_TRANSIENT, [
            'temp_index' => $temp,
            'live_index' => 'wp_posts',
            'dirty_ids'  => range( 1, IndexManager::MUTATION_LIMIT ),
            'tombstones' => [],
            'overflow'   => false,
        ], HOUR_IN_SECONDS );

        $this->search_client->method( 'deleteObject' )->willReturn( [ 'taskID' => 1 ] );

        // One more distinct removal pushes the combined log past MUTATION_LIMIT.
        $this->index_manager->delete_post( IndexManager::MUTATION_LIMIT + 1 );

        $state = get_transient( IndexManager::REBUILD_STATE_TRANSIENT );
        $this->assertTrue( $state['overflow'] );
    }

    /**
     * Seed the mutation buckets of the active rebuild state.
     *
     * @param int[] $dirty_ids
     * @param int[] $tombstones
     */
    private function seed_mutation_state( string $temp_index, array $dirty_ids, array $tombstones ): void {
        set_transient( IndexManager::REBUILD_STATE_TRANSIENT, [
            'temp_index' => $temp_index,
            'live_index' => 'wp_posts',
            'dirty_ids'  => $dirty_ids,
            'tombstones' => $tombstones,
            'overflow'   => false,
        ], HOUR_IN_SECONDS );
    }
}
