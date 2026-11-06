from datetime import timedelta
from functools import lru_cache

from api.material_queue import app, PROCESS, RECOVER, RECOVERY_INTERVAL

from .material_processor import MaterialWorker


@lru_cache(maxsize=1)
def worker():
    return MaterialWorker()


@app.task(name=PROCESS, acks_late=True, reject_on_worker_lost=True)
def process_material(identity):
    worker().process_one(identity)


@app.task(name=RECOVER)
def recover_material():
    # Only sweep work old enough to have missed its normal broker delivery.
    worker().recover_one(timedelta(seconds=2 * RECOVERY_INTERVAL))
