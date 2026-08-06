# syntax=docker/dockerfile:1
# check=skip=InvalidDefaultArgInFrom
ARG PYTHON_IMAGE
FROM ${PYTHON_IMAGE}
WORKDIR /app
COPY server.py jwks.json ./
EXPOSE 8080
HEALTHCHECK --interval=2s --timeout=2s --retries=15 \
    CMD python -c "import urllib.request; urllib.request.urlopen('http://localhost:8080/jwks')"
CMD ["python", "/app/server.py"]
