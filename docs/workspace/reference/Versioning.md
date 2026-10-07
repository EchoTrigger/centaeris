# Versioning

Workspace's product version is `0.1.0`. Rust, Python, and npm component metadata,
lockfiles, API discovery, and MCP server information advertise the same product
version. `scripts/test_product_version.py` checks their agreement in the local CI
gate.

`core-revision.txt` selects one complete Core commit. Local builds require the
resolved Cargo Git source to match that commit; CI uses the same locked revision.
Publishing Workspace requires the selected Core commit to be publicly fetchable.
The example environment uses the same revision for image labels.

Product versions do not select wire schemas or processing semantics. The Core
protocol, authorization schema, transcript schema, and immutable document
processing specification retain their own identifiers. Processor identity changes
require coordinated producer and consumer validation and new representation
digests. Plugin package versions belong to their publishers.
