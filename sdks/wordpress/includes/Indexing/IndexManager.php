<?php
/**
 * Manages the Flapjack search index — CRUD operations for WordPress content.
 *
 * @package Flapjack\WordPress\Indexing
 */

declare(strict_types=1);

namespace Flapjack\WordPress\Indexing;

use Flapjack\WordPress\ClientFactory;
use Flapjack\WordPress\Status\FailureReporter;
use Flapjack\FlapjackSearch\Exceptions\NotFoundException;

class IndexManager {

    /**
     * Transient holding the canonical cross-request rebuild state
     * (the temp index name and its live destination). This is the single
     * owner of full-reindex temp-index state; BackgroundIndexer keeps only
     * display/progress fields in its own transient.
     */
    public const REBUILD_STATE_TRANSIENT = 'flapjack_reindex_rebuild_state';

    /**
     * Maximum number of single-post mutations (dirty writes + tombstones) that
     * may accumulate during a rebuild before publication refuses the move. Past
     * this bound the temp index is too stale to trust with a cheap replay, so
     * the rebuild aborts rather than publish a possibly-inconsistent index.
     */
    public const MUTATION_LIMIT = 10000;

    /**
     * Per-request monotonic counter guaranteeing distinct temp-index names for
     * multiple rebuilds started within the same second in one request.
     */
    private static int $rebuild_sequence = 0;

    private ClientFactory $client_factory;

    public function __construct( ClientFactory $client_factory ) {
        $this->client_factory = $client_factory;
    }

    /**
     * Index a single post.
     *
     * @param \WP_Post|int $post
     * @return array The API response.
     */
    public function index_post( \WP_Post|int $post ): array {
        if ( is_int( $post ) ) {
            $post = get_post( $post );
        }

        if ( ! $post instanceof \WP_Post ) {
            throw new \InvalidArgumentException( 'Invalid post.' );
        }

        if ( ! $this->should_index_post( $post ) ) {
            // If the post shouldn't be indexed, remove it in case it was previously indexed.
            return $this->delete_post( $post->ID );
        }

        $record = $this->build_record( $post );
        $client = $this->client_factory->get_client();
        $index  = $this->client_factory->get_index_name();

        $response = $client->addOrUpdateObject( $index, $record['objectID'], $record );

        // If a full rebuild is in flight, the live write above keeps real-time
        // search correct; record the ID so publication replays this post's
        // current state onto the temp index before the atomic move.
        $this->record_dirty_id( (int) $post->ID );

        return $response;
    }

    /**
     * Delete a post from the index.
     *
     * @param int $post_id
     * @return array The API response.
     */
    public function delete_post( int $post_id ): array {
        $client = $this->client_factory->get_client();
        $index  = $this->client_factory->get_index_name();

        try {
            $result = $client->deleteObject( $index, (string) $post_id );
        } catch ( NotFoundException $e ) {
            // A typed not-found means the object simply isn't in the index —
            // deletion is idempotent, so this is a success, not a failure. Match
            // on the exception type rather than substrings in an arbitrary
            // message, which could swallow unrelated errors that happen to
            // mention "404" or "not found".
            $result = [ 'deleted' => true ];
        } catch ( \Throwable $e ) {
            // A real delete failure — record it durably before rethrowing so it
            // stays visible in status/admin instead of vanishing into the
            // caller's catch block.
            FailureReporter::record( $e, [
                'operation'  => 'delete_post',
                'source'     => 'index_manager',
                'post_id'    => $post_id,
                'index_name' => $index,
            ] );

            throw $e;
        }

        // Tombstone the removal so an in-flight rebuild drops this post from the
        // temp index before publishing, even if its batch had already been built.
        $this->record_tombstone( $post_id );

        return $result;
    }

