FROM python:3.12.10-slim-bookworm@sha256:fd95fa221297a88e1cf49c55ec1828edd7c5a428187e67b5d1805692d11588db

WORKDIR /opt/mock
COPY server.py /opt/mock/server.py
EXPOSE 9999
CMD ["python", "/opt/mock/server.py"]
