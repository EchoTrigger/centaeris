from django.core.management.base import BaseCommand, CommandError

from app_core.models import UserLibraryObject
from app_core.note_history import collect_history


class Command(BaseCommand):
    help = "Prune note history within byte/count/age policy; always retain live heads and protected versions."

    def add_arguments(self, parser):
        parser.add_argument("--dry-run", action="store_true")

    def handle(self, *args, **options):
        failures = 0
        ids = UserLibraryObject.objects.filter(objectKind="note", noteHead__isnull=False).values_list("id", flat=True)
        for note_id in ids.iterator():
            try:
                planned = collect_history(note_id, dry_run=options["dry_run"])
                self.stdout.write(f"{note_id}: revisions={len(planned)} dryRun={options['dry_run']}")
            except ValueError:
                failures += 1
                self.stderr.write(f"{note_id}: history_policy_failed")
        if failures:
            raise CommandError("note_history_gc_failed")
