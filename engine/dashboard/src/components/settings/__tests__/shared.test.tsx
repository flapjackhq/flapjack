import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';
import { FieldChips } from '@/components/settings/shared';
import type { FieldInfo } from '@/hooks/useIndexFields';

const fields: FieldInfo[] = [
  { name: 'title', type: 'text' },
  { name: 'brand', type: 'text' },
];

describe('FieldChips', () => {
  it('exposes selection state through aria-pressed on each chip', () => {
    render(
      <FieldChips availableFields={fields} selectedValues={['title']} onToggle={vi.fn()} />
    );

    expect(screen.getByTestId('attr-chip-title')).toHaveAttribute('aria-pressed', 'true');
    expect(screen.getByTestId('attr-chip-brand')).toHaveAttribute('aria-pressed', 'false');
  });

  it('toggles the field whose chip is pressed', async () => {
    const user = userEvent.setup();
    const onToggle = vi.fn();

    render(
      <FieldChips availableFields={fields} selectedValues={['title']} onToggle={onToggle} />
    );
    await user.click(screen.getByTestId('attr-chip-brand'));

    expect(onToggle).toHaveBeenCalledWith('brand');
  });

  it('renders nothing when there are no available fields', () => {
    const { container } = render(
      <FieldChips availableFields={[]} selectedValues={[]} onToggle={vi.fn()} />
    );

    expect(container).toBeEmptyDOMElement();
  });
});
