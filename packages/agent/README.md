# Centaeris Agent

`centaeris-agent` provides the Rust Agent SDK. Core owns Session, Run, tool and
continuation semantics. Applications supply admission, persistence, model and
resource configuration through `SessionHost`.

## Native Sessions

An application registers its viewer, implements `SessionHost`, and passes that
host, its canonical event receiver and its disconnect token to `AgentClient::new`.
`create_session` and `open_session` return an `AgentSession` bound to the client.

`AgentSession` provides load, send, Run subscription, cancellation, supplements,
question answers, answer-now intervention, context compaction and Run listing.
`send` returns durable admission identities. The client's `wait_for_run` consumes
events until the Run has a durable outcome.

`resume_session` loads existing history and returns the Session with its current
Run subscription. The application host owns execution recovery and the Run identity.
Dropping a client detaches its viewer while accepted Runs remain host-owned.

The SDK dispatches synchronous host operations through Tokio's blocking pool and
owns asynchronous operations in Tokio tasks. Hosts retain admission permits for
the actual operation and provide their event stream's closure signal.

## Host composition

The shared local product host lives in
[Desktop services](../desktop/src/services/README.md). It composes the local
profile, SQLite store, model configuration, product services and RPC listener.
Desktop and CLI use that application host.

The [native Session example](../desktop/src/services/examples/session.rs) uses the
shared host with the SDK.

```sh
cargo run -p centaeris-desktop-services --example session
cargo run -p centaeris-desktop-services --example session -- "Explain this repository"
```

`local_execution_host` and `openssh` expose the reusable local execution adapters.
See [ExecutionHost](../../docs/architecture/ExecutionHostContract.md) for their
runtime boundary.
