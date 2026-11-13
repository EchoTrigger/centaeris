import os

from celery import Celery

os.environ.setdefault("DJANGO_SETTINGS_MODULE", "api.settings")
PROCESS = "session_title.process"
app = Celery("centaeris_titles", broker=os.environ.get("MATERIAL_BROKER_URL"),
             include=["app_core.session_title_tasks"])
app.conf.update(
    task_default_queue="session_titles", task_ignore_result=True,
    worker_prefetch_multiplier=1, task_publish_retry=False,
    broker_transport_options={"socket_connect_timeout": 0.2, "socket_timeout": 0.2},
)
