<script module lang="ts">
  export type ButtonVariant = 'primary' | 'secondary' | 'danger';
</script>

<script lang="ts">
  let {
    label,
    type = 'button',
    variant = 'secondary',
    disabled = false,
    ariaLabel,
    onpress,
  }: {
    label: string;
    type?: 'button' | 'submit';
    variant?: ButtonVariant;
    disabled?: boolean;
    ariaLabel?: string;
    onpress?: (event: MouseEvent) => void;
  } = $props();
</script>

<button
  class="button button_{variant}"
  data-variant={variant}
  {type}
  {disabled}
  aria-label={ariaLabel}
  onclick={onpress}>{label}</button
>

<style>
  .button {
    display: inline-flex;
    min-height: var(--console-control-min-height);
    align-items: center;
    justify-content: center;
    gap: var(--console-space-sm);
    border: var(--console-border-width) solid transparent;
    border-radius: var(--console-radius);
    padding: var(--console-space-sm) var(--console-space-md);
    font: inherit;
    font-weight: var(--console-control-font-weight);
    line-height: 1.25;
    text-align: center;
    cursor: pointer;
    transition:
      background-color var(--console-motion-duration) ease,
      border-color var(--console-motion-duration) ease,
      color var(--console-motion-duration) ease;
  }

  .button_primary {
    border-color: var(--console-accent);
    color: var(--console-on-accent);
    background: var(--console-accent);
  }

  .button_secondary {
    border-color: var(--console-border);
    color: var(--console-text);
    background: var(--console-surface-muted);
  }

  .button_danger {
    border-color: var(--console-danger);
    color: var(--console-danger);
    background: var(--console-danger-surface);
  }

  .button:focus-visible {
    outline: calc(var(--console-border-width) * 3) solid var(--console-focus);
    outline-offset: calc(var(--console-border-width) * 2);
  }

  .button:disabled {
    opacity: 0.55;
    cursor: not-allowed;
  }

  @media (hover: hover) {
    .button_primary:not(:disabled):hover {
      border-color: var(--console-accent-hover);
      background: var(--console-accent-hover);
    }

    .button_secondary:not(:disabled):hover {
      background: var(--console-surface-hover);
    }

    .button_danger:not(:disabled):hover {
      background: var(--console-danger-hover);
    }
  }
</style>
