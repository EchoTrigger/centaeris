# Read-only Office preview

Workspace previews DOCX, XLSX and PPTX files as PDFs produced by the existing
document-processing pipeline. LibreOffice runs headlessly inside the isolated
document-processor container. The browser receives only the derived PDF, so the
preview has no Office editor surface, save action or WOPI integration. The
original file remains available through its authenticated download route.

## Deployment

Build and publish the document-processor image described in
[Document processing](DocumentProcessing.md), keep the `document` processing
specification active, and run the material worker with access to the same object
storage as the API. No separate Office viewer service, exposed viewer port or
Office-specific environment variable is required.

LibreOffice Writer, Calc and Impress are installed in the document-processor
image. The processor invokes LibreOffice in headless safe mode with a bounded
conversion timeout, then publishes `preview.pdf` using the normal immutable
derived-representation contract. Conversion failures remain task failures; they
do not replace or mutate the original object.

## Request lifecycle

`GET /api/office-preview/{ownerKind}/{objectId}` accepts
`userLibraryObject`, `sourceObject` and `artifact` owners. It rechecks the
authenticated user, workspace or source access, object state, content generation
and SHA-256 digest on every request.

The representation identity includes the owner kind and ID, content generation,
content digest and document processing specification digest. An existing exact
representation streams immediately as `application/pdf`. Otherwise the API
atomically queues the standard platform material-processing task and returns a
local `202` loading page that refreshes once per second. The worker publishes the
derived representation atomically; a later request then streams the PDF.

The endpoint is read-only and sends `Cache-Control: no-store`. It exposes no
write, save or original-content route. Access revocation takes effect on the
next request. As with any successfully delivered response, bytes already received
by a client cannot be recalled.

## Browser presentation

The web client lazily loads PDF.js and paints the derived PDF inside the app;
Office preview does not depend on the browser's native PDF plugin. Library,
attachment and file-panel previews share the renderer, with previous/next page
controls, zoom/fit-width controls and accessible page text. Only the current page is rendered, with a
four-million-pixel canvas budget. The worker is bundled with the web application.

The client polls authenticated `202` responses once per second, stopping after
two minutes or when the preview is closed. Errors, invalid PDFs and timeouts
show a visible message. Reload starts a fresh read and polling attempt; it does
not reset a failed material task or its retry budget. Original downloads remain
available. Authorization remains entirely at the API boundary.

## Verification

Use representative Word, Excel and PowerPoint files. Confirm that Chinese text
renders, spreadsheet formula results are present, slides are readable, the
preview fills the intended area, and the original download still returns the
source file. Also verify that changing the source generation creates a new
representation and that revoked access returns 404.
