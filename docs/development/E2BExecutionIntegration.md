# E2B execution integration

Centaeris provides an open Agent control plane. It owns Workspace access, task
intent, fair queueing, cancellation, delivery and usage attribution. Core owns the
native Centaeris Agent's reasoning, tools, events, checkpoints and continuation.
External harnesses retain their native sessions and execution loops. Managed
execution should reuse the complete E2B platform rather than expand the custom
Docker/runsc backend into a node, VM or snapshot-management product.

This draft carries existing lifecycle correctness fixes and defines the first
integration boundary. The E2B adapter, deployment and end-to-end acceptance are
not implemented by the initial revision.

## Ownership and reuse

| Boundary | Centaeris owns | Reused implementation |
| --- | --- | --- |
| Product control | Workspace authorization, Run intent, queueing, approval and delivery | Existing API, Worker and product surfaces |
| Native Agent | Core session, tool and continuation semantics | Existing Core |
| External harness | Identity binding, capability negotiation and sourced event projections | The harness's native protocol and recovery |
| Managed environment | Provider binding, policy, observations and outstanding control obligations | E2B lifecycle, templates, snapshots and sandbox runtime |
| Usage | Source, task/environment attribution, budget and reconciliation | Provider, gateway and harness usage evidence |

The business database remains authoritative for business authorization and pending
control responsibility. E2B remains authoritative for its sandbox's physical state.
Persist provider identities and observations without recreating E2B's internal
placement, node-capacity or snapshot state machines.

## First integration boundary

Run one real task through the existing product admission path, Worker, Runtime,
Core and E2B, then preserve or destroy the environment according to its frozen
policy. The initial integration must include file operations, command execution,
cancellation, reconnect, cleanup and usage attribution.

`ExecutionHostRunner` is the starting point for Core's command and filesystem
operations. It is not the environment lifecycle interface or an external harness
protocol. Runtime startup, residency reconciliation, execution creation and
teardown currently call Docker directly; these entry points must dispatch through
the selected provider without requiring a Docker daemon for an E2B-only instance.
Use the smallest lifecycle seam required for this provider. Keep Core independent
of the concrete SDK, transport and deployment.

Select and pin the E2B SDK, runtime, template and deployment versions. E2B Embed
is the first self-hosted candidate on a Linux host with verified KVM support. Its
single-machine deployment is separate from cluster provisioning or autoscaling.
Existing runsc availability does not establish Firecracker/KVM compatibility.

## Lifecycle safety

Before a provider side effect, persist the business attempt, scoped provider
request identity and responsible owner. Record the returned sandbox identity when
known. A create timeout remains uncertain; do not change the request key and
create another sandbox or release a commitment solely because a query is empty.
Verify the selected deployment's idempotency route, retention and lookup behavior.
Keep an unresolved attempt recoverable after adapter restart.

Preserve independent bounded control processing. Ordinary capacity exhaustion must
not prevent cancellation intent, terminal advancement or short control requests.
Slow provider cleanup has its own bounded processing capacity. Control authority
cannot acquire ordinary execution capability. Completion requires the appropriate
terminal, cleanup and release facts; a worker's claim of success is insufficient.

Cancelling a Run does not by itself authorize destroying its Environment. Separate
the Run's execution-slot release from environment retention, provider cleanup and
remaining cost. Keep uncertain operations charged according to their applicable
business commitment while investigation responsibility remains pending.

## Retention and resource policy

Represent preservation of files/artifacts, native harness sessions, process-memory
snapshots and continuously online services as distinct capabilities. Do not infer
that a paused service remains online or that a filesystem-only restore preserves
process state. Persist the actual provider result and any allowed degradation.

The proposed default for new environments is preserved work products and a
recoverable harness session. It requires explicit policy, supported capabilities
and acceptance before activation. Existing retained environments keep their
original same-instance and process-continuity obligations. No default changes in
this draft.

Keep the current 8 global / 4 Workspace / 8 ordinary Worker execution settings as
the acceptance baseline. Distinguish business execution policy from provider
physical admission. Hosted allocations continue using full resource reservations;
the carried Core limits/reservation contract does not enable hosted overcommit.
Advertise byte, inode or private-writable limits only when the selected provider
actually enforces them. Unsupported required limits fail explicitly rather than
becoming size observations or a new custom storage implementation.

## Legacy transition

The integration branch retains the existing correctness source through
`67db3b70e816cb0a95f0e5c5c977a9789ea66f12`, including:

- Proven non-dispatch settlement, with resident reuse and Unknown protections.
- Execute/control lease purpose and independent control processing.
- Server-verified terminal, cleanup and release prerequisites for completion.
- Durable waiter wakeups after confirmed capacity release.
- Supporting behavior tests and Runtime schema 13 through 16 forward migrations.
- The Core resource-limit/admission-commitment distinction, with hosted full charging.

The later opt-in timing and target-bound host transport optimization series stays
on the original branch. Its diagnostic failure fix applies to that omitted timing
path and is not needed by this baseline. The original published history remains
available; no commits are rewritten or removed.

New environments freeze their provider and policy revision. Old Docker/runsc
environments remain with the matching legacy backend until their obligations are
settled. Do not switch active Runs, reinterpret signed grants, or claim that live
processes can be converted to Firecracker snapshots. File transfer requires
verification. Retire the legacy path only after no live bindings or pending
cleanup obligations require it.

## Acceptance

- A real Core task performs preparation, file read/write, command execution and
  delivery in the exact E2B sandbox selected for its Workspace.
- Cancelling with ordinary execution and connection pools saturated completes
  the correct Run-scoped cleanup and release without extra execution authority.
- A lost creation reply does not cause duplicate creation or early settlement.
- Restarting Runtime or the adapter recovers the provider identity and pending
  obligations; command recovery does not blindly repeat accepted effects.
- Retention/reconnect preserves the promised files and session state. A reduced
  pause mode cannot be reported as full-memory retention.
- Provider refusal and capacity limits produce classified waiting or failure,
  not an additional Centaeris node-placement subsystem.
- Usage retains measured, confirmed, estimated and unknown provenance. Native
  subscription usage that is unavailable is not recorded as zero.
- New provider bindings coexist with legacy environments without changing old
  signatures, authorization or retention policy.

Record release build identities and separate queue wait, preparation, first useful
tool, Run terminal, Job completion and environment settlement. Compare IO under
equivalent persistence semantics. Earlier dev-profile Docker/runsc timings are
diagnostic historical evidence, not E2B performance results.

## Subsequent integration

After the Core/E2B path passes, connect one external harness. Evaluate T3's public
host interface before choosing a native adapter, checking execution location,
events, approval, cancellation, resume and usage attribution. T3 is optional and
must not become another mandatory Workspace/authorization authority.

Then validate one bounded plan/edit/review/approve workflow with explicit writer
ownership, cancellation propagation and effect-safe recovery. Desktop isolation
and broader billing follow their actual platform and provider capabilities. A
general workflow language and new VM, snapshot or filesystem infrastructure are
outside this integration.

## References

- [E2B Embed](https://e2b.dev/resources/introducing-e2b-embed)
- [E2B deployment models](https://docs.e2b.dev/byoc)
- [E2B persistence](https://docs.e2b.dev/sandbox/persistence)
- [E2B runtime architecture](https://github.com/e2b-dev/runtime/blob/main/docs/ARCHITECTURE.md)
- [T3 external control](https://github.com/pingdotgg/t3code/blob/main/docs/user/outside-agents.md)
