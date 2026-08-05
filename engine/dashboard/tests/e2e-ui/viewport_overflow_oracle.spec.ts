import { test, expect } from '@playwright/test';
import {
  describeHorizontalOverflow,
  isHorizontallyUnusable,
  summarizeHorizontalOverflow,
} from '../fixtures/viewport_overflow';

/**
 * Known-answer self-test for the 390px route-audit oracle.
 *
 * `stage3_route_audit_390.spec.ts` is the closing proof for the shared-shell fix, so its
 * measurement has to be provably able to fail for a real defect and provably unable to
 * fail for legitimate layout. Each case below renders a synthetic specimen with a
 * hand-calculated expected verdict; the clipped-overflow case is the one that a
 * document-`scrollWidth`-only oracle silently passes.
 */

const VIEWPORT_WIDTH = 390;
const VIEWPORT_HEIGHT = 844;

function specimen(body: string): string {
  return `<!doctype html><html><head><style>
    * { box-sizing: border-box; }
    body { margin: 0; padding: 0; }
  </style></head><body>${body}</body></html>`;
}

test.describe('390px overflow oracle', () => {
  test.use({ viewport: { width: VIEWPORT_WIDTH, height: VIEWPORT_HEIGHT } });

  test('reports a contained layout as usable', async ({ page }) => {
    await page.setContent(specimen(
      '<div data-testid="contained" style="width:390px;height:40px;background:#eee"></div>',
    ));

    const report = await describeHorizontalOverflow(page);

    expect(report.viewportWidth).toBe(VIEWPORT_WIDTH);
    expect(report.documentWidth).toBe(VIEWPORT_WIDTH);
    expect(report.documentOverflow).toBe(false);
    expect(report.culpritCount).toBe(0);
    expect(report.culprits).toEqual([]);
    expect(isHorizontallyUnusable(report)).toBe(false);
    expect(summarizeHorizontalOverflow(report)).toBe('');
  });

  test('reports document-widening overflow with the offending element', async ({ page }) => {
    await page.setContent(specimen(
      '<div data-testid="wide-block" style="width:800px;height:40px;background:#eee"></div>',
    ));

    const report = await describeHorizontalOverflow(page);

    expect(report.documentWidth).toBe(800);
    expect(report.documentOverflow).toBe(true);
    expect(report.culpritCount).toBe(1);
    expect(report.culprits).toEqual([
      { selector: '[data-testid="wide-block"]', right: 800, width: 800, clippedByAncestor: false },
    ]);
    expect(isHorizontallyUnusable(report)).toBe(true);
    expect(summarizeHorizontalOverflow(report)).toContain('[data-testid="wide-block"] right=800px');
  });

  test('still reports overflow that an ancestor clips out of the document width', async ({ page }) => {
    await page.setContent(specimen(
      '<div data-testid="clipping-shell" style="width:390px;overflow-x:hidden">'
      + '<div data-testid="clipped-content" style="width:800px;height:40px;background:#eee"></div>'
      + '</div>',
    ));

    const report = await describeHorizontalOverflow(page);

    // The clip keeps the document at viewport width — a scrollWidth-only oracle passes here.
    expect(report.documentWidth).toBe(VIEWPORT_WIDTH);
    expect(report.documentOverflow).toBe(false);
    expect(report.culpritCount).toBe(1);
    expect(report.culprits).toEqual([
      { selector: '[data-testid="clipped-content"]', right: 800, width: 800, clippedByAncestor: true },
    ]);
    expect(isHorizontallyUnusable(report)).toBe(true);
    expect(summarizeHorizontalOverflow(report)).toContain('overflow is clipped, not scrollable');
  });

  test('accepts wide content inside a horizontal scroll container', async ({ page }) => {
    await page.setContent(specimen(
      '<div data-testid="scroll-shell" style="width:390px;overflow-x:auto">'
      + '<div data-testid="scrollable-content" style="width:800px;height:40px;background:#eee"></div>'
      + '</div>',
    ));

    const report = await describeHorizontalOverflow(page);

    expect(report.documentWidth).toBe(VIEWPORT_WIDTH);
    expect(report.documentOverflow).toBe(false);
    expect(report.culpritCount).toBe(0);
    expect(isHorizontallyUnusable(report)).toBe(false);
  });

  test('accepts inline text truncated by an ellipsis clip', async ({ page }) => {
    await page.setContent(specimen(
      '<div data-testid="truncating-label" style="margin-left:180px;width:200px;overflow:hidden;'
      + 'text-overflow:ellipsis;white-space:nowrap;font-size:16px">'
      + '<span data-testid="truncated-text">'
      + 'an extremely long index name that cannot possibly fit inside two hundred pixels of label'
      + '</span></div>',
    ));

    const report = await describeHorizontalOverflow(page);

    expect(report.documentWidth).toBe(VIEWPORT_WIDTH);
    expect(report.documentOverflow).toBe(false);
    expect(report.culpritCount).toBe(0);
    expect(isHorizontallyUnusable(report)).toBe(false);
  });

  test('accepts nested inline-only text truncated by an ellipsis clip', async ({ page }) => {
    await page.setContent(specimen(
      '<div data-testid="nested-truncating-label" style="margin-left:180px;width:200px;'
      + 'overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font-size:16px">'
      + '<span data-testid="nested-inline-text"><strong>'
      + 'an extremely long nested index name that cannot fit inside the label'
      + '</strong></span></div>',
    ));

    const report = await describeHorizontalOverflow(page);

    expect(report.documentWidth).toBe(VIEWPORT_WIDTH);
    expect(report.documentOverflow).toBe(false);
    expect(report.culpritCount).toBe(0);
    expect(report.culprits).toEqual([]);
    expect(isHorizontallyUnusable(report)).toBe(false);
  });

  test('reports an atomic inline-flex child clipped by an ellipsis ancestor', async ({ page }) => {
    await page.setContent(specimen(
      '<div data-testid="atomic-truncate-shell" style="width:390px;overflow:hidden;'
      + 'text-overflow:ellipsis;white-space:nowrap">'
      + '<span data-testid="clipped-inline-flex" style="display:inline-flex;width:800px;'
      + 'height:40px;background:#eee"></span>'
      + '</div>',
    ));

    const report = await describeHorizontalOverflow(page);

    expect(report.documentWidth).toBe(VIEWPORT_WIDTH);
    expect(report.documentOverflow).toBe(false);
    expect(report.culpritCount).toBe(1);
    expect(report.culprits).toEqual([
      {
        selector: '[data-testid="clipped-inline-flex"]',
        right: 800,
        width: 800,
        clippedByAncestor: true,
      },
    ]);
    expect(isHorizontallyUnusable(report)).toBe(true);
  });

  test('reports a block child clipped by a truncate-style ancestor', async ({ page }) => {
    await page.setContent(specimen(
      '<div data-testid="truncate-shell" style="width:390px;overflow:hidden;'
      + 'text-overflow:ellipsis;white-space:nowrap">'
      + '<div data-testid="clipped-flex-child" style="display:flex;width:800px;height:40px;'
      + 'background:#eee"></div>'
      + '</div>',
    ));

    const report = await describeHorizontalOverflow(page);

    expect(report.documentWidth).toBe(VIEWPORT_WIDTH);
    expect(report.documentOverflow).toBe(false);
    expect(report.culpritCount).toBe(1);
    expect(report.culprits).toEqual([
      {
        selector: '[data-testid="clipped-flex-child"]',
        right: 800,
        width: 800,
        clippedByAncestor: true,
      },
    ]);
    expect(isHorizontallyUnusable(report)).toBe(true);
  });

  test('reports a visible overflowing closed-state trigger', async ({ page }) => {
    await page.setContent(specimen(
      '<button data-testid="closed-state-trigger" data-state="closed" '
      + 'style="display:block;width:800px;height:40px">Visible trigger</button>',
    ));

    const report = await describeHorizontalOverflow(page);

    expect(report.documentWidth).toBe(800);
    expect(report.documentOverflow).toBe(true);
    expect(report.culpritCount).toBe(1);
    expect(report.culprits).toEqual([
      {
        selector: '[data-testid="closed-state-trigger"]',
        right: 800,
        width: 800,
        clippedByAncestor: false,
      },
    ]);
    expect(isHorizontallyUnusable(report)).toBe(true);
  });

  test('ignores off-canvas content hidden from the accessibility tree', async ({ page }) => {
    await page.setContent(specimen(
      '<div data-testid="closed-drawer" aria-hidden="true" style="position:absolute;left:390px;top:0;'
      + 'width:300px;height:200px;background:#eee">'
      + '<div data-testid="drawer-body" style="width:280px;height:40px"></div>'
      + '</div>',
    ));

    const report = await describeHorizontalOverflow(page);

    // The hidden drawer widens raw documentElement.scrollWidth to 690px, but it is
    // off-canvas and out of the accessibility tree, so the oracle must report the
    // document as contained — otherwise a legitimate collapsed drawer fails the audit.
    expect(report.documentWidth).toBe(VIEWPORT_WIDTH);
    expect(report.documentOverflow).toBe(false);
    expect(report.culpritCount).toBe(0);
    expect(report.culprits).toEqual([]);
    expect(isHorizontallyUnusable(report)).toBe(false);
    expect(summarizeHorizontalOverflow(report)).toBe('');
  });

  test('reports only the outermost offender of a nested overflow', async ({ page }) => {
    await page.setContent(specimen(
      '<div data-testid="wide-parent" style="width:800px">'
      + '<div data-testid="wide-child" style="width:700px;height:40px;background:#eee"></div>'
      + '</div>',
    ));

    const report = await describeHorizontalOverflow(page);

    expect(report.documentOverflow).toBe(true);
    expect(report.culpritCount).toBe(1);
    expect(report.culprits.map((culprit) => culprit.selector)).toEqual([
      '[data-testid="wide-parent"]',
    ]);
  });
});
