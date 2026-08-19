import Button from './Button.svelte';

export const primaryButtonStory = {
  name: 'Primary button',
  component: Button,
  props: { label: 'Continue' },
};

export const disabledButtonStory = {
  name: 'Disabled button',
  component: Button,
  props: { label: 'Unavailable', disabled: true },
};
