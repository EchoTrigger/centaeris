import { useCallback, useEffect, useRef, useState } from "react";
import { getDocument, GlobalWorkerOptions } from "pdfjs-dist";
import workerUrl from "pdfjs-dist/build/pdf.worker.min.mjs?worker&url";
import { useTranslation } from "../i18n";
import { loadOfficePdf } from "../chat/loadOfficePdf.mjs";

GlobalWorkerOptions.workerSrc = workerUrl;

function PreviewPage({ document, pageNumber, title, width, zoom, onPaint, onError }) {
  const { t } = useTranslation();
  const canvasRef = useRef(null);
  const [painted, setPainted] = useState(false);
  const [pageText, setPageText] = useState("");

  useEffect(() => {
    if (!document || !width) return;
    let disposed = false;
    let rendering;
    let page;
    void (async () => {
      page = await document.getPage(pageNumber);
      if (disposed) return;
      const base = page.getViewport({ scale: 1 });
      const viewport = page.getViewport({ scale: width * zoom / base.width });
      const ratio = Math.min(window.devicePixelRatio || 1, 2, Math.sqrt(4_000_000 / (viewport.width * viewport.height)));
      const buffer = canvasRef.current.ownerDocument.createElement("canvas");
      buffer.width = Math.max(1, Math.floor(viewport.width * ratio));
      buffer.height = Math.max(1, Math.floor(viewport.height * ratio));
      rendering = page.render({ canvasContext: buffer.getContext("2d"), viewport, transform: [ratio, 0, 0, ratio, 0, 0] });
      await rendering.promise;
      const text = await page.getTextContent();
      if (disposed) return;
      const canvas = canvasRef.current;
      canvas.width = buffer.width;
      canvas.height = buffer.height;
      canvas.getContext("2d").drawImage(buffer, 0, 0);
      canvas.style.width = `${viewport.width}px`;
      canvas.style.height = `${viewport.height}px`;
      setPageText(text.items.map((item) => item.str || "").join(" "));
      setPainted(true);
      onPaint(pageNumber);
      page.cleanup();
    })().catch(() => { if (!disposed) onError(); });
    return () => { disposed = true; rendering?.cancel(); };
  }, [document, onError, onPaint, pageNumber, width, zoom]);

  return <div className="officePreviewPage">
    <canvas
      ref={canvasRef}
      role="img"
      aria-label={t("officePreview.pageLabel", { title, page: pageNumber })}
      style={{ display: painted ? "block" : "none" }}
    />
    <p className="srOnly">{painted ? pageText : ""}</p>
  </div>;
}

function PreviewDocument({ src, title, onError }) {
  const { t } = useTranslation();
  const [document, setDocument] = useState(null);
  const [error, setError] = useState("");
  const [paintedPages, setPaintedPages] = useState(() => new Set());
  const [width, setWidth] = useState(0);
  const [zoom, setZoom] = useState(1);
  const viewportRef = useRef(null);

  useEffect(() => {
    const controller = new AbortController();
    let task;
    void loadOfficePdf(src, { signal: controller.signal }).then(async (data) => {
      if (controller.signal.aborted) return;
      task = getDocument({ data, isEvalSupported: false });
      const pdf = await task.promise;
      if (!controller.signal.aborted) setDocument(pdf);
    }).catch((failure) => {
      if (!controller.signal.aborted) { setError(failure.name === "TimeoutError" ? "officePreview.timeout" : "officePreview.error"); onError?.(failure); }
    });
    return () => { controller.abort(); void task?.destroy(); };
  }, [src, onError]);

  useEffect(() => {
    const target = viewportRef.current;
    const observer = new ResizeObserver(([entry]) => setWidth(Math.max(1, Math.floor(entry.contentRect.width))));
    observer.observe(target);
    return () => observer.disconnect();
  }, []);

  const markPainted = useCallback((pageNumber) => setPaintedPages((current) => {
    if (current.has(pageNumber)) return current;
    const next = new Set(current);
    next.add(pageNumber);
    return next;
  }), []);
  const markError = useCallback(() => setError("officePreview.error"), []);

  return <>
    {document && !error ? <nav className="officePreviewControls" aria-label={t("officePreview.navigation")}>
      <button type="button" aria-label={t("officePreview.zoomOut")} disabled={zoom <= 0.5} onClick={() => setZoom((value) => value - 0.25)}>−</button>
      <button type="button" aria-label={t("officePreview.fitWidth")} onClick={() => setZoom(1)}>{Math.round(zoom * 100)}%</button>
      <button type="button" aria-label={t("officePreview.zoomIn")} disabled={zoom >= 3} onClick={() => setZoom((value) => value + 0.25)}>+</button>
    </nav> : null}
    <div className="officePreviewViewport" ref={viewportRef} aria-busy={!paintedPages.size && !error}>
      {error ? <div className="officePreviewState" role="alert">{t(error)}</div> : <>
        {document ? <div className="officePreviewDocument">
          {Array.from({ length: document.numPages }, (_, index) => <PreviewPage
            key={index + 1}
            document={document}
            pageNumber={index + 1}
            title={title}
            width={width}
            zoom={zoom}
            onPaint={markPainted}
            onError={markError}
          />)}
        </div> : null}
      </>}
    </div>
  </>;
}

/** @param {{src: string, title: string, className?: string, onError?: (error: unknown) => void}} props */
export default function OfficePreview({ src, title, className = "", onError }) {
  const { t } = useTranslation();
  const [attempt, setAttempt] = useState(0);
  return <section className={`officePreview ${className}`} aria-label={title}>
    <PreviewDocument key={`${src}:${attempt}`} src={src} title={title} onError={onError} />
    <button className="officePreviewReload" type="button" onClick={() => setAttempt((value) => value + 1)}>{t("officePreview.reload")}</button>
  </section>;
}
