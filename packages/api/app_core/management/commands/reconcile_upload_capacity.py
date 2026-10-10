from django.core.management.base import BaseCommand, CommandError
from app_core.upload_capacity import reconcile_ingress_pool


class Command(BaseCommand):
    help = "Clean exact owned request namespaces after every writer sharing this temporary pool has stopped."

    def add_arguments(self, parser):
        parser.add_argument("--api-workers-stopped", action="store_true")

    def handle(self, *args, **options):
        if not options["api_workers_stopped"]:
            raise CommandError("Stop every API writer sharing this pool and confirm with --api-workers-stopped")
        try:
            count = reconcile_ingress_pool()
        except (ValueError, OSError) as error:
            raise CommandError(str(error)) from error
        self.stdout.write(f"Confirmed cleanup for {count} request namespaces; released their temporary capacity")
