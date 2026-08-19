import '@testing-library/jest-dom/vitest';
import { fireEvent, render, screen, within } from '@testing-library/svelte';
import { beforeAll, describe, expect, it, vi } from 'vitest';
import EngineSecuritySources from '../src/host/EngineSecuritySources.svelte';
import type {
  ConsoleTransport,
  EngineSecuritySourcesCapability,
  SecuritySource,
} from '../src/lib/transport/console_transport';

beforeAll(() => {
  if (!HTMLDialogElement.prototype.showModal) {
    HTMLDialogElement.prototype.showModal = function showModal() {
      this.open = true;
    };
  }
  if (!HTMLDialogElement.prototype.close) {
    HTMLDialogElement.prototype.close = function close() {
      this.open = false;
    };
  }
});

const loopback: SecuritySource = {
  source: '127.0.0.1/32',
  description: 'Local console',
};

function fixture(initialSources: SecuritySource[] = [loopback]) {
  let sources = initialSources;
  const securitySources: EngineSecuritySourcesCapability = {
    kind: 'engine',
    list: vi.fn(async () => sources),
    append: vi.fn(async (entry) => {
      sources = [...sources, entry];
    }),
    remove: vi.fn(async (source) => {
      sources = sources.filter((entry) => entry.source !== source);
    }),
  };
  const transport: ConsoleTransport = {
    securitySources,
    listIndexes: vi.fn(async () => []),
    searchIndex: vi.fn(async () => {
      throw new Error('not used');
    }),
  };
  return { securitySources, transport };
}

