import Button from './Button.svelte';

export const primaryButtonStory = {
  name: 'Primary button',
  component: Button,
  props: { label: 'Continue', variant: 'primary' as const },
};

export const secondaryButtonStory = {
  name: 'Secondary button',
  component: Button,
  props: { label: 'Copy', variant: 'secondary' as const },
};

export const dangerButtonStory = {
  name: 'Danger button',
  component: Button,
  props: { label: 'Revoke', variant: 'danger' as const },
};

export const disabledButtonStory = {
  name: 'Disabled button',
  component: Button,
  props: { label: 'Unavailable', disabled: true },
};
