# The scanned-document extractor (`extract.py`): a one-shot OCR job (`run`)
# and the pull endpoint the cell's connector reads (`serve`). Build from the
# repository root:  docker build -f demo/documents/Dockerfile .
FROM python:3.13-slim-bookworm@sha256:a5a7017efc5969e00ae2b91c8eb0631049a569652aa4ffc4501769049ae45c72

RUN apt-get update \
    && apt-get install -y --no-install-recommends tesseract-ocr fonts-dejavu-core \
    && rm -rf /var/lib/apt/lists/* \
    && pip install --no-cache-dir pillow==11.3.0 pypdfium2==4.30.0 \
    && useradd --uid 10001 --create-home --shell /usr/sbin/nologin extractor \
    && mkdir -p /data && chown extractor /data

COPY demo/documents/extract.py demo/documents/generate.py /app/

USER 10001
VOLUME ["/data"]
EXPOSE 8080
ENTRYPOINT ["python3", "/app/extract.py"]
CMD ["serve", "--store", "/data"]
