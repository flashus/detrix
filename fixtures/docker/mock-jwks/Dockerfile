FROM python:3.12-slim
WORKDIR /app
COPY server.py jwks.json ./
EXPOSE 8080
HEALTHCHECK --interval=2s --timeout=2s --retries=15 \
    CMD python -c "import urllib.request; urllib.request.urlopen('http://localhost:8080/jwks')"
CMD ["python", "/app/server.py"]
