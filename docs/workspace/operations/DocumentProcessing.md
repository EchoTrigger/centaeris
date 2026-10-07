# Document processing

## Required service

The document-processor image is a required material-worker dependency. The
dedicated Python service inspects its image and specification, then registers the
current specification with the platform. Runtime obtains that identity from the
API when registering material MCP tools; it has no processor implementation.
Missing/invalid processor configuration fails Worker startup. Material tool
registration fails closed until a platform specification is available.

Specification checks and document containers use a read-only root, no network,
dropped capabilities, a non-root user, and fixed limits of 4 CPUs, 4 GiB memory,
and 64 processes. Both CPU and GPU modes use the same CPU quota; the Docker
host must provide at least 4 CPUs.
Each document receives fresh anonymous input/output volumes. Processing is owned
by a durable platform task, not by a user's run or a Rust scheduling job.

## Supported classes

The processor provides native inspection or conversion for supported Office,
PDF, text, and image inputs. OCR is used when a native representation is
insufficient and the selected specification requires it. The representation is
bound to the source version and processing specification.

## Streaming

Long documents are processed incrementally. There is no fixed page-count
ceiling. Each page or frame is handled in order and output is written as it is
accepted, while pixel, output-size, timeout, process, and memory budgets remain
enforced. A permanent error stops immediately; repeated transient I/O or timeout
errors stop after the bounded retry policy.

MCP reads/searches return a durable operation handle while processing is pending.
The model polls get_operation and reads again after completion. Cancellation
detaches that run's operation; even cancelling all waiters does not stop a live
platform material. Source deletion/version changes prevent stale publication.
Worker restart reclaims expired leases without accepting results from an old epoch.

## Measurement boundary

Synthetic and native-parser tests cover more than 1,000 pages or frames, UTF-8
locations, bounded output, and manifest validation. They do not prove OCR
quality on every real document.

Model reuse is currently process-local. Cross-document warm reuse and the
historical startup reduction have not been remeasured and must not be claimed
without a current controlled run.
