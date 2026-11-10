from django.core.management.base import BaseCommand

from app_core.plugin_lifecycle import initialize_plugin_catalog


class Command(BaseCommand):
    help = "Create the explicit empty installed Plugin catalog when it is absent."

    def handle(self, *args, **options):
        initialize_plugin_catalog()
        self.stdout.write(self.style.SUCCESS("Verified installed Plugin catalog"))
