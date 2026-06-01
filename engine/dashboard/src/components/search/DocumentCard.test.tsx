import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import type { DisplayPreferences } from '@/lib/types';
import { DocumentCard } from './DocumentCard';

vi.mock('@monaco-editor/react', () => ({
  default: ({ value }: { value: string }) => <pre data-testid="monaco-editor">{value}</pre>,
}));

const fullPreferences: DisplayPreferences = {
  titleAttribute: 'name',
  subtitleAttribute: 'description',
  imageAttribute: 'image_url',
  tagAttributes: ['tags'],
};

const baseDocument = {
  objectID: 'p01',
  name: 'MacBook Pro 16"',
  description: 'Apple M3 Max chip laptop',
  image_url: 'https://cdn.example.test/products/p01.jpg',
  brand: 'Apple',
  category: 'Laptops',
  price: 3499,
  rating: 4.8,
  inStock: true,
  tags: ['laptop', 'professional'],
};

describe('DocumentCard display preferences contract', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it('renders configured title/subtitle/image/tag fields and removes consumed rows', () => {
    render(
      <DocumentCard
        document={{
          ...baseDocument,
          _highlightResult: {
            name: { value: '<em>MacBook</em> Pro 16\"<script>alert(1)</script>', matchLevel: 'full' },
            description: { value: 'Apple <em>M3 Max</em> chip laptop', matchLevel: 'partial' },
            tags: [
              { value: '<em>laptop</em>', matchLevel: 'full' },
              { value: 'professional', matchLevel: 'none' },
            ],
          },
        }}
        displayPreferences={fullPreferences}
      />
    );

    const title = screen.getByTestId('document-card-title');
    const subtitle = screen.getByTestId('document-card-subtitle');
    expect(within(title).getByText('MacBook')).toBeInTheDocument();
    expect(title.querySelector('script')).not.toBeInTheDocument();
    expect(within(subtitle).getByText('M3 Max')).toBeInTheDocument();

    const image = screen.getByTestId('document-card-image') as HTMLImageElement;
    expect(image.src).toBe('https://cdn.example.test/products/p01.jpg');
    expect(image).toHaveAttribute('referrerpolicy', 'no-referrer');

    const highlightedTag = screen.getByText('laptop');
    expect(highlightedTag.tagName).toBe('EM');
    expect(screen.getByText('professional')).toBeInTheDocument();

    expect(screen.queryByText('name:')).not.toBeInTheDocument();
    expect(screen.queryByText('description:')).not.toBeInTheDocument();
    expect(screen.queryByText('image_url:')).not.toBeInTheDocument();
    expect(screen.queryByText('tags:')).not.toBeInTheDocument();

    expect(screen.getByText('brand:')).toBeInTheDocument();
    expect(screen.getByText('category:')).toBeInTheDocument();
    expect(screen.getByText('price:')).toBeInTheDocument();
    expect(screen.getByText('rating:')).toBeInTheDocument();
    expect(screen.getByText('inStock:')).toBeInTheDocument();
  });

  it('reuses the row highlight wrapper for configured tag badges and non-image header fallbacks', () => {
    render(
      <DocumentCard
        document={{
          ...baseDocument,
          brand: '',
          _highlightResult: {
            brand: { value: '<em>Apple</em>', matchLevel: 'full' },
            tags: [
              { value: '<em>laptop</em>', matchLevel: 'full' },
              { value: 'professional', matchLevel: 'none' },
            ],
          },
        }}
        displayPreferences={{
          titleAttribute: null,
          subtitleAttribute: null,
          imageAttribute: 'brand',
          tagAttributes: ['tags'],
        }}
      />
    );

    const configuredHeader = screen.getByTestId('document-card-configured-header');
    const configuredImageValue = within(configuredHeader).getByText('Apple').parentElement;
    expect(configuredImageValue).not.toBeNull();
    expect(configuredImageValue?.className).toContain('[&>em]:bg-yellow-200');

    const configuredTagValue = within(configuredHeader).getByText('laptop').parentElement;
    expect(configuredTagValue).not.toBeNull();
    expect(configuredTagValue?.className).toContain('[&>em]:bg-yellow-200');
  });

  it('does not inject non-http image schemes into the configured header image', () => {
    render(
      <DocumentCard
        document={{
          ...baseDocument,
          image_url: 'javascript:alert(1)',
        }}
        displayPreferences={fullPreferences}
      />
    );

    expect(screen.queryByTestId('document-card-image')).not.toBeInTheDocument();
    expect(screen.getByText('javascript:alert(1)')).toBeInTheDocument();
  });

  it('does not treat scheme-relative image URLs as same-origin paths', () => {
    render(
      <DocumentCard
        document={{
          ...baseDocument,
          image_url: '//cdn.attacker.test/track.png',
        }}
        displayPreferences={fullPreferences}
      />
    );

    expect(screen.queryByTestId('document-card-image')).not.toBeInTheDocument();
    expect(screen.getByText('//cdn.attacker.test/track.png')).toBeInTheDocument();
  });

  it('keeps default no-preferences fallback and preview-count behavior', async () => {
    const user = userEvent.setup();
    const { image_url: _ignoredImage, ...documentWithoutConfiguredFields } = baseDocument;

    render(
      <DocumentCard
        document={documentWithoutConfiguredFields}
        fieldOrder={['name', 'description', 'brand', 'category', 'price', 'rating', 'inStock', 'tags']}
      />
    );

    expect(screen.queryByTestId('document-card-title')).not.toBeInTheDocument();
    expect(screen.queryByTestId('document-card-subtitle')).not.toBeInTheDocument();
    expect(screen.queryByTestId('document-card-image')).not.toBeInTheDocument();

    expect(screen.getByText('name:')).toBeInTheDocument();
    expect(screen.getByText('description:')).toBeInTheDocument();
    expect(screen.getByText('brand:')).toBeInTheDocument();
    expect(screen.getByText('category:')).toBeInTheDocument();
    expect(screen.getByText('price:')).toBeInTheDocument();
    expect(screen.getByText('rating:')).toBeInTheDocument();

    expect(screen.queryByText('inStock:')).not.toBeInTheDocument();
    expect(screen.queryByText('tags:')).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: '+2 more fields' })).toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: '+2 more fields' }));
    expect(screen.getByText('inStock:')).toBeInTheDocument();
    expect(screen.getByText('tags:')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Show less' })).toBeInTheDocument();
  });

  it('applies preview count after consumed fields in partial preferences', () => {
    render(
      <DocumentCard
        document={baseDocument}
        displayPreferences={{
          titleAttribute: 'name',
          subtitleAttribute: null,
          imageAttribute: 'image_url',
          tagAttributes: ['tags'],
        }}
        fieldOrder={['name', 'description', 'brand', 'category', 'price', 'rating', 'inStock', 'tags']}
      />
    );

    expect(screen.queryByText('name:')).not.toBeInTheDocument();
    expect(screen.queryByText('tags:')).not.toBeInTheDocument();

    expect(screen.getByText('description:')).toBeInTheDocument();
    expect(screen.getByText('brand:')).toBeInTheDocument();
    expect(screen.getByText('category:')).toBeInTheDocument();
    expect(screen.getByText('price:')).toBeInTheDocument();
    expect(screen.getByText('rating:')).toBeInTheDocument();
    expect(screen.getByText('inStock:')).toBeInTheDocument();

    expect(screen.queryByRole('button', { name: /more fields/ })).not.toBeInTheDocument();
  });

  it('keeps matched brand visible without overriding parent remainder order', () => {
    render(
      <DocumentCard
        document={{
          ...baseDocument,
          warranty: '2 years',
          seller: 'Flapjack Store',
          condition: 'new',
          material: 'aluminum',
          _highlightResult: {
            brand: { value: '<em>Apple</em>', matchLevel: 'full' },
            seller: { value: 'Flapjack Store', matchLevel: 'none' },
            warranty: { value: '2 years', matchLevel: 'none' },
            condition: { value: 'new', matchLevel: 'none' },
            material: { value: 'aluminum', matchLevel: 'none' },
          },
        }}
        displayPreferences={{
          titleAttribute: 'name',
          subtitleAttribute: 'description',
          imageAttribute: 'image_url',
          tagAttributes: ['tags'],
        }}
        fieldOrder={[
          'name',
          'description',
          'image_url',
          'tags',
          'inStock',
          'seller',
          'warranty',
          'condition',
          'material',
          'category',
          'price',
          'rating',
          'brand',
        ]}
      />
    );

    // Brand is matched but appears late in the parent-provided order; collapsed preview
    // should still keep it visible ahead of lower-priority unmatched overflow fields.
    expect(screen.getByText('brand:')).toBeInTheDocument();
    expect(screen.getByText('inStock:')).toBeInTheDocument();
    expect(screen.getByText('seller:')).toBeInTheDocument();
    expect(screen.getByText('warranty:')).toBeInTheDocument();
    expect(screen.getByText('condition:')).toBeInTheDocument();
    expect(screen.getByText('material:')).toBeInTheDocument();

    expect(screen.queryByText('category:')).not.toBeInTheDocument();
    expect(screen.queryByText('price:')).not.toBeInTheDocument();
    expect(screen.queryByText('rating:')).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: '+3 more fields' })).toBeInTheDocument();
  });

  it('does not guarantee unmatched brand visibility in collapsed preview', () => {
    render(
      <DocumentCard
        document={{
          ...baseDocument,
          warranty: '2 years',
          seller: 'Flapjack Store',
          condition: 'new',
          material: 'aluminum',
          _highlightResult: {
            seller: { value: '<em>Flapjack Store</em>', matchLevel: 'full' },
            warranty: { value: '2 years', matchLevel: 'none' },
            condition: { value: 'new', matchLevel: 'none' },
            material: { value: 'aluminum', matchLevel: 'none' },
            brand: { value: 'Apple', matchLevel: 'none' },
          },
        }}
        displayPreferences={{
          titleAttribute: 'name',
          subtitleAttribute: 'description',
          imageAttribute: 'image_url',
          tagAttributes: ['tags'],
        }}
        fieldOrder={[
          'name',
          'description',
          'image_url',
          'tags',
          'inStock',
          'seller',
          'warranty',
          'condition',
          'material',
          'category',
          'price',
          'rating',
          'brand',
        ]}
      />
    );

    // Unmatched trailing fields are allowed to fall below collapsed preview cut-off.
    expect(screen.getByText('inStock:')).toBeInTheDocument();
    expect(screen.getByText('seller:')).toBeInTheDocument();
    expect(screen.getByText('warranty:')).toBeInTheDocument();
    expect(screen.getByText('condition:')).toBeInTheDocument();
    expect(screen.getByText('material:')).toBeInTheDocument();
    expect(screen.queryByText('brand:')).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: '+3 more fields' })).toBeInTheDocument();
  });

  it('preserves existing affordances with configured headers', async () => {
    const user = userEvent.setup();
    const onDelete = vi.fn();
    const writeText = vi.fn().mockResolvedValue(undefined);

    Object.defineProperty(navigator, 'clipboard', {
      configurable: true,
      value: { writeText },
    });

    render(
      <DocumentCard
        document={{
          ...baseDocument,
          _highlightResult: {
            brand: { value: '<em>Apple</em>', matchLevel: 'full' },
          },
        }}
        displayPreferences={fullPreferences}
        onDelete={onDelete}
      />
    );

    expect(screen.getByText('p01')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'JSON' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Copy' })).toBeInTheDocument();
    const brandRow = screen.getByText('brand:').closest('div');
    expect(brandRow).not.toBeNull();
    const highlightedBrand = within(brandRow as HTMLElement).getByText('Apple');
    expect(highlightedBrand.tagName).toBe('EM');

    await user.click(screen.getByRole('button', { name: 'Copy' }));
    expect(writeText).toHaveBeenCalledTimes(1);
    expect(writeText).toHaveBeenCalledWith(expect.stringContaining('"objectID": "p01"'));

    await user.click(screen.getByRole('button', { name: 'JSON' }));
    expect(screen.getByTestId('monaco-editor')).toBeInTheDocument();

    await user.click(screen.getByTitle('Delete document'));
    expect(onDelete).toHaveBeenCalledWith('p01');
    expect(screen.getByTestId('document-card-title')).toHaveTextContent('MacBook Pro 16"');
  });
});
