# Centaeris Document Processor

`packages/document_processor` (`centaeris-document-processor`) is the required
material-worker dependency that inspects, converts, and OCRs supported Office,
PDF, text, and image inputs for the hosted platform. It registers its current
processing specification with the platform; Runtime obtains that identity
through the API when registering material MCP tools and has no processor
implementation of its own.

The service ships CPU and GPU image variants (`cpu`/`gpu` optional
dependencies, mutually exclusive). Processing runs in a read-only, no-network,
non-root container with dropped capabilities and fixed CPU/memory/process
limits; each document gets fresh anonymous input/output volumes. Long
documents are streamed page-by-page under enforced pixel, output-size,
timeout, process, and memory budgets, with bounded retry on transient errors.

See [Document processing](../../docs/workspace/operations/DocumentProcessing.md)
for the complete streaming, retry, and measurement boundary.
