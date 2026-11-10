import logging
import os
from functools import partial

from django.db import transaction


def _publish(identity):
    from kombu.exceptions import OperationalError
    from redis.exceptions import RedisError

    from api.material_queue import app, PROCESS

    try:
        app.send_task(PROCESS, args=(identity,), retry=False)
    except (OperationalError, RedisError):
        # The committed task remains available to the recovery sweep.
        logging.getLogger(__name__).warning("Material broker delivery failed")


def dispatch_after_commit(task):
    if task.status == "pending" and os.environ.get("MATERIAL_BROKER_URL"):
        transaction.on_commit(partial(_publish, task.pk))
