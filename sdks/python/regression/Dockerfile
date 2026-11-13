# The regression-check worker for a hosted cell (norn_regression.worker).
# Build from the repository root:
#   docker build -f sdks/python/regression/Dockerfile -t norn-regression-worker .
# Run with NORN_URL (https://<studio>/api/host), ORG, ENV and NORN_TOKEN_FILE
# (a mounted Studio API key). Stateless.
FROM python:3.11-slim@sha256:07d4824036af339fa1b6ef769cc99d9c10a141769538b52b3b597c84f7a7b535 AS client
WORKDIR /build
COPY sdks/python/requirements.txt sdks/python/prepare.py ./gen/
COPY sdks/openapi/norn-api.openapi.json ./
RUN python -m venv /gen && /gen/bin/pip install --no-cache-dir --require-hashes -r gen/requirements.txt \
 && python -I gen/prepare.py norn-api.openapi.json client-input.json \
 && PATH=/gen/bin:$PATH /gen/bin/openapi-python-client generate --path client-input.json --output-path /client/norn_client --meta none

FROM python:3.11-slim@sha256:07d4824036af339fa1b6ef769cc99d9c10a141769538b52b3b597c84f7a7b535
COPY sdks/python/regression/requirements.txt /app/requirements.txt
RUN pip install --no-cache-dir --require-hashes -r /app/requirements.txt && useradd --system --uid 10001 worker
COPY --from=client /client /app/client
COPY sdks/python/regression/norn_regression /app/norn_regression
ENV PYTHONPATH=/app/client:/app PYTHONUNBUFFERED=1
USER worker
ENTRYPOINT ["python", "-m", "norn_regression.worker"]
