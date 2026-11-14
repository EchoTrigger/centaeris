from api.title_queue import app, PROCESS
from .session_titles import process_session_title


@app.task(name=PROCESS, max_retries=0)
def generate_session_title(job_id):
    process_session_title(job_id)
