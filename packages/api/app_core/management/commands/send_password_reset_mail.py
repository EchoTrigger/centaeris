import time

from django.conf import settings
from django.core.management.base import BaseCommand

from app_core.password_reset import process_next_password_reset_mail


class Command(BaseCommand):
    help = "Send queued password reset email without logging recipients or reset URLs."

    def handle(self, *args, **options):
        if not settings.PASSWORD_RESET_ENABLED:
            self.stdout.write("Password reset mail sender is disabled.")
            return
        if not settings.PASSWORD_RESET_MAIL_SENDER:
            raise RuntimeError("PASSWORD_RESET_MAIL_SENDER must be 1")

        while True:
            if not process_next_password_reset_mail():
                time.sleep(2)