    /**
     * Reindex all content atomically.
     *
     * This is the single canonical full-reindex entrypoint for CLI, admin, and
     * REST callers. It populates a fresh temporary index from current
     * WordPress content, applies settings to that temp index, and atomically
     * moves it over the live index via Algolia's operationIndex(move). The
     * previous live index is only replaced once the rebuild fully succeeds, so
     * a failure mid-rebuild leaves the existing live index untouched and no
     * live-index cleanup is attempted. Publication is exact: objects that no
     * longer exist in WordPress are gone after the move.
     *
     * @return array{total: int, batches: int}
     */
    public function reindex_all(): array {
        $this->begin_rebuild();

        try {
            $counts = $this->build_temp_index();
        } catch ( \Throwable $e ) {
            // Rebuild failed before publication — drop the orphaned temp index
            // and leave the live index untouched.
            $this->abort_rebuild();
            throw $e;
        }

        // publish_rebuild() reconciles mid-build mutations, applies settings, and
        // performs the atomic move; it self-aborts (temp cleanup) on failure.
        $this->publish_rebuild();

        return $counts;
    }

    /**
     * Begin a cross-request rebuild, persisting the canonical temp-index name
     * and an empty mid-build mutation log.
     *
     * Used by both the synchronous full reindex and background reindexing, where
     * each batch runs in a separate request. The returned temp index is the
     * single canonical target for all batches until publish_rebuild() moves it
     * over the live index. Because the state lives in a shared transient, live
     * single-post writes from concurrent requests can record dirty IDs and
     * tombstones against the same rebuild.
     *
     * @return string The temporary index name.
     */
    public function begin_rebuild(): string {
        $live_index = $this->client_factory->get_index_name();
        $temp_index = $this->generate_temp_index_name( $live_index );

        set_transient( self::REBUILD_STATE_TRANSIENT, [
            'temp_index' => $temp_index,
            'live_index' => $live_index,
            'cursor'     => 0,
            'dirty_ids'  => [],
            'tombstones' => [],
            'overflow'   => false,
        ], HOUR_IN_SECONDS );

        return $temp_index;
    }

    /**
     * Enumerate and index the next keyset page of the active rebuild onto its
     * temp index, advancing the persisted ID cursor.
     *
     * This is the single enumeration owner shared by the synchronous full
     * reindex and cross-request background batches, so both seek by ID (stable
     * against inserts/deletes mid-build) instead of paging by offset.
     *
     * @param int $batch_size
     * @return array{processed: int, done: bool}
     */
    public function index_rebuild_batch( int $batch_size ): array {
        $state      = $this->get_rebuild_state();
        $after      = (int) ( $state['cursor'] ?? 0 );
        $post_types = (array) get_option( 'flapjack_post_types', [ 'post', 'page' ] );

        $posts = $this->query_indexable_posts_after( $post_types, $after, $batch_size );
        $seen  = count( $posts );

        $records = [];
        $highest = $after;
        foreach ( $posts as $post ) {
            if ( $this->should_index_post( $post ) ) {
                $records[] = $this->build_record( $post );
            }
            // The cursor advances past every enumerated post — including
            // ineligible ones — so a page of skipped posts still makes progress.
            $highest = max( $highest, (int) $post->ID );
        }

        // The keyset cursor must strictly advance while posts remain; otherwise
        // the query is returning overlapping rows and enumeration is unsound.
        if ( $seen > 0 && $highest <= $after ) {
            throw new \RuntimeException( 'Reindex enumeration cursor failed to advance; aborting to avoid an inconsistent index.' );
        }

        $this->append_records_to_rebuild( $records );
        $this->advance_rebuild_cursor( $highest );

        return [
            'processed' => count( $records ),
            'done'      => $seen < $batch_size,
        ];
    }

    /**
     * Append already-built records to the in-progress rebuild's temp index.
     *
     * @param array<int, array<string, mixed>> $records
     */
    public function append_records_to_rebuild( array $records ): void {
        if ( empty( $records ) ) {
            return;
        }

        $state = $this->get_rebuild_state();
        $this->client_factory->get_client()->saveObjects( $state['temp_index'], $records );
    }

