# Button

Use the native button for console actions. Callers provide its visible label and optional action,
submit type, disabled state, accessible label, and governed visual variant.

- `primary`: the single dominant action in the current screen or state.
- `secondary`: ordinary reversible actions such as retry or copy.
- `danger`: destructive intent that still requires the owning workflow's confirmation policy.

All variants consume semantic theme tokens, keep a minimum 44px target, expose a visible
`focus-visible` outline, and retain native keyboard activation. Disabled buttons remain visible but
cannot activate. `ariaLabel` may add object context to a repeated action while `label` stays concise;
it must never contain a secret or opaque platform identifier.
