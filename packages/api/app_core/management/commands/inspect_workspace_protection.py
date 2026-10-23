"""Inspect recorded snapshot protection without granting deletion authority."""
import json

from django.core.management.base import BaseCommand, CommandError

from app_core.models import Session
from app_core.workspace_snapshot_protection import collect_session_workspace_protection


class Command(BaseCommand):
    help = "Read one Session's known snapshot roots and unresolved coverage; never a deletion permit."

    def add_arguments(self, parser):
        parser.add_argument("--session-id", required=True)
        parser.add_argument("--max-records", type=int, default=1000,
            help="Per-source history budget, 1..10000; truncation remains unresolved.")

    def handle(self, *args, **options):
        try:
            inventory = collect_session_workspace_protection(options["session_id"], max_records=options["max_records"])
        except Session.DoesNotExist:
            raise CommandError("snapshot protection Session not found") from None
        except ValueError:
            raise CommandError("invalid snapshot protection scope or record budget") from None
        self.stdout.write(json.dumps({
            "schema": "workspace.snapshot_protection.v1", "sessionId": inventory.session_id,
            "deletionAuthorized": False,
            "knownRoots": [{"storageKey": root.storage_key, "sourceKind": root.source_kind,
                "sourceId": root.source_id, "agentRunId": root.agent_run_id} for root in inventory.roots],
            "unresolved": list(inventory.unresolved),
        }, sort_keys=True))