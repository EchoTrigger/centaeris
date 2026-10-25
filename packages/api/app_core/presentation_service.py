"""Application-scoped construction; no connection is opened by the factory."""
from django.db import connection, connections, transaction

from .http.stream_response import stream_database_call
from .presentation_changes import drain_pending_changes, presentation_change_keys, read_presentation_versions
from .presentation_notifications import listen_presentation_changes
from .presentation_observer import PresentationObserverHub


def _listener_connection_parameters():
    database = connections["default"]
    if database.vendor != "postgresql":
        raise RuntimeError("presentation changes require PostgreSQL")
    parameters = database.get_connection_params()
    # Django configures synchronous ORM cursors; LISTEN uses AsyncConnection's
    # own cursor and never obtains a lease from the request transaction pool.
    for name in ("cursor_factory", "row_factory", "pool"):
        parameters.pop(name, None)
    parameters["prepare_threshold"] = None
    return parameters


@stream_database_call
def _read_versions(scope):
    with transaction.atomic():
        with connection.cursor() as cursor:
            cursor.execute("SET LOCAL statement_timeout='5s'")
        return read_presentation_versions(scope.workspace_id, scope.agent_id, scope.session_id)


@stream_database_call
def _drain_pending():
    with transaction.atomic():
        with connection.cursor() as cursor:
            cursor.execute("SET LOCAL statement_timeout='5s'")
        return drain_pending_changes(limit=256)


def create_presentation_hub():
    def hints(ready):
        return listen_presentation_changes(ready, connection_parameters=_listener_connection_parameters)

    def change_keys(scope):
        return presentation_change_keys(scope.workspace_id, scope.agent_id, scope.session_id).values()

    return PresentationObserverHub(_read_versions, hints, change_keys, drain=_drain_pending)