    /**
     * Publish the in-progress rebuild: reconcile mid-build mutations, apply
     * settings, then atomically move the temp index over the live index.
     *
     * On any failure the temp index is dropped and the live index is left
     * untouched, so a failed publication never damages the current live data.
     */
    public function publish_rebuild(): void {
        $state = $this->get_rebuild_state();

        try {
            $this->reconcile_mutations( $state );
            $this->publish_temp_index( $state['temp_index'], $state['live_index'] );
        } catch ( \Throwable $e ) {
            $this->abort_rebuild();
            throw $e;
        }

        delete_transient( self::REBUILD_STATE_TRANSIENT );
    }

    /**
     * Read and validate the canonical rebuild state.
     *
     * Only temp_index and live_index are guaranteed present; the cursor and
     * mutation buckets are seeded by begin_rebuild and defaulted by readers so
     * legacy or partially-seeded state stays safe to consume.
     *
     * @return array{temp_index: string, live_index: string, cursor?: int, dirty_ids?: int[], tombstones?: int[], overflow?: bool}
     */
    private function get_rebuild_state(): array {
        $state = get_transient( self::REBUILD_STATE_TRANSIENT );

        if ( ! is_array( $state ) || empty( $state['temp_index'] ) || empty( $state['live_index'] ) ) {
            throw new \RuntimeException( 'No reindex rebuild is in progress.' );
        }

        return $state;
    }

    /**
     * Abort the in-progress rebuild: clear its state and best-effort delete the
     * orphaned temp index. Cleanup failures are swallowed so they can never mask
     * the original rebuild error that triggered the abort.
     */
    private function abort_rebuild(): void {
        $state = get_transient( self::REBUILD_STATE_TRANSIENT );
        delete_transient( self::REBUILD_STATE_TRANSIENT );

        if ( ! is_array( $state ) || empty( $state['temp_index'] ) ) {
            return;
        }

        try {
            $this->client_factory->get_client()->deleteIndex( $state['temp_index'] );
        } catch ( \Throwable $cleanup_error ) {
            if ( defined( 'WP_DEBUG' ) && WP_DEBUG ) {
                error_log( sprintf(
                    '[Flapjack Search] Failed to clean up temp index %s after a rebuild abort: %s',
                    $state['temp_index'],
                    $cleanup_error->getMessage()
                ) );
            }
        }
    }

    /**
     * Reconcile single-post mutations recorded while the rebuild ran, replaying
     * them onto the temp index so the moved index reflects the latest state.
     *
     * Refuses to proceed (and thus refuses the move) when the mutation log
     * overflowed — the temp index is then too stale to trust.
     *
     * @param array{temp_index: string, dirty_ids?: int[], tombstones?: int[], overflow?: bool} $state
     */
    private function reconcile_mutations( array $state ): void {
        if ( ! empty( $state['overflow'] ) ) {
            throw new \RuntimeException( sprintf(
                'Reindex mutation log overflowed (> %d changes during rebuild); refusing to publish a possibly-inconsistent index.',
                self::MUTATION_LIMIT
            ) );
        }

        $client = $this->client_factory->get_client();
        $temp   = $state['temp_index'];

        foreach ( $state['dirty_ids'] ?? [] as $post_id ) {
            $post = get_post( (int) $post_id );

            if ( $post instanceof \WP_Post && $this->should_index_post( $post ) ) {
                $client->saveObjects( $temp, [ $this->build_record( $post ) ] );
            } else {
                // The post was edited into an unindexable state after its batch
                // was written — remove it from the temp index.
                $client->deleteObject( $temp, (string) $post_id );
            }
        }

        foreach ( $state['tombstones'] ?? [] as $post_id ) {
            $client->deleteObject( $temp, (string) $post_id );
        }
    }

