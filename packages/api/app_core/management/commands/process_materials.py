import logging
import signal
import threading

from django.core.management.base import BaseCommand

from app_core.material_processor import MaterialWorker


class Command(BaseCommand):
    help = "Run the platform-owned document processor Worker"

    def handle(self, *args, **options):
        stopped = threading.Event()
        for number in (signal.SIGTERM, signal.SIGINT):
            signal.signal(number, lambda *_: stopped.set())
        worker = MaterialWorker()
        while not stopped.is_set():
            try:
                if not worker.run_once(stopped):
                    stopped.wait(2)
            except Exception as error:
                logging.getLogger(__name__).warning("Material worker attempt failed: %s", type(error).__name__)
                stopped.wait(5)
