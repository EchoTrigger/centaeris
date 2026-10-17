FROM python:3.12.10-slim-bookworm

WORKDIR /opt/monitor
COPY --chmod=0444 workspace_monitor.py /opt/monitor/workspace_monitor.py
COPY --chmod=0444 workspace_monitor/index.html workspace_monitor/app.js workspace_monitor/style.css /opt/monitor/workspace_monitor/
RUN mkdir -p /job && chmod 0555 /job /opt/monitor /opt/monitor/workspace_monitor

USER 65534:65534
ENTRYPOINT ["python", "-I", "-B", "/opt/monitor/workspace_monitor.py", "--job-dir", "/job", "--port", "9000"]
