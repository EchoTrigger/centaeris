import { classHighlighter, highlightTree } from "@lezer/highlight";
import { codePreviewLanguageCanLoad, loadCodePreviewLanguage } from "../loadCodePreviewLanguage.mjs";

export type SyntaxRange = Readonly<{ from: number; to: number; className: string }>;
const languages = new Map<string, ReturnType<typeof loadCodePreviewLanguage>>();

export async function highlightCode(code: string, language: string): Promise<readonly SyntaxRange[]> {
  const name = language.toLowerCase();
  if (!codePreviewLanguageCanLoad(name)) return [];
  let loading = languages.get(name);
  if (!loading) {
    loading = loadCodePreviewLanguage(name);
    languages.set(name, loading);
  }
  const support = await loading;
  const ranges: SyntaxRange[] = [];
  highlightTree(support.language.parser.parse(code), classHighlighter, (from, to, className) => ranges.push({ from, to, className }));
  return ranges;
}
