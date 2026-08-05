import type { Page } from '@playwright/test';

/**
 * Rendered-width measurement for the 390px route audit.
 *
 * Document-level `scrollWidth > innerWidth` on its own is a maskable oracle in two
 * directions. It over-reports: off-canvas content parked in a presentationally hidden
 * subtree (a collapsed `aria-hidden` drawer) inflates raw `scrollWidth` even though it is
 * legitimately off-screen, which would fail a contained shell. It also under-reports: any
 * ancestor with `overflow-x: hidden` (or `clip`) absorbs a descendant's overflow, so a
 * shell "fix" that clips wide content reports a narrow document while the content itself
 * sits off-screen and unreachable. The audit therefore derives the document width from the
 * widest *visible, unclipped, unhidden* element edge — not raw `scrollWidth` — AND flags
 * every element whose rendered box escapes the viewport's right edge, so neither hidden
 * content nor clipping can distort the audit.
 *
 * Two escapes are legitimate and are not reported:
 * - content inside a horizontal scroll container (`overflow-x: auto | scroll`), which
 *   the user can reach by scrolling that container;
 * - inline content truncated by a `text-overflow: ellipsis` clip, which is intended
 *   truncation rather than lost layout.
 *
 * Measurement boundary: the dashboard renders its page body inside a horizontally
 * scrollable `<main class="overflow-auto">`. Elements below that container are reachable
 * by scrolling and are therefore exempt from element-level culprit detection; the app's
 * own container also prevents them from widening the document. This oracle consequently
 * proves shared shell chrome containment, not page-body adaptation at 390px.
 *
 * Two further boundaries limit what a green audit proves, both deliberate:
 * - Elements removed from the rendered layout (`display: none`, `visibility: hidden`,
 *   `hidden`, `inert`, `aria-hidden="true"`) are exempt from both the element-level
 *   culprit scan and the document-width signal, because off-canvas menus and collapsed
 *   panels legitimately park outside the viewport. A responsive fix that hides shell
 *   controls at 390px therefore reports green: the audit proves that whatever the shell
 *   still renders is contained, not that every desktop control survived.
 * - Only the viewport's right edge is measured. Content pushed past the left edge
 *   (negative margins, `-translate-x`) is not reported, since that is also how off-canvas
 *   drawers are normally parked and flagging it would red-light a legitimate mobile shell.
 */

/** Sub-pixel layout rounding tolerance, in CSS pixels. */
const OVERFLOW_TOLERANCE_PX = 1;

/** Cap on reported culprits so evidence rows stay readable; `culpritCount` keeps the true total. */
const MAX_REPORTED_CULPRITS = 10;

export type ViewportOverflowCulprit = {
  /** Best available identifier for the offending element (`data-testid` preferred). */
  selector: string;
  /** Rendered right edge in CSS pixels, relative to the viewport's left edge. */
  right: number;
  /** Rendered width in CSS pixels. */
  width: number;
  /** True when an ancestor clips this element, i.e. the overflow is hidden rather than scrollable. */
  clippedByAncestor: boolean;
};

export type HorizontalOverflowReport = {
  viewportWidth: number;
  documentWidth: number;
  documentOverflow: boolean;
  /** Total number of outermost offending elements found (may exceed `culprits.length`). */
  culpritCount: number;
  /** Outermost offenders only, capped at {@link MAX_REPORTED_CULPRITS}. */
  culprits: ViewportOverflowCulprit[];
};

