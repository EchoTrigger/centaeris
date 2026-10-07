import { lazy, Suspense, useEffect, useState } from "react";
import { useTranslation } from "../i18n";
import { codePreviewCanRender } from "../chat/codePreviewFormats.mjs";
import { officeFileType } from "../chat/officeFormats.mjs";
import { TextFilePreview } from "./TextFilePreview";

const OfficePreview = lazy(() => import("./OfficePreview"));

/** @param {{src: string, title: string, contentType?: string, className?: string, onError?: (error: unknown) => void}} props */
export function DocumentPreview({ src, title, contentType = "", className = "", onError }) {
  const { t } = useTranslation();
  const [state, setState] = useState({ src: "", text: "", error: false });
  const office = Boolean(officeFileType(title));
  const text = !office && (contentType.startsWith("text/") || codePreviewCanRender(title, contentType));
  useEffect(() => {
    if (!text) return;
    const abort = new AbortController();
    void fetch(src, { credentials: "include", signal: abort.signal }).then(async (response) => {
      if (!response.ok) throw Object.assign(new Error("preview_unavailable"), { status: response.status });
      const value = await response.text();
      if (!abort.signal.aborted) setState({ src, text: value, error: false });
    }).catch(error => {
      if (!abort.signal.aborted) { setState({ src, text: "", error: true }); onError?.(error); }
    });
    return () => abort.abort();
  }, [src, text, onError]);
  if (office) return <Suspense fallback={null}><OfficePreview src={src} title={title} className={className} onError={onError} /></Suspense>;
  if (!text) return <iframe className={className} src={src} title={title} />;
  if (state.src !== src) return null;
  if (state.error) return <div role="alert">{t("appRoute.unableToLoadThisFile")}</div>;
  return <TextFilePreview className={className} content={state.text} contentType={contentType} title={title} renderMarkdown />;
}
