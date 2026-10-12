from django.core.management.base import BaseCommand, CommandError

from app_core.note_history import collect_note_history_gc


class Command(BaseCommand):
    help = "Prune note history within byte/count/age policy; always retain live heads and protected versions."

    def add_arguments(self, parser):
        parser.add_argument("--dry-run", action="store_true")

    def handle(self, *args, **options):
        count, failures = collect_note_history_gc(options["dry_run"])
        self.stdout.write(f"revisions={count} failed={failures} dryRun={options['dry_run']}")
        if failures:
            raise CommandError("note_history_gc_failed")
