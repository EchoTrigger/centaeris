from django.core.management.base import BaseCommand, CommandError
from app_core.native_archive import (
    NativeArchiveBlocked,
    require_native_archive_activation,
)


class Command(BaseCommand):
    help = "Refuse native activation while archived execution duties remain unsettled."

    def handle(self, *args, **options):
        try:
            require_native_archive_activation()
        except NativeArchiveBlocked as error:
            raise CommandError(str(error)) from None
        self.stdout.write("Native archive activation checks passed.")