export async function describeHorizontalOverflow(
  page: Page,
  tolerancePx: number = OVERFLOW_TOLERANCE_PX,
  maxReportedCulprits: number = MAX_REPORTED_CULPRITS,
): Promise<HorizontalOverflowReport> {
  return page.evaluate(
    ({ tolerance, maxCulprits }) => {
      const viewportWidth = window.innerWidth;

      const isHorizontalScroller = (element: Element): boolean => {
        const overflowX = window.getComputedStyle(element).overflowX;
        return overflowX === 'auto' || overflowX === 'scroll';
      };

      const isClipping = (element: Element): boolean => {
        const overflowX = window.getComputedStyle(element).overflowX;
        return overflowX === 'hidden' || overflowX === 'clip';
      };

      const truncatesWithEllipsis = (element: Element): boolean =>
        window.getComputedStyle(element).textOverflow === 'ellipsis';

      const isInlineContentTruncatedBy = (element: Element, ancestor: Element): boolean => {
        if (!truncatesWithEllipsis(ancestor)) {
          return false;
        }

        let inlineBox: Element | null = element;
        while (inlineBox && inlineBox !== ancestor) {
          // Atomic inline-level boxes (inline-block/flex/grid) own layout and can hide
          // unreachable controls. Only ordinary inline text boxes inherit the owner's
          // legitimate ellipsis exemption.
          if (window.getComputedStyle(inlineBox).display !== 'inline') {
            return false;
          }
          inlineBox = inlineBox.parentElement;
        }

        return inlineBox === ancestor;
      };

      const isPresentationallyHidden = (element: Element): boolean => {
        if (element.hasAttribute('hidden') || element.hasAttribute('inert')) {
          return true;
        }
        if (element.getAttribute('aria-hidden') === 'true') {
          return true;
        }
        const style = window.getComputedStyle(element);
        return style.visibility === 'hidden' || style.display === 'none';
      };

      const describeElement = (element: Element): string => {
        const testId = element.getAttribute('data-testid');
        if (testId) {
          return `[data-testid="${testId}"]`;
        }
        if (element.id) {
          return `#${element.id}`;
        }
        const classes = Array.from(element.classList).slice(0, 3).join('.');
        const tagName = element.tagName.toLowerCase();
        return classes ? `${tagName}.${classes}` : tagName;
      };

      const culprits: {
        selector: string;
        right: number;
        width: number;
        clippedByAncestor: boolean;
      }[] = [];
      const flagged = new Set<Element>();

      // Widest visible, unclipped content edge — the effective document width.
      // Starts at the viewport so a contained page reports exactly viewportWidth.
      let visibleDocumentRight = viewportWidth;

      for (const element of Array.from(document.body.querySelectorAll('*'))) {
        const rect = element.getBoundingClientRect();
        if (rect.width <= 0 || rect.height <= 0) {
          continue;
        }
        if (rect.right <= viewportWidth + tolerance) {
          continue;
        }
        if (isPresentationallyHidden(element)) {
          continue;
        }

        // An element extends the document's scrollable width past the viewport only
        // when no ancestor hides it, clips it (overflow-x: hidden|clip), or scrolls it
        // (overflow-x: auto|scroll). This mirrors how the browser propagates
        // scrollWidth, and — unlike raw documentElement.scrollWidth — exempts
        // off-canvas content parked in a presentationally hidden subtree so a
        // legitimate collapsed drawer cannot inflate the document-width signal.
        let extendsDocument = true;
        for (let box = element.parentElement; box; box = box.parentElement) {
          if (
            isPresentationallyHidden(box)
            || isClipping(box)
            || isHorizontalScroller(box)
          ) {
            extendsDocument = false;
            break;
          }
        }
        if (extendsDocument) {
          visibleDocumentRight = Math.max(visibleDocumentRight, rect.right);
        }

        let skip = false;
        let clippedByAncestor = false;
        let ancestor = element.parentElement;

        while (ancestor) {
          // Document order means an already-flagged ancestor is the outermost offender.
          if (
            flagged.has(ancestor)
            || isHorizontalScroller(ancestor)
            || isPresentationallyHidden(ancestor)
          ) {
            skip = true;
            break;
          }
          if (isClipping(ancestor)) {
            if (isInlineContentTruncatedBy(element, ancestor)) {
              skip = true;
              break;
            }
            clippedByAncestor = true;
          }
          ancestor = ancestor.parentElement;
        }

        if (skip) {
          continue;
        }

        flagged.add(element);
        culprits.push({
          selector: describeElement(element),
          right: Math.round(rect.right),
          width: Math.round(rect.width),
          clippedByAncestor,
        });
      }

      const documentWidth = Math.round(visibleDocumentRight);

      return {
        viewportWidth,
        documentWidth,
        documentOverflow: documentWidth > viewportWidth + tolerance,
        culpritCount: culprits.length,
        culprits: culprits.slice(0, maxCulprits),
      };
    },
    { tolerance: tolerancePx, maxCulprits: maxReportedCulprits },
  );
}

export function isHorizontallyUnusable(report: HorizontalOverflowReport): boolean {
  return report.documentOverflow || report.culpritCount > 0;
}

export function summarizeHorizontalOverflow(report: HorizontalOverflowReport): string {
  if (!isHorizontallyUnusable(report)) {
    return '';
  }

  const widthPart = report.documentOverflow
    ? `document ${report.documentWidth}px > viewport ${report.viewportWidth}px`
    : `document ${report.documentWidth}px within viewport ${report.viewportWidth}px (overflow is clipped, not scrollable)`;

  if (report.culpritCount === 0) {
    return widthPart;
  }

  const rendered = report.culprits
    .map((culprit) => `${culprit.selector} right=${culprit.right}px w=${culprit.width}px${culprit.clippedByAncestor ? ' clipped' : ''}`)
    .join(', ');

  return `${widthPart}; ${report.culpritCount} offending element(s): ${rendered}`;
}
