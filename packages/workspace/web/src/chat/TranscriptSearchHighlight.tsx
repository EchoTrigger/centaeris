import { createContext, useContext } from "react";

export const TranscriptSearchQuery = createContext("");

// Highlight rendered text without changing Markdown source or inserting HTML.
export function HighlightedTranscriptText({ text }: { text: string }) {
  const query = useContext(TranscriptSearchQuery);
  if (!query) return text;
  const folded = text.toLowerCase();
  const needle = query.toLowerCase();
  const parts = [];
  let start = 0;
  for (let at = folded.indexOf(needle); at >= 0; at = folded.indexOf(needle, start)) {
    parts.push(text.slice(start, at), <mark key={at}>{text.slice(at, at + query.length)}</mark>);
    start = at + query.length;
  }
  parts.push(text.slice(start));
  return parts;
}
