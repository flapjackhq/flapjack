import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { AddDocumentsDialog } from '@/components/documents/AddDocumentsDialog';

const mockUseAddDocuments = vi.hoisted(() => vi.fn());
const mockUseIndexFields = vi.hoisted(() => vi.fn());

vi.mock('@/hooks/useDocuments', () => ({
  useAddDocuments: mockUseAddDocuments,
}));

vi.mock('@/hooks/useIndexFields', () => ({
  useIndexFields: mockUseIndexFields,
}));

vi.mock('@/components/documents/SampleDataTabContent', () => ({
  SampleDataTabContent: () => <div data-testid="sample-data-tab" />,
}));

describe('AddDocumentsDialog JSON editor', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mockUseAddDocuments.mockReturnValue({ mutateAsync: vi.fn(), isPending: false });
    mockUseIndexFields.mockReturnValue({ data: undefined });
  });

  it('labels the raw JSON editor so it is reachable by its accessible name', () => {
    render(<AddDocumentsDialog open onOpenChange={vi.fn()} indexName="products" />);

    const editor = screen.getByLabelText('Documents JSON');
    expect(editor.tagName).toBe('TEXTAREA');
    expect(editor).toHaveValue('');
  });

  it('records manual edits made through the labelled editor', async () => {
    const user = userEvent.setup();
    render(<AddDocumentsDialog open onOpenChange={vi.fn()} indexName="products" />);

    const editor = screen.getByLabelText('Documents JSON');
    // `[` and `]` are user-event keyboard descriptors, so type a brace-only document.
    await user.type(editor, '{{"objectID": "1"}');

    expect(editor).toHaveValue('{"objectID": "1"}');
  });
});
