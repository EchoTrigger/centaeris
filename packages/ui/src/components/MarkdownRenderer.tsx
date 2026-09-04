import { type ReactNode } from "react";
import { toPositiveInt } from "./chat/numberUtils";

type MarkdownInlinePart =
  | { kind: "text"; text: string }
  | { kind: "code"; text: string }
  | { kind: "bold"; text: string }
  | { kind: "link"; text: string; href: string };

export type OpenWorkspacePathHandler = (
  path: string,
  options?: { startLine?: number; endLine?: number; agentRunId?: string },
) => void;

const isSafeLinkHref = (href: string): boolean => {
  const normalized = href.trim().toLowerCase();
  return normalized.startsWith("https://");
};

const parseMarkdownInline = (text: string): MarkdownInlinePart[] => {
  const pattern = /(`([^`]+)`)|(\*\*([^*]+)\*\*)|(\[([^\]]+)\]\(([^)]+)\))/g;
  const parts: MarkdownInlinePart[] = [];
  let lastIndex = 0;
  for (const match of text.matchAll(pattern)) {
    const index = match.index ?? 0;
    if (index > lastIndex) {
      parts.push({ kind: "text", text: text.slice(lastIndex, index) });
    }
    if (match[2]) {
      parts.push({ kind: "code", text: match[2] });
    } else if (match[4]) {
      parts.push({ kind: "bold", text: match[4] });
    } else if (match[6]) {
      parts.push({ kind: "link", text: match[6], href: match[7] ?? "" });
    }
    lastIndex = index + match[0].length;
  }
  if (lastIndex < text.length) {
    parts.push({ kind: "text", text: text.slice(lastIndex) });
  }
  return parts;
};

const isMarkdownTableDivider = (line: string): boolean =>
  /^\s*\|?\s*:?-{3,}:?\s*(\|\s*:?-{3,}:?\s*)+\|?\s*$/.test(line);

const splitMarkdownTableRow = (line: string): string[] => {
  const trimmed = line.trim().replace(/^\|/, "").replace(/\|$/, "");
  return trimmed.split("|").map((cell) => cell.trim());
};

const parseWorkspacePathReference = (
  value: string,
): { path: string; startLine?: number; endLine?: number } | null => {
  const trimmed = value
    .trim()
    .replace(/^<|>$/g, "")
    .replace(/[),.;，。；）]+$/g, "");
  if (
    !trimmed ||
    /^[a-z][a-z0-9+.-]*:\/\//i.test(trimmed) ||
    trimmed.startsWith("#")
  ) {
    return null;
  }
  const lineMatch = trimmed.match(/^(.*?)(?::(\d+)(?:[-:](\d+))?)$/);
  const pathText = (lineMatch?.[1] ?? trimmed).trim();
  if (!pathText || /\s/.test(pathText)) {
    return null;
  }
  const normalizedPath = pathText.replace(/\\/g, "/");
  const hasPathSeparator = normalizedPath.includes("/");
  const hasKnownWorkspacePrefix =
    /^(core|desktop|ui|docs|scripts|src)\//.test(normalizedPath);
  const hasKnownExtension = /\.[a-z0-9]{1,8}$/i.test(normalizedPath);
  if (!hasPathSeparator && !hasKnownWorkspacePrefix) {
    return null;
  }
  if (!hasKnownExtension && !hasKnownWorkspacePrefix) {
    return null;
  }
  return {
    path: pathText,
    startLine: toPositiveInt(lineMatch?.[2]),
    endLine: toPositiveInt(lineMatch?.[3]),
  };
};

const renderWorkspacePathInline = (
  text: string,
  key: string,
  onOpenWorkspacePath?: OpenWorkspacePathHandler,
): ReactNode => {
  const pathReference = parseWorkspacePathReference(text);
  if (!pathReference || !onOpenWorkspacePath) {
    return <code key={key}>{text}</code>;
  }
  const lineLabel = pathReference.startLine
    ? `:${pathReference.startLine}${pathReference.endLine ? `-${pathReference.endLine}` : ""}`
    : "";
  return (
    <button
      type="button"
      key={key}
      className="answerPathInlineButton"
      title={`打开 ${pathReference.path}${lineLabel}`}
      onClick={(event) => {
        event.preventDefault();
        event.stopPropagation();
        onOpenWorkspacePath(pathReference.path, {
          startLine: pathReference.startLine,
          endLine: pathReference.endLine,
        });
      }}
    >
      <code>{text}</code>
    </button>
  );
};

const renderMarkdownInline = (
  text: string,
  keyPrefix: string,
  onOpenWorkspacePath?: OpenWorkspacePathHandler,
): ReactNode[] =>
  parseMarkdownInline(text).map((part, index) => {
    const key = `${keyPrefix}-${index}`;
    if (part.kind === "code") {
      return renderWorkspacePathInline(part.text, key, onOpenWorkspacePath);
    }
    if (part.kind === "bold") {
      return <strong key={key}>{part.text}</strong>;
    }
    if (part.kind === "link") {
      const pathReference =
        parseWorkspacePathReference(part.href) ??
        parseWorkspacePathReference(part.text);
      if (pathReference && onOpenWorkspacePath) {
        return (
          <button
            type="button"
            key={key}
            className="answerPathLinkButton"
            title={`打开 ${pathReference.path}`}
            onClick={(event) => {
              event.preventDefault();
              event.stopPropagation();
              onOpenWorkspacePath(pathReference.path, {
                startLine: pathReference.startLine,
                endLine: pathReference.endLine,
              });
            }}
          >
            {part.text}
          </button>
        );
      }
      if (!isSafeLinkHref(part.href)) {
        return <span key={key} title="Unsupported link scheme">{part.text}</span>;
      }
      const href = part.href;
      return (
        <a
          key={key}
          href={href}
          target="_blank"
          rel="noreferrer"
        >
          {part.text}
        </a>
      );
    }
    return part.text;
  });

export const renderMarkdownNodes = (
  text: string,
  onOpenWorkspacePath?: OpenWorkspacePathHandler,
): ReactNode[] => {
  const normalized = text.trim();
  if (!normalized) {
    return [];
  }

  const lines = normalized.split(/\r?\n/);
  const nodes: ReactNode[] = [];
  let index = 0;
  let nodeIndex = 0;

  const collectParagraph = () => {
    const paragraphLines: string[] = [];
    while (index < lines.length) {
      const line = lines[index];
      const trimmed = line.trim();
      if (!trimmed) {
        break;
      }
      if (
        /^```/.test(trimmed) ||
        /^#{1,3}\s+/.test(line) ||
        trimmed.startsWith(">") ||
        /^[-*]\s+/.test(trimmed) ||
        /^\d+\.\s+/.test(trimmed)
      ) {
        break;
      }
      if (
        index + 1 < lines.length &&
        line.includes("|") &&
        isMarkdownTableDivider(lines[index + 1])
      ) {
        break;
      }
      paragraphLines.push(line);
      index += 1;
    }
    if (paragraphLines.length > 0) {
      nodes.push(
        <p key={`md-p-${nodeIndex++}`}>
          {renderMarkdownInline(
            paragraphLines.join("\n"),
            `md-p-${nodeIndex}`,
            onOpenWorkspacePath,
          )}
        </p>,
      );
    }
  };

  while (index < lines.length) {
    const line = lines[index];
    const trimmed = line.trim();
    if (!trimmed) {
      index += 1;
      continue;
    }

    if (trimmed.startsWith("```")) {
      const language = trimmed.slice(3).trim();
      index += 1;
      const codeLines: string[] = [];
      while (index < lines.length && !lines[index].trim().startsWith("```")) {
        codeLines.push(lines[index]);
        index += 1;
      }
      if (index < lines.length) {
        index += 1;
      }
      nodes.push(
        <pre key={`md-code-${nodeIndex++}`} className="markdown-code-block">
          <code data-language={language || undefined}>
            {codeLines.join("\n")}
          </code>
        </pre>,
      );
      continue;
    }

    const headingMatch = line.match(/^(#{1,3})\s+(.+)$/);
    if (headingMatch) {
      const level = headingMatch[1].length;
      const content = renderMarkdownInline(
        headingMatch[2].trim(),
        `md-h-${nodeIndex}`,
        onOpenWorkspacePath,
      );
      if (level === 1) {
        nodes.push(<h1 key={`md-h-${nodeIndex++}`}>{content}</h1>);
      } else if (level === 2) {
        nodes.push(<h2 key={`md-h-${nodeIndex++}`}>{content}</h2>);
      } else {
        nodes.push(<h3 key={`md-h-${nodeIndex++}`}>{content}</h3>);
      }
      index += 1;
      continue;
    }

    if (trimmed.startsWith(">")) {
      const quoteLines: string[] = [];
      while (index < lines.length && lines[index].trim().startsWith(">")) {
        quoteLines.push(lines[index].trim().replace(/^>\s?/, ""));
        index += 1;
      }
      nodes.push(
        <blockquote key={`md-quote-${nodeIndex++}`} className="markdown-blockquote">
          {renderMarkdownInline(
            quoteLines.join("\n"),
            `md-quote-${nodeIndex}`,
            onOpenWorkspacePath,
          )}
        </blockquote>,
      );
      continue;
    }

    if (
      index + 1 < lines.length &&
      line.includes("|") &&
      isMarkdownTableDivider(lines[index + 1])
    ) {
      const headers = splitMarkdownTableRow(line);
      index += 2;
      const rows: string[][] = [];
      while (
        index < lines.length &&
        lines[index].includes("|") &&
        lines[index].trim()
      ) {
        rows.push(splitMarkdownTableRow(lines[index]));
        index += 1;
      }
      nodes.push(
        <div className="markdown-wide-block" key={`md-wide-table-${nodeIndex++}`}>
          <div className="markdown-table">
            <table>
              <thead>
                <tr>
                  {headers.map((header, cellIndex) => (
                    <th key={`h-${cellIndex}`}>
                      {renderMarkdownInline(
                        header,
                        `md-th-${nodeIndex}-${cellIndex}`,
                        onOpenWorkspacePath,
                      )}
                    </th>
                  ))}
                </tr>
              </thead>
              <tbody>
                {rows.map((row, rowIndex) => (
                  <tr key={`r-${rowIndex}`}>
                    {headers.map((_, cellIndex) => (
                      <td key={`c-${cellIndex}`}>
                        {renderMarkdownInline(
                          row[cellIndex] ?? "",
                          `md-td-${nodeIndex}-${rowIndex}-${cellIndex}`,
                          onOpenWorkspacePath,
                        )}
                      </td>
                    ))}
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </div>,
      );
      continue;
    }

    const listMatch = trimmed.match(/^([-*]|\d+\.)\s+(.+)$/);
    if (listMatch) {
      const ordered = /\d+\./.test(listMatch[1]);
      const items: string[] = [];
      while (index < lines.length) {
        const itemMatch = lines[index].trim().match(/^([-*]|\d+\.)\s+(.+)$/);
        if (!itemMatch || /\d+\./.test(itemMatch[1]) !== ordered) {
          break;
        }
        items.push(itemMatch[2]);
        index += 1;
      }
      const renderedItems = items.map((item, itemIndex) => (
        <li key={`li-${itemIndex}`}>
          {renderMarkdownInline(
            item,
            `md-li-${nodeIndex}-${itemIndex}`,
            onOpenWorkspacePath,
          )}
        </li>
      ));
      nodes.push(
        ordered ? (
          <ol key={`md-list-${nodeIndex++}`}>{renderedItems}</ol>
        ) : (
          <ul key={`md-list-${nodeIndex++}`}>{renderedItems}</ul>
        ),
      );
      continue;
    }

    collectParagraph();
  }

  return nodes;
};
