import '@testing-library/jest-dom/vitest';
import { render, screen } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';
import { Button } from '../src/lib/ui';
import * as buttonStories from '../src/lib/ui/Button.stories';

const { disabledButtonStory, primaryButtonStory } = buttonStories;

describe('Button public contract', () => {
  it('has an accessible name, receives Tab focus, and activates once with Enter', async () => {
    const user = userEvent.setup();
    const onpress = vi.fn();
    render(primaryButtonStory.component, { props: { ...primaryButtonStory.props, onpress } });

    const button = screen.getByRole('button', { name: primaryButtonStory.props.label });
    expect(primaryButtonStory.component).toBe(Button);
    await user.tab();
    expect(button).toHaveFocus();
    await user.keyboard('{Enter}');
    expect(onpress).toHaveBeenCalledOnce();
  });

  it('does not activate when disabled', async () => {
    const user = userEvent.setup();
    const onpress = vi.fn();
    render(disabledButtonStory.component, { props: { ...disabledButtonStory.props, onpress } });

    const button = screen.getByRole('button', { name: disabledButtonStory.props.label });
    expect(button).toBeDisabled();
    await user.click(button);
    expect(onpress).not.toHaveBeenCalled();
  });

  it('publishes governed primary, secondary, and danger variants', () => {
    expect(
      Object.values(buttonStories)
        .map((story) => story.props.variant)
        .filter(Boolean)
        .sort()
    ).toEqual(['danger', 'primary', 'secondary']);

    render(primaryButtonStory.component, { props: primaryButtonStory.props });
    expect(screen.getByRole('button', { name: primaryButtonStory.props.label })).toHaveAttribute(
      'data-variant',
      'primary'
    );
  });
});