    /**
     * Record a live post write against an in-flight rebuild so it is replayed
     * onto the temp index at publication. No-op when no rebuild is active.
     */
    private function record_dirty_id( int $post_id ): void {
        $this->record_mutation( 'dirty_ids', 'tombstones', $post_id );
    }

    /**
     * Record a live post removal against an in-flight rebuild so it is dropped
     * from the temp index at publication. No-op when no rebuild is active.
     */
    private function record_tombstone( int $post_id ): void {
        $this->record_mutation( 'tombstones', 'dirty_ids', $post_id );
    }

    /**
     * Move a post ID into one mutation bucket, removing it from the other so the
     * latest intent wins, and flag overflow once the log exceeds MUTATION_LIMIT.
     */
    private function record_mutation( string $add_key, string $remove_key, int $post_id ): void {
        $state = get_transient( self::REBUILD_STATE_TRANSIENT );

        if ( ! is_array( $state ) || empty( $state['temp_index'] ) ) {
            // No active rebuild — the live single-post write is the whole story.
            return;
        }

        $state[ $remove_key ] = array_values( array_diff( $state[ $remove_key ] ?? [], [ $post_id ] ) );

        $bucket = $state[ $add_key ] ?? [];
        if ( ! in_array( $post_id, $bucket, true ) ) {
            $bucket[] = $post_id;
        }
        $state[ $add_key ] = $bucket;

        if ( count( $state['dirty_ids'] ) + count( $state['tombstones'] ) > self::MUTATION_LIMIT ) {
            $state['overflow'] = true;
        }

        set_transient( self::REBUILD_STATE_TRANSIENT, $state, HOUR_IN_SECONDS );
    }

    /**
     * Generate a unique temporary index name for a rebuild.
     *
     * Bare time() collides for two rebuilds within the same second. Combining it
     * with a random component (cross-request uniqueness) and a per-request
     * monotonic sequence (within-request uniqueness) keeps every temp name
     * distinct while remaining a pure `<live>_tmp_<digits>` identifier.
     */
    private function generate_temp_index_name( string $live_index ): string {
        self::$rebuild_sequence++;

        return sprintf(
            '%s_tmp_%d%04d%03d',
            $live_index,
            time(),
            random_int( 0, 9999 ),
            self::$rebuild_sequence % 1000
        );
    }

    /**
     * Build the whole live store into the rebuild's temp index by draining the
     * shared keyset enumeration primitive one page at a time.
     *
     * @return array{total: int, batches: int}
     */
    private function build_temp_index(): array {
        $total   = 0;
        $batches = 0;

        do {
            $result = $this->index_rebuild_batch( 500 );

            if ( $result['processed'] > 0 ) {
                $total += $result['processed'];
                $batches++;
            }
        } while ( ! $result['done'] );

        return [
            'total'   => $total,
            'batches' => $batches,
        ];
    }

    /**
     * Advance the persisted rebuild cursor, re-reading the state first so a
     * concurrent single-post mutation logged against the same rebuild is not
     * clobbered by the cursor write.
     */
    private function advance_rebuild_cursor( int $cursor ): void {
        $state = get_transient( self::REBUILD_STATE_TRANSIENT );

        if ( ! is_array( $state ) || empty( $state['temp_index'] ) ) {
            return;
        }

        $state['cursor'] = max( (int) ( $state['cursor'] ?? 0 ), $cursor );
        set_transient( self::REBUILD_STATE_TRANSIENT, $state, HOUR_IN_SECONDS );
    }

    /**
     * Query one keyset page of indexable posts with ID greater than the cursor.
     *
     * @param string[] $post_types
     * @return \WP_Post[]
     */
    private function query_indexable_posts_after( array $post_types, int $after, int $batch_size ): array {
        add_filter( 'posts_where', [ $this, 'restrict_query_to_cursor' ], 10, 2 );

        try {
            $query = new \WP_Query( [
                'post_type'         => $post_types,
                'post_status'       => 'publish',
                'posts_per_page'    => $batch_size,
                'orderby'           => 'ID',
                'order'             => 'ASC',
                'flapjack_after_id' => $after,
                // Disable Flapjack search interception for this query.
                'flapjack_bypass'   => true,
            ] );
        } finally {
            remove_filter( 'posts_where', [ $this, 'restrict_query_to_cursor' ], 10 );
        }

        return $query->posts;
    }

