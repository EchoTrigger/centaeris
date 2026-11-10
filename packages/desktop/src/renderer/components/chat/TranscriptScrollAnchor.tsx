import { Component, type ReactNode, type RefObject } from "react";
import { elementScroll, type Virtualizer } from "@tanstack/react-virtual";

export type TranscriptReadingAnchor = {
  id: string;
  rowKey: string | null;
  top: number;
  scrollTop: number;
};
type Props = {
  ids: readonly string[];
  viewportRef: RefObject<HTMLDivElement | null>;
  contentRef?: RefObject<HTMLDivElement | null>;
  onPrepend: (anchor: TranscriptReadingAnchor) => void;
  children: ReactNode;
};

function anchorAt(node: HTMLElement, viewport: HTMLDivElement): TranscriptReadingAnchor {
  return {
    id: node.dataset.transcriptAnchor!,
    rowKey: node.closest<HTMLElement>("[data-history-row]")?.dataset.historyRow ?? null,
    top: node.getBoundingClientRect().top - viewport.getBoundingClientRect().top,
    scrollTop: viewport.scrollTop,
  };
}

function readTranscriptAnchor(viewport: HTMLDivElement): TranscriptReadingAnchor | null {
  const bounds = viewport.getBoundingClientRect();
  const visible = [...viewport.querySelectorAll<HTMLElement>("[data-transcript-anchor]")].filter(
    (node) => {
      const rect = node.getBoundingClientRect();
      return rect.bottom > bounds.top && rect.top < bounds.bottom;
    },
  );
  const node = visible.find(
    (node) => !visible.some((child) => child !== node && node.contains(child)),
  );
  return node ? anchorAt(node, viewport) : null;
}

// A snapshot must precede DOM mutation. Capturing at request start loses wheel
// movement during I/O; a layout-effect cleanup can already see changed children.
export class TranscriptScrollAnchor extends Component<
  Props,
  object,
  TranscriptReadingAnchor | null
> {
  getSnapshotBeforeUpdate(previous: Props): TranscriptReadingAnchor | null {
    const first = previous.ids[0];
    if (!first || this.props.ids.indexOf(first) <= 0) return null;
    const viewport = this.props.viewportRef.current;
    if (!viewport) return null;
    return readTranscriptAnchor(viewport);
  }

  componentDidUpdate(_previous: Props, _state: object, snapshot: TranscriptReadingAnchor | null) {
    if (snapshot) this.props.onPrepend(snapshot);
  }

  render() {
    return (
      <div
        ref={this.props.contentRef}
        style={{ width: "100%", position: "relative", display: "flow-root" }}
      >
        {this.props.children}
      </div>
    );
  }
}

type TimelineVirtualizer = Virtualizer<HTMLDivElement, HTMLDivElement>;
type ScrollOptions = { adjustments?: number; behavior?: ScrollBehavior };

// One instance owns reading-position preservation across page insertion,
// observer commits and deferred renderers. The follower separately owns latest.
export function createTranscriptScrollAnchor(getViewport: () => HTMLDivElement | null) {
  let reading: TranscriptReadingAnchor | null = null;
  let pending: { top: number; instance: TimelineVirtualizer } | null = null;

  const observeScroll = () => {
    const viewport = getViewport();
    if (!viewport || !reading) return;
    // Only unowned scroll changes move the reading target. Every programmatic
    // write below updates this baseline before its DOM scroll event arrives.
    const movement = viewport.scrollTop - reading.scrollTop;
    reading.top -= movement;
    reading.scrollTop = viewport.scrollTop;
    if (movement === 0) return;
    const node = [...viewport.querySelectorAll<HTMLElement>("[data-transcript-anchor]")].find(
      (node) => node.dataset.transcriptAnchor === reading?.id,
    );
    if (!node) return;
    const rect = node.getBoundingClientRect();
    const bounds = viewport.getBoundingClientRect();
    // Once the reader has moved beyond the old point, preserve what they are
    // reading now. Do not adopt a position already shifted by uncommitted layout.
    if (
      (rect.bottom <= bounds.top || rect.top >= bounds.bottom) &&
      Math.abs(rect.top - bounds.top - reading.top) < 1
    ) {
      reading = readTranscriptAnchor(viewport) ?? reading;
    }
  };
  const write = (top: number, options?: ScrollOptions, instance?: TimelineVirtualizer) => {
    const viewport = getViewport();
    if (!viewport) return;
    if (instance) elementScroll(top, options ?? {}, instance);
    else viewport.scrollTop = top;
    if (reading) reading.scrollTop = viewport.scrollTop;
  };
  const release = () => {
    reading = null;
    pending = null;
  };
  return {
    get rowKey() {
      return reading?.rowKey;
    },
    begin(anchor: TranscriptReadingAnchor) {
      reading = anchor;
      pending = null;
    },
    release,
    hold(element: Element) {
      const viewport = getViewport();
      if (!viewport) {
        release();
        return;
      }
      const node = element.closest<HTMLElement>("[data-transcript-anchor]");
      reading = node ? anchorAt(node, viewport) : null;
      pending = null;
    },
    observeScroll,
    manualIntent() {
      observeScroll();
      pending = null;
    },
    scrollToFn(offset: number, options: ScrollOptions, instance: TimelineVirtualizer) {
      if (options.adjustments !== undefined) {
        // A resize requests correction before TanStack publishes spacer geometry.
        // Apply it after that commit so the browser cannot clamp to the old height.
        pending = { top: offset + options.adjustments, instance };
      } else write(offset, options, instance);
    },
    shouldAdjustSize(item: { end: number }, _delta: number, instance: TimelineVirtualizer) {
      // The content anchor owns all size corrections while a page settles.
      // Otherwise preserve visible disclosure titles by adjusting only above.
      return !reading && item.end <= (instance.scrollOffset ?? 0);
    },
    commit(instance: TimelineVirtualizer) {
      const viewport = getViewport();
      if (!viewport) return;
      const correction = pending;
      pending = null;
      if (!reading) {
        if (correction) write(correction.top, { behavior: "auto" }, correction.instance);
        return;
      }
      observeScroll();
      const anchor = reading;
      const node = [...viewport.querySelectorAll<HTMLElement>("[data-transcript-anchor]")].find(
        (node) => node.dataset.transcriptAnchor === anchor.id,
      );
      if (!node) {
        release();
        return;
      }
      const delta =
        node.getBoundingClientRect().top - viewport.getBoundingClientRect().top - anchor.top;
      if (Math.abs(delta) >= 1) write(viewport.scrollTop + delta);
      const settled = instance.getVirtualItems().every((item) => {
        const row = instance.elementsCache?.get(item.key);
        return (
          !row ||
          (!row.querySelector('[aria-busy="true"]') &&
            Math.abs(row.getBoundingClientRect().height - item.size) < 1)
        );
      });
      if (
        settled &&
        Math.abs(
          node.getBoundingClientRect().top - viewport.getBoundingClientRect().top - anchor.top,
        ) < 1
      )
        reading = null;
    },
  };
}
