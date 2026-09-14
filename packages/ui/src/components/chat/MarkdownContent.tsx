import { memo, useEffect, useMemo, useRef } from "react";
import {
  renderMarkdownNodes,
  type OpenWorkspacePathHandler,
} from "../MarkdownRenderer";

export type MarkdownBlockProjection = {
  sourceText: string;
  sealedBlocks: readonly { id: number; text: string }[];
  sealedEnd: number;
  scanOffset: number;
  lineStart: number;
  inFence: boolean;
  nextBlockId: number;
};

export const createMarkdownBlockProjection = (): MarkdownBlockProjection => ({
  sourceText: "",
  sealedBlocks: [],
  sealedEnd: 0,
  scanOffset: 0,
  lineStart: 0,
  inFence: false,
  nextBlockId: 0,
});

export const updateMarkdownBlockProjection = (
  previous: MarkdownBlockProjection,
  text: string,
  finalize: boolean,
): MarkdownBlockProjection => {
  if (!text.startsWith(previous.sourceText)) {
    return updateMarkdownBlockProjection(
      createMarkdownBlockProjection(),
      text,
      finalize,
    );
  }

  const appendedBlocks: { id: number; text: string }[] = [];
  let sealedEnd = previous.sealedEnd;
  let lineStart = previous.lineStart;
  let inFence = previous.inFence;
  let nextBlockId = previous.nextBlockId;

  const sealThrough = (end: number) => {
    const blockText = text.slice(sealedEnd, end);
    if (blockText.trim()) {
      appendedBlocks.push({ id: nextBlockId++, text: blockText });
    }
    sealedEnd = end;
  };

  for (let index = previous.scanOffset; index < text.length; index += 1) {
    if (text[index] !== "\n") {
      continue;
    }
    const trimmedLine = text.slice(lineStart, index).trim();
    if (trimmedLine.startsWith("```")) {
      inFence = !inFence;
      if (!inFence) {
        sealThrough(index + 1);
      }
    } else if (!inFence && !trimmedLine) {
      sealThrough(index + 1);
    }
    lineStart = index + 1;
  }

  if (finalize) {
    sealThrough(text.length);
    lineStart = text.length;
  }

  return {
    sourceText: text,
    sealedBlocks:
      appendedBlocks.length > 0
        ? [...previous.sealedBlocks, ...appendedBlocks]
        : previous.sealedBlocks,
    sealedEnd,
    scanOffset: text.length,
    lineStart,
    inFence,
    nextBlockId,
  };
};

const MarkdownBlock = memo(function MarkdownBlock({
  text,
  onOpenWorkspacePath,
}: {
  text: string;
  onOpenWorkspacePath?: OpenWorkspacePathHandler;
}) {
  return <>{renderMarkdownNodes(text, onOpenWorkspacePath)}</>;
});

export function MarkdownContent({
  text,
  isStreaming = false,
  onOpenWorkspacePath,
}: {
  text: string;
  isStreaming?: boolean;
  onOpenWorkspacePath?: OpenWorkspacePathHandler;
}) {
  const committedProjection = useRef(createMarkdownBlockProjection());
  const projection = useMemo(
    () =>
      updateMarkdownBlockProjection(
        committedProjection.current,
        text,
        !isStreaming,
      ),
    [isStreaming, text],
  );

  useEffect(() => {
    committedProjection.current = projection;
  }, [projection]);

  const activeText = projection.sourceText.slice(projection.sealedEnd);
  if (projection.sealedBlocks.length === 0 && !activeText.trim()) {
    return null;
  }

  return (
    <div className="markdown-content">
      {projection.sealedBlocks.map((block) => (
        <MarkdownBlock
          key={block.id}
          text={block.text}
          onOpenWorkspacePath={onOpenWorkspacePath}
        />
      ))}
      {activeText ? (
        <MarkdownBlock
          key={projection.nextBlockId}
          text={activeText}
          onOpenWorkspacePath={onOpenWorkspacePath}
        />
      ) : null}
    </div>
  );
}