    /**
     * WordPress `posts_where` filter: translate the keyset cursor query var into
     * a SQL ID boundary so full-reindex enumeration seeks by ID instead of
     * paging by offset. Only affects queries carrying flapjack_after_id.
     *
     * @param string    $where
     * @param \WP_Query $query
     */
    public function restrict_query_to_cursor( string $where, \WP_Query $query ): string {
        $after = (int) $query->get( 'flapjack_after_id' );

        if ( $after > 0 ) {
            global $wpdb;
            $where .= $wpdb->prepare( " AND {$wpdb->posts}.ID > %d", $after );
        }

        return $where;
    }

    /**
     * Apply settings to the temp index, then atomically move it over the live index.
     */
    private function publish_temp_index( string $temp_index, string $live_index ): void {
        $this->configure_index_settings( $temp_index );

        // Atomic swap: move temp → live (overwrites the live index).
        $this->client_factory->get_client()->operationIndex( $temp_index, [
            'operation'   => 'move',
            'destination' => $live_index,
        ] );
    }

    /**
     * Get index statistics.
     *
     * @return array{exists: bool, count: int, name: string}
     */
    public function get_index_stats(): array {
        $client     = $this->client_factory->get_client();
        $index_name = $this->client_factory->get_index_name();

        try {
            $settings = $client->getSettings( $index_name );
            // Try to get a count via an empty search.
            $result = $client->searchSingleIndex( $index_name, [
                'query'            => '',
                'hitsPerPage'      => 0,
                'analytics'        => false,
            ] );

            return [
                'exists' => true,
                'count'  => (int) ( $result['nbHits'] ?? 0 ),
                'name'   => $index_name,
            ];
        } catch ( \Throwable $e ) {
            return [
                'exists' => false,
                'count'  => 0,
                'name'   => $index_name,
            ];
        }
    }

    /**
     * Check whether a post should be indexed.
     */
    public function should_index_post( \WP_Post $post ): bool {
        $post_types = (array) get_option( 'flapjack_post_types', [ 'post', 'page' ] );

        if ( ! in_array( $post->post_type, $post_types, true ) ) {
            return false;
        }

        if ( 'publish' !== $post->post_status ) {
            return false;
        }

        if ( ! empty( $post->post_password ) ) {
            return false;
        }

        /**
         * Filter whether a specific post should be indexed.
         *
         * @param bool     $should_index Whether to index the post.
         * @param \WP_Post $post         The post object.
         */
        return (bool) apply_filters( 'flapjack_should_index_post', true, $post );
    }

