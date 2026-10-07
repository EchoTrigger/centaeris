# Hosted model admission

Core owns the model and task semantics through `ModelClient`. Workspace Runtime
implements that interface by calling Django `/internal/model-runs`. Django owns
the provider credentials and the OpenAI/Anthropic SDK calls, so it is the sole
hosted admission point for actual provider attempts. The SDK clients have
`max_retries=0`; Runtime retains its existing proxy retry loop. The Core
in-process ticket is not used on this hosted path.

Each provider credential must be assigned to an explicit `ModelQuotaDomain`.
No quota identity is inferred from provider name, model name, or secret.
Released credentials migrate with a null domain and real calls fail closed
until an operator configures and enables one. A domain may be shared by
several credentials when the external account actually shares a quota. Use
the same domain ID and concurrency limit for each binding:

```sh
python manage.py configure_model_quota --domain-id ACCOUNT_QUOTA_ID --max-concurrent LIMIT --provider-id PROVIDER_ID --enable
```

`LIMIT` is the account's configured concurrent attempt ceiling, from 1 to 64;
it is not the local Desktop default. The command rejects an attempt to reuse
the same domain ID with a different limit. Run it against the upgraded API
database before resuming real model traffic.

Django holds a PostgreSQL session advisory lock for one slot throughout a
provider response or streamed iterator. Waiting requests poll without holding
a slot and cancellation closes their connection. A provider 408, 429, or 5xx
response with valid `Retry-After` advances a shared database cooldown. The
database clock is used for that cooldown. Existing calls continue during a
new cooldown; subsequent calls wait.

This coordinates multiple API processes and replicas that use the same
PostgreSQL database through direct or session-pooled connections. Transaction
pooling is incompatible with session advisory locks and must not sit on this
connection path. It does not provide a global FIFO queue or rate limit by
token/request count. If the advisory-lock connection is lost while an SDK call
is already in flight, PostgreSQL releases its slot and another process may
start a call; that failure window is outside the strict concurrency guarantee.
Runtime cannot recover provider `Retry-After` from a lost proxy response, and
the current proxy error body does not forward that header. Network ambiguity
may still cause a later proxy retry to repeat an external call; no
exactly-once execution or reversal of provider effects is promised.

Before changing quota bindings or deploying the migration, stop accepting new
tasks and drain or explicitly terminate active model calls. Apply the forward
migration, configure every real provider, update Runtime and API together,
check health and a controlled fake-provider call, then resume intake. Do not
mix old and new ownership binaries. A rollback needs its own tested data and
binary plan; simply starting the old binary after the new migration is not a
verified rollback.
