import { memo } from "react";
import { MarkdownRenderer } from "./markdown/MarkdownRenderer";

export const MarkdownContent = memo(function MarkdownContent(/** @type {{ text: string, allowVisuals?: boolean, isStreaming?: boolean }} */ { text, allowVisuals = false, isStreaming = false }) {
  return <MarkdownRenderer source={text} streaming={isStreaming} allowVisuals={allowVisuals} />;
});

export const StreamingMarkdownContent = memo(function StreamingMarkdownContent({ text, finalized = false }) {
  return <MarkdownContent text={text} allowVisuals isStreaming={!finalized} />;
});
