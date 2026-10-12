from django.core.management.base import BaseCommand, CommandError

from app_core.models import UserLibraryObject
from app_core.note_migration import export_note, migrate_note


class Command(BaseCommand):
    help = "Verify and backfill note bodies; retain original files. Export only during a rollback maintenance window."

    def add_arguments(self, parser):
        parser.add_argument("--dry-run", action="store_true")
        parser.add_argument("--limit", type=int, default=100)
        parser.add_argument("--export-for-rollback", action="store_true")

    def handle(self, *args, **options):
        if options["limit"] <= 0:
            raise CommandError("--limit must be positive")
        export = options["export_for_rollback"]
        query = UserLibraryObject.objects.filter(objectKind="note", purgedAt__isnull=True,
                                                 noteHead__isnull=not export).order_by("id")
        ids = list(query.values_list("id", flat=True)[:options["limit"]])
        operation = export_note if export else migrate_note
        failures = []
        for note_id in ids:
            try:
                state = operation(note_id, dry_run=options["dry_run"])
                self.stdout.write(f"{note_id}: {state}")
            except (OSError, ValueError) as error:
                failures.append(note_id)
                self.stderr.write(f"{note_id}: {type(error).__name__}")
        remaining = query.count()
        self.stdout.write(f"processed={len(ids)} failed={len(failures)} remaining={remaining}")
        if failures:
            raise CommandError("note_migration_failed")