describe('standalone engine Security Sources composition', () => {
  it('loads the engine-global allowlist without mutating and renders customer text safely', async () => {
    const malicious: SecuritySource = {
      source: '127.0.0.0/8',
      description: '<img src=x onerror=alert(1)>',
    };
    const { securitySources, transport } = fixture([loopback, malicious]);

    const { container } = render(EngineSecuritySources, { props: { transport } });

    expect(await screen.findByRole('heading', { name: 'Security Sources' })).toBeVisible();
    expect(screen.getByText('2 entries')).toBeVisible();
    expect(screen.getByRole('article', { name: '127.0.0.1/32' })).toHaveTextContent(
      'Local console'
    );
    expect(screen.getByRole('article', { name: '127.0.0.0/8' })).toHaveTextContent(
      '<img src=x onerror=alert(1)>'
    );
    expect(container.querySelector('img')).toBeNull();
    expect(securitySources.list).toHaveBeenCalledOnce();
    expect(securitySources.append).not.toHaveBeenCalled();
    expect(securitySources.remove).not.toHaveBeenCalled();
  });

  it('keeps load failure distinct from empty and retries exactly once', async () => {
    const { securitySources, transport } = fixture([]);
    vi.mocked(securitySources.list)
      .mockRejectedValueOnce(new Error('private backend detail'))
      .mockResolvedValueOnce([]);
    render(EngineSecuritySources, { props: { transport } });

    expect(await screen.findByRole('alert')).toHaveTextContent('Could not load security sources.');
    expect(screen.queryByText('No security sources configured yet.')).not.toBeInTheDocument();
    expect(document.body).not.toHaveTextContent('private backend detail');

    await fireEvent.click(screen.getByRole('button', { name: 'Retry' }));
    expect(await screen.findByText('No security sources configured yet.')).toBeVisible();
    expect(securitySources.list).toHaveBeenCalledTimes(2);
  });

  it('validates Add Source, retains values on failure, locks pending dismissal, and restores focus', async () => {
    const { securitySources, transport } = fixture([]);
    let rejectAppend: (error: Error) => void = () => undefined;
    vi.mocked(securitySources.append).mockImplementationOnce(
      () =>
        new Promise<void>((_resolve, reject) => {
          rejectAppend = reject;
        })
    );
    render(EngineSecuritySources, { props: { transport } });
    await screen.findByText('No security sources configured yet.');

    const addTrigger = screen.getByRole('button', { name: 'Add Source' });
    addTrigger.focus();
    await fireEvent.click(addTrigger);
    let dialog = screen.getByRole('dialog', { name: 'Add security source' });
    const sourceInput = within(dialog).getByLabelText('Source');
    const descriptionInput = within(dialog).getByLabelText('Description');
    expect(sourceInput).toHaveFocus();

    await fireEvent.input(sourceInput, { target: { value: '   ' } });
    await fireEvent.click(within(dialog).getByRole('button', { name: 'Add source' }));
    expect(within(dialog).getByRole('alert')).toHaveTextContent('Source is required.');
    expect(securitySources.append).not.toHaveBeenCalled();

    await fireEvent.input(sourceInput, { target: { value: ' 127.0.0.0/8 ' } });
    await fireEvent.input(descriptionInput, { target: { value: ' Local proxy ' } });
    await fireEvent.click(within(dialog).getByRole('button', { name: 'Add source' }));
    expect(securitySources.append).toHaveBeenCalledExactlyOnceWith({
      source: '127.0.0.0/8',
      description: 'Local proxy',
    });
    expect(within(dialog).getByRole('button', { name: 'Adding…' })).toBeDisabled();
    expect(sourceInput).toBeDisabled();
    expect(descriptionInput).toBeDisabled();
    await fireEvent(dialog, new Event('cancel', { cancelable: true }));
    expect(screen.getByRole('dialog', { name: 'Add security source' })).toBeVisible();

    rejectAppend(new Error('Invalid CIDR: secret detail'));
    dialog = await screen.findByRole('dialog', { name: 'Add security source' });
    expect(within(dialog).getByRole('alert')).toHaveTextContent('Could not add security source.');
    expect(document.body).not.toHaveTextContent('secret detail');
    expect(within(dialog).getByLabelText('Source')).toHaveValue(' 127.0.0.0/8 ');
    expect(within(dialog).getByLabelText('Description')).toHaveValue(' Local proxy ');
    expect(within(dialog).getByLabelText('Source')).toBeEnabled();
    expect(within(dialog).getByLabelText('Description')).toBeEnabled();

    await fireEvent.click(within(dialog).getByRole('button', { name: 'Cancel' }));
    expect(screen.queryByRole('dialog', { name: 'Add security source' })).not.toBeInTheDocument();
    expect(addTrigger).toHaveFocus();
  });

  it('adds successfully, refreshes exact state, and returns focus to Add Source', async () => {
    const { securitySources, transport } = fixture([]);
    render(EngineSecuritySources, { props: { transport } });
    await screen.findByText('No security sources configured yet.');

    const addTrigger = screen.getByRole('button', { name: 'Add Source' });
    addTrigger.focus();
    await fireEvent.click(addTrigger);
    const dialog = screen.getByRole('dialog', { name: 'Add security source' });
    await fireEvent.input(within(dialog).getByLabelText('Source'), {
      target: { value: '127.0.0.0/8' },
    });
    await fireEvent.click(within(dialog).getByRole('button', { name: 'Add source' }));

    expect(await screen.findByRole('article', { name: '127.0.0.0/8' })).toHaveTextContent(
      'No description'
    );
    expect(securitySources.append).toHaveBeenCalledOnce();
    expect(securitySources.list).toHaveBeenCalledTimes(2);
    expect(addTrigger).toHaveFocus();
    expect(screen.getByRole('status')).toHaveTextContent('Security source added.');
  });

  it('deletes on one click only after success and keeps a failed target focused', async () => {
    const { securitySources, transport } = fixture([loopback]);
    let rejectRemoval: (error: Error) => void = () => undefined;
    vi.mocked(securitySources.remove).mockImplementationOnce(
      () =>
        new Promise<void>((_resolve, reject) => {
          rejectRemoval = reject;
        })
    );
    render(EngineSecuritySources, { props: { transport } });
    const row = await screen.findByRole('article', { name: '127.0.0.1/32' });
    const deleteButton = within(row).getByRole('button', {
      name: 'Delete security source 127.0.0.1/32',
    });
    deleteButton.focus();
    await fireEvent.click(deleteButton);

    expect(securitySources.remove).toHaveBeenCalledExactlyOnceWith('127.0.0.1/32');
    expect(deleteButton).toBeDisabled();
    expect(row).toBeVisible();
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();

    rejectRemoval(new Error('private delete detail'));
    expect(await within(row).findByRole('alert')).toHaveTextContent(
      'Could not delete security source.'
    );
    expect(document.body).not.toHaveTextContent('private delete detail');
    expect(deleteButton).toBeEnabled();
    expect(deleteButton).toHaveFocus();
    expect(row).toBeVisible();

    vi.mocked(securitySources.remove).mockImplementationOnce(async (source) => {
      expect(source).toBe('127.0.0.1/32');
      // Mirror the fixture's successful server mutation before the component reloads.
      vi.mocked(securitySources.list).mockResolvedValueOnce([]);
    });
    await fireEvent.click(deleteButton);

    expect(await screen.findByText('No security sources configured yet.')).toBeVisible();
    expect(screen.queryByRole('article', { name: '127.0.0.1/32' })).not.toBeInTheDocument();
    expect(screen.getByRole('region', { name: 'Security Sources screen' })).toHaveFocus();
    expect(screen.getByRole('status')).toHaveTextContent('Security source deleted.');
  });
});
