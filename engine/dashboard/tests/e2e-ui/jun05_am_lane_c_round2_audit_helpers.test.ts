import { describe, expect, it } from 'vitest';
import {
  collectStableOverviewHeadings,
  selectEventStatusFinding,
  selectDocumentActionFinding,
} from './jun05_am_lane_c_round2_audit_helpers';

describe('round 2 route audit helpers', () => {
  it('derives the document action finding from observed delete button labels', () => {
    expect(
      selectDocumentActionFinding([
        { ariaLabel: null, text: 'JSON', title: 'Toggle JSON view' },
        { ariaLabel: 'delete document movie_050', text: '', title: 'Delete document' },
      ]),
    ).toBe('Delete action exposes a document-specific accessible name: delete document movie_050.');
  });

  it('keeps the generic delete finding when no document-specific label is observed', () => {
    expect(
      selectDocumentActionFinding([
        { ariaLabel: null, text: '', title: 'Delete document' },
      ]),
    ).toBe('Delete action is exposed as a generic icon-title action rather than a document-specific action name.');
  });

  it('describes the Event Debugger status filter with the current Failed label', () => {
    expect(
      selectEventStatusFinding([
        { label: 'All', value: '' },
        { label: 'OK', value: 'ok' },
        { label: 'Failed', value: 'error' },
      ]),
    ).toBe('Status filter uses the visible label Failed while preserving the underlying error status value.');
  });

  it('keeps only baseline index headings in overview evidence', () => {
    expect(
      collectStableOverviewHeadings([
        'Overview',
        'Indexes',
        'Documents',
        'Storage',
        'Status',
        'Search Analytics (Last 7 Days)',
        'Indexes',
        'e2e-products',
        'e2e-vector-settings-1780655145872-8irkcb',
        'movies',
        'nonexistent-index',
        'qs-delete-1780655140867',
      ], [
        'e2e-products',
        'e2e-vector-settings-1780655145872-8irkcb',
        'movies',
        'nonexistent-index',
        'qs-delete-1780655140867',
      ], ['movies']),
    ).toEqual([
      'Overview',
      'Indexes',
      'Documents',
      'Storage',
      'Status',
      'Search Analytics (Last 7 Days)',
      'Indexes',
      'movies',
    ]);
  });
});
