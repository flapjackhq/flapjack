import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import type { DisplayPreferences } from '@/lib/types';
import { ResultsPanel } from '../ResultsPanel';

const mockUseSearch = vi.hoisted(() => vi.fn());
const mockUseDeleteDocument = vi.hoisted(() => vi.fn());
const mockUseDisplayPreferences = vi.hoisted(() => vi.fn());
const mockDocumentCardCalls = vi.hoisted(() => vi.fn());

vi.mock('@/hooks/useSearch', () => ({
  useSearch: mockUseSearch,
}));

vi.mock('@/hooks/useDocuments', () => ({
  useDeleteDocument: mockUseDeleteDocument,
}));

vi.mock('@/hooks/useDisplayPreferences', () => ({
  useDisplayPreferences: mockUseDisplayPreferences,
}));

vi.mock('@/components/search/DocumentCard', () => ({
  DocumentCard: (props: any) => {
    mockDocumentCardCalls(props);
    const objectID = props.document.objectID ?? 'no-object-id';

    return (
      <div data-testid={`document-card-${objectID}`}>
        <span>{objectID}</span>
        <button type="button" onClick={props.onClick}>open</button>
        <button type="button" onClick={() => props.onDelete?.(objectID)}>delete</button>
      </div>
    );
  },
}));

vi.mock('@/components/ui/confirm-dialog', () => ({
  ConfirmDialog: ({ open, onConfirm }: { open: boolean; onConfirm: () => void }) => (
    open ? <button data-testid="confirm-delete" type="button" onClick={onConfirm}>confirm</button> : null
  ),
}));

const defaultProps = {
  indexName: 'products',
  params: {
    query: 'laptop',
    page: 0,
    hitsPerPage: 20,
  },
  onParamsChange: vi.fn(),
};

const savedPreferences: DisplayPreferences = {
  titleAttribute: 'name',
  subtitleAttribute: 'description',
  imageAttribute: 'image_url',
  tagAttributes: ['tags'],
};

describe('ResultsPanel summary contract', () => {
  beforeEach(() => {
    vi.clearAllMocks();

    mockUseDeleteDocument.mockReturnValue({
      mutate: vi.fn(),
      isPending: false,
    });

    mockUseDisplayPreferences.mockReturnValue({
      preferences: savedPreferences,
    });
  });

  it('renders localized result counts with the plural label', () => {
    mockUseSearch.mockReturnValue({
      data: {
        hits: [{ objectID: 'doc-1', name: 'Laptop Pro' }],
        nbHits: 1234,
        nbPages: 62,
        processingTimeMS: 13,
        queryID: 'qid-1',
      },
      isLoading: false,
      error: null,
    });

    render(<ResultsPanel {...defaultProps} />);

    expect(screen.getByTestId('results-count')).toHaveTextContent('1,234');
    expect(screen.getByTestId('results-label')).toHaveTextContent('results');
    expect(screen.getByTestId('results-panel')).toHaveTextContent('1,234 results');
    expect(screen.getByTestId('document-card-doc-1')).toBeInTheDocument();
  });

  it('renders the singular result label when exactly one hit exists', () => {
    mockUseSearch.mockReturnValue({
      data: {
        hits: [{ objectID: 'doc-2', name: 'Single Result' }],
        nbHits: 1,
        nbPages: 1,
        processingTimeMS: 2,
        queryID: 'qid-2',
      },
      isLoading: false,
      error: null,
    });

    render(<ResultsPanel {...defaultProps} />);

    expect(screen.getByTestId('results-count')).toHaveTextContent('1');
    expect(screen.getByTestId('results-label')).toHaveTextContent('result');
  });

  it('reads preferences once and passes display-preferences + canonical fieldOrder into each card', async () => {
    const user = userEvent.setup();
    const onResultClick = vi.fn();
    const deleteMutate = vi.fn();

    mockUseDeleteDocument.mockReturnValue({
      mutate: deleteMutate,
      isPending: false,
    });

    mockUseSearch.mockReturnValue({
      data: {
        hits: [
          {
            objectID: 'doc-1',
            name: 'Laptop Pro',
            brand: 'Acme',
            _highlightResult: { name: { value: '<em>Laptop</em> Pro', matchLevel: 'full' } },
          },
          {
            objectID: 'doc-2',
            category: 'Laptops',
            brand: 'Acme',
          },
        ],
        nbHits: 2,
        nbPages: 1,
        processingTimeMS: 9,
        queryID: 'qid-seam',
      },
      isLoading: false,
      error: null,
    });

    render(
      <ResultsPanel
        {...defaultProps}
        params={{ query: 'laptop', page: 1, hitsPerPage: 10 }}
        onResultClick={onResultClick}
      />
    );

    expect(mockUseDisplayPreferences).toHaveBeenCalledWith('products');

    expect(mockDocumentCardCalls).toHaveBeenCalledTimes(2);
    expect(mockDocumentCardCalls.mock.calls[0][0].fieldOrder).toEqual(['name', 'brand', 'category']);
    expect(mockDocumentCardCalls.mock.calls[1][0].fieldOrder).toEqual(['name', 'brand', 'category']);
    expect(mockDocumentCardCalls.mock.calls[0][0].displayPreferences).toEqual(savedPreferences);
    expect(mockDocumentCardCalls.mock.calls[1][0].displayPreferences).toEqual(savedPreferences);

    await user.click(within(screen.getByTestId('document-card-doc-2')).getByRole('button', { name: 'open' }));
    expect(onResultClick).toHaveBeenCalledWith('doc-2', 12, 'qid-seam');

    await user.click(within(screen.getByTestId('document-card-doc-1')).getByRole('button', { name: 'delete' }));
    await user.click(screen.getByTestId('confirm-delete'));
    expect(deleteMutate).toHaveBeenCalledWith('doc-1', expect.any(Object));
  });
});

describe('ResultsPanel pagination contract', () => {
  beforeEach(() => {
    vi.clearAllMocks();

    mockUseDeleteDocument.mockReturnValue({
      mutate: vi.fn(),
      isPending: false,
    });

    mockUseDisplayPreferences.mockReturnValue({
      preferences: savedPreferences,
    });

    mockUseSearch.mockReturnValue({
      data: {
        hits: [{ objectID: 'doc-1', name: 'Laptop Pro' }],
        nbHits: 60,
        nbPages: 3,
        processingTimeMS: 4,
        queryID: 'qid-page',
      },
      isLoading: false,
      error: null,
    });
  });

  it('gives the icon-only page buttons accessible names and the correct disabled state', () => {
    render(<ResultsPanel {...defaultProps} />);

    const previous = screen.getByRole('button', { name: 'Previous page' });
    const next = screen.getByRole('button', { name: 'Next page' });

    expect(previous).toBeDisabled();
    expect(next).toBeEnabled();
    expect(screen.getByTestId('pagination-controls')).toHaveTextContent('Page 1 of 3');
  });

  it('advances to the next page when the named next button is clicked', async () => {
    const user = userEvent.setup();
    const onParamsChange = vi.fn();

    render(<ResultsPanel {...defaultProps} onParamsChange={onParamsChange} />);
    await user.click(screen.getByRole('button', { name: 'Next page' }));

    expect(onParamsChange).toHaveBeenCalledWith({ page: 1 });
  });
});
