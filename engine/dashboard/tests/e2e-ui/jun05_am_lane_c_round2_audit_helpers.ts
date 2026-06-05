type ButtonEvidence = {
  ariaLabel: string | null;
  text: string;
  title: string | null;
};

type StatusOptionEvidence = {
  label: string;
  value: string;
};

export function selectDocumentActionFinding(buttons: readonly ButtonEvidence[]): string {
  const documentSpecificDelete = buttons.find((button) => (
    button.ariaLabel?.toLowerCase().startsWith('delete document ') ?? false
  ));

  if (documentSpecificDelete?.ariaLabel) {
    return `Delete action exposes a document-specific accessible name: ${documentSpecificDelete.ariaLabel}.`;
  }

  return 'Delete action is exposed as a generic icon-title action rather than a document-specific action name.';
}

export function selectEventStatusFinding(options: readonly StatusOptionEvidence[]): string {
  const errorStatusOption = options.find((option) => option.value === 'error');
  if (errorStatusOption) {
    return `Status filter uses the visible label ${errorStatusOption.label} while preserving the underlying error status value.`;
  }

  return 'Status filter is missing the error-status option in the observed UI.';
}

export function collectStableOverviewHeadings(
  headings: readonly string[],
  overviewIndexHeadings: readonly string[],
  baselineIndexHeadings: readonly string[],
): string[] {
  const overviewIndexHeadingSet = new Set(overviewIndexHeadings);
  const baselineIndexHeadingSet = new Set(baselineIndexHeadings);
  const nonIndexHeadings = headings.filter((heading) => !overviewIndexHeadingSet.has(heading));
  const stableIndexHeadings = overviewIndexHeadings.filter((heading) => baselineIndexHeadingSet.has(heading));

  return [...nonIndexHeadings, ...stableIndexHeadings];
}
