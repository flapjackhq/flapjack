import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { MemoryRouter } from 'react-router-dom';
import { FacetsPanel } from '@/components/search/FacetsPanel';

const mockUseSearch = vi.hoisted(() => vi.fn());

vi.mock('@/hooks/useSearch', () => ({
  useSearch: mockUseSearch,
}));

vi.mock('@/hooks/useDevMode', () => ({
  useDevMode: (selector: (state: { log: () => void }) => unknown) => selector({ log: () => {} }),
}));

const facetResponse = {
  data: {
    facets: {
      brand: { Apple: 12, Sony: 4 },
    },
    nbHits: 16,
  },
  status: 'success',
  isFetching: false,
  isPlaceholderData: false,
};

function renderPanel(facetFilters?: string[], onParamsChange = vi.fn()) {
  render(
    <MemoryRouter>
      <FacetsPanel
        indexName="products"
        params={{ query: '', facetFilters }}
        onParamsChange={onParamsChange}
      />
    </MemoryRouter>
  );
  return onParamsChange;
}

describe('FacetsPanel facet value state', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mockUseSearch.mockReturnValue(facetResponse);
  });

  it('exposes each facet value as a checkbox reflecting its selected state', () => {
    renderPanel(['brand:Apple']);

    expect(screen.getByRole('checkbox', { name: /Apple/ })).toBeChecked();
    expect(screen.getByRole('checkbox', { name: /Sony/ })).not.toBeChecked();
  });

  it('shows every facet value unchecked when no filter is active', () => {
    renderPanel();

    expect(screen.getByRole('checkbox', { name: /Apple/ })).not.toBeChecked();
    expect(screen.getByRole('checkbox', { name: /Sony/ })).not.toBeChecked();
  });

  it('adds the facet filter when an unchecked facet value is clicked', async () => {
    const user = userEvent.setup();
    const onParamsChange = renderPanel(undefined, vi.fn());

    await user.click(screen.getByRole('checkbox', { name: /Sony/ }));

    expect(onParamsChange).toHaveBeenCalledWith({ facetFilters: ['brand:Sony'] });
  });
});
