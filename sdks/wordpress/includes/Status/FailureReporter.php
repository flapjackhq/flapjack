<?php
/**
 * Durable, sanitized store for the latest indexing failure.
 *
 * This is the single canonical owner of persisted failure state. Indexing
 * (IndexManager), background jobs (BackgroundIndexer), and WordPress hook flow
 * (PostSyncHooks) all report through here instead of inventing their own option
 * shapes, and REST/status, CLI, and admin UI all read back the same record.
 *
 * The store keeps only the latest failure and only bounded, sanitized metadata:
 * never API keys, request/response bodies, full stack traces, or unbounded logs.
 *
 * @package Flapjack\WordPress\Status
 */

declare(strict_types=1);

namespace Flapjack\WordPress\Status;

class FailureReporter {

    /**
     * Option holding the latest sanitized failure record. This is the durable
     * failure key removed by uninstall (see uninstall.php).
     */
    public const OPTION = 'flapjack_last_failure';

    /**
     * Upper bound on a stored, human-readable message so a verbose upstream
     * error can never grow the option without limit.
     */
    private const MAX_MESSAGE_LENGTH = 500;

    /**
     * Upper bound on short label fields (operation, source, class, index name).
     */
    private const MAX_LABEL_LENGTH = 200;

    /**
     * Placeholder substituted for any redacted secret-like token.
     */
    private const REDACTION = '[redacted]';

    /**
     * Record the latest failure, replacing any previously stored one.
     *
     * Callers pass the throwable plus their own responsibility-boundary context
     * so the stored record identifies what failed and where, without any caller
     * duplicating persistence or sanitization logic.
     *
     * @param \Throwable           $exception The failure being reported.
     * @param array<string, mixed> $context   operation, source, and optional
     *                                         post_id, index_name, status_code.
     */
    public static function record( \Throwable $exception, array $context ): void {
        $record = [
            'operation'       => self::sanitize_label( (string) ( $context['operation'] ?? 'unknown' ) ),
            'source'          => self::sanitize_label( (string) ( $context['source'] ?? 'unknown' ) ),
            'exception_class' => self::sanitize_label( get_class( $exception ) ),
            'message'         => self::sanitize_message( $exception->getMessage() ),
            'occurred_at'     => time(),
        ];

        if ( isset( $context['post_id'] ) && is_numeric( $context['post_id'] ) ) {
            $record['post_id'] = (int) $context['post_id'];
        }

        if ( ! empty( $context['index_name'] ) ) {
            $record['index_name'] = self::sanitize_label( (string) $context['index_name'] );
        }

        $status_code = self::extract_status_code( $exception, $context );
        if ( null !== $status_code ) {
            $record['status_code'] = $status_code;
        }

        update_option( self::OPTION, $record );
    }

    /**
     * Return the latest sanitized failure record, or null if none is stored.
     *
     * @return array<string, mixed>|null
     */
    public static function latest(): ?array {
        $record = get_option( self::OPTION, null );

        return is_array( $record ) ? $record : null;
    }

    /**
     * Remove the stored failure record (e.g. after a successful reindex or on
     * uninstall).
     */
    public static function clear(): void {
        delete_option( self::OPTION );
    }

    /**
     * Reduce an arbitrary upstream message to a bounded, tag-free, secret-free
     * single line safe to persist and later display.
     */
    private static function sanitize_message( string $message ): string {
        $message = wp_strip_all_tags( $message );
        $message = self::redact_secrets( $message );
        $message = trim( (string) preg_replace( '/\s+/', ' ', $message ) );

        if ( mb_strlen( $message ) > self::MAX_MESSAGE_LENGTH ) {
            $message = mb_substr( $message, 0, self::MAX_MESSAGE_LENGTH ) . '…';
        }

        return $message;
    }

    /**
     * Reduce a short label to a bounded, tag-free, secret-free value.
     */
    private static function sanitize_label( string $value ): string {
        $value = self::redact_secrets( wp_strip_all_tags( $value ) );
        $value = trim( (string) preg_replace( '/\s+/', ' ', $value ) );

        if ( mb_strlen( $value ) > self::MAX_LABEL_LENGTH ) {
            $value = mb_substr( $value, 0, self::MAX_LABEL_LENGTH );
        }

        return $value;
    }

    /**
     * Redact credential-shaped substrings so a stored error can never leak an
     * API key. Covers `key=value`/`key: value` pairs for sensitive key names and
     * standalone long alphanumeric tokens (e.g. 32-char Algolia admin keys).
     */
    private static function redact_secrets( string $text ): string {
        $sensitive_key = 'api[_-]?key|apikey|x-algolia-api-key|admin[_-]?key|access[_-]?key|token|secret|password|authorization';

        $text = (string) preg_replace(
            '/(authorization["\']?\s*[:=]\s*["\']?)(?:(?:basic|bearer|digest|token)\s+)?([^\s"\'&,;]+)/i',
            '$1' . self::REDACTION,
            $text
        );

        $text = (string) preg_replace(
            '/((?:' . $sensitive_key . ')["\']?\s*[:=]\s*["\']?)([^\s"\'&,;]+)/i',
            '$1' . self::REDACTION,
            $text
        );

        $text = (string) preg_replace( '/\b[A-Za-z0-9]{32,}\b/', self::REDACTION, $text );

        return $text;
    }

    /**
     * Resolve an HTTP-range status code from explicit context or the throwable's
     * code, returning null when neither is a plausible status.
     *
     * @param array<string, mixed> $context
     */
    private static function extract_status_code( \Throwable $exception, array $context ): ?int {
        if ( isset( $context['status_code'] ) && is_numeric( $context['status_code'] ) ) {
            $status = (int) $context['status_code'];
            return self::is_http_status( $status ) ? $status : null;
        }

        $code = $exception->getCode();
        if ( is_int( $code ) && self::is_http_status( $code ) ) {
            return $code;
        }

        return null;
    }

    private static function is_http_status( int $code ): bool {
        return $code >= 100 && $code < 600;
    }
}
