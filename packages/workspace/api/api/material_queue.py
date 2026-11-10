import os

from celery import Celery


os.environ.setdefault("DJANGO_SETTINGS_MODULE", "api.settings")

PROCESS = "material.process"
RECOVER = "material.recover"
RECOVERY_INTERVAL = 5

app = Celery("centaeris_material", broker=os.environ.get("MATERIAL_BROKER_URL"),
             include=["app_core.material_queue_tasks"])
app.conf.update(
    task_default_queue="material",
    task_ignore_result=True,
    worker_prefetch_multiplier=1,
    broker_transport_options={"socket_connect_timeout": 1, "socket_timeout": 1},
    beat_schedule={
        "recover-material": {
            "task": RECOVER,
            "schedule": RECOVERY_INTERVAL,
            "options": {"expires": RECOVERY_INTERVAL},
        },
    },
)