    /**
     * Build a search record from a WP_Post.
     *
     * @param \WP_Post $post
     * @return array<string, mixed>
     */
    public function build_record( \WP_Post $post ): array {
        $record = [
            'objectID'       => (string) $post->ID,
            'post_id'        => $post->ID,
            'post_title'     => $post->post_title,
            'post_excerpt'   => $this->get_excerpt( $post ),
            'post_content'   => $this->get_clean_content( $post ),
            'post_type'      => $post->post_type,
            'post_type_label' => get_post_type_object( $post->post_type )?->labels->singular_name ?? $post->post_type,
            'post_status'    => $post->post_status,
            'post_date'      => strtotime( $post->post_date_gmt ) ?: 0,
            'post_modified'  => strtotime( $post->post_modified_gmt ) ?: 0,
            'permalink'      => get_permalink( $post ),
            'author'         => [
                'id'   => (int) $post->post_author,
                'name' => get_the_author_meta( 'display_name', (int) $post->post_author ),
            ],
        ];

        // Thumbnail.
        $thumbnail_id = get_post_thumbnail_id( $post );
        if ( $thumbnail_id ) {
            $record['thumbnail'] = wp_get_attachment_image_url( (int) $thumbnail_id, 'medium' );
        }

        // Taxonomies.
        $taxonomies = get_object_taxonomies( $post->post_type, 'objects' );
        foreach ( $taxonomies as $taxonomy ) {
            if ( ! $taxonomy->public ) {
                continue;
            }
            $terms = get_the_terms( $post, $taxonomy->name );
            if ( ! empty( $terms ) && ! is_wp_error( $terms ) ) {
                $record[ 'taxonomy_' . $taxonomy->name ] = array_map(
                    fn( \WP_Term $term ) => $term->name,
                    $terms
                );
            }
        }

        // Menu order (useful for pages).
        $record['menu_order'] = $post->menu_order;

        // Comment count.
        $record['comment_count'] = (int) $post->comment_count;

        /**
         * Filter the search record before indexing.
         *
         * @param array    $record The search record.
         * @param \WP_Post $post   The post object.
         */
        return (array) apply_filters( 'flapjack_post_record', $record, $post );
    }

    /**
     * Configure index settings (searchable attributes, facets, etc.).
     */
    public function configure_index_settings( string $index_name ): void {
        $client = $this->client_factory->get_client();

        $searchable = (array) get_option( 'flapjack_searchable_attrs', [ 'post_title', 'post_content', 'post_excerpt' ] );

        $searchable_attributes = [];
        if ( in_array( 'post_title', $searchable, true ) ) {
            $searchable_attributes[] = 'post_title';
        }
        if ( in_array( 'post_content', $searchable, true ) ) {
            $searchable_attributes[] = 'post_content';
        }
        if ( in_array( 'post_excerpt', $searchable, true ) ) {
            $searchable_attributes[] = 'post_excerpt';
        }
        if ( in_array( 'author', $searchable, true ) ) {
            $searchable_attributes[] = 'author.name';
        }

        $settings = [
            'searchableAttributes' => $searchable_attributes,
            'attributesForFaceting' => [
                'filterOnly(post_type)',
                'filterOnly(post_status)',
                'taxonomy_category',
                'taxonomy_post_tag',
                'author.name',
            ],
            'customRanking' => [
                'desc(post_date)',
            ],
            'attributesToSnippet' => [
                'post_content:30',
                'post_excerpt:30',
            ],
            'attributesToHighlight' => [
                'post_title',
                'post_content',
                'post_excerpt',
            ],
        ];

        /**
         * Filter the index settings before applying.
         *
         * @param array  $settings   The index settings.
         * @param string $index_name The index name.
         */
        $settings = (array) apply_filters( 'flapjack_index_settings', $settings, $index_name );

        $client->setSettings( $index_name, $settings );
    }

    /**
     * Get cleaned post content (strip shortcodes, blocks, HTML).
     */
    private function get_clean_content( \WP_Post $post ): string {
        $content = $post->post_content;
        $content = strip_shortcodes( $content );
        $content = excerpt_remove_blocks( $content );
        $content = wp_strip_all_tags( $content );
        $content = preg_replace( '/\s+/', ' ', $content ) ?? $content;

        // Truncate to ~10k chars to stay within index limits.
        if ( mb_strlen( $content ) > 10000 ) {
            $content = mb_substr( $content, 0, 10000 );
        }

        return trim( $content );
    }

    /**
     * Get the post excerpt, generating one if needed.
     */
    private function get_excerpt( \WP_Post $post ): string {
        if ( ! empty( $post->post_excerpt ) ) {
            return wp_strip_all_tags( $post->post_excerpt );
        }

        // Generate from content.
        $content = $this->get_clean_content( $post );
        return mb_strlen( $content ) > 300 ? mb_substr( $content, 0, 300 ) . '...' : $content;
    }
}
