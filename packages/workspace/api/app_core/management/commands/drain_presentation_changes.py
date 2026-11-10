"""Advance one bounded presentation invalidation batch outside browser traffic."""
from django.core.management.base import BaseCommand, CommandError

from app_core.presentation_changes import drain_pending_changes


class Command(BaseCommand):
    help = "Project one bounded batch of retained presentation invalidations."

    def add_arguments(self, parser):
        parser.add_argument("--limit", type=int, default=256)
        parser.add_argument("--database", default="default")

    def handle(self, *args, **options):
        try:
            consumed = drain_pending_changes(options["limit"], using=options["database"])
        except ValueError as error:
            raise CommandError(str(error)) from error
        self.stdout.write(f"Projected {consumed} retained presentation invalidations.")
