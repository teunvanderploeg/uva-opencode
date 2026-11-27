# UvA AI Chat bridge for OpenCode in a container (README.md).
FROM python:3.12-slim-bookworm

# Chromium lives where a non-root user can run it. Xvnc (TigerVNC) and noVNC give the one-time login window a screen.
ENV PYTHONUNBUFFERED=1 \
    PLAYWRIGHT_BROWSERS_PATH=/opt/ms-playwright \
    DISPLAY=:99 \
    UVA_STATE_DIR=/data \
    UVA_BIND=0.0.0.0 \
    UVA_CHROME_SANDBOX=0 \
    HOME=/tmp

# libatomic1 is needed by Playwright's node driver before `playwright install --with-deps` can add it.
RUN apt-get update \
 && apt-get install -y --no-install-recommends tigervnc-standalone-server tigervnc-tools novnc websockify ca-certificates libatomic1 \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY requirements.txt .
RUN pip install --no-cache-dir -r requirements.txt \
 && playwright install --with-deps chromium \
 && rm -rf /var/lib/apt/lists/*

# opencode.example.json is the model catalog (prompt tools); /v1/models reads it.
COPY bridge.py browser_session.py runtime_paths.py opencode.example.json entrypoint.sh ./
RUN chmod 755 /app/entrypoint.sh \
 && useradd --uid 10001 --no-create-home --shell /usr/sbin/nologin uva \
 && mkdir /data && chown 10001:10001 /data

USER 10001
EXPOSE 8765 6080
ENTRYPOINT ["/app/entrypoint.sh"]
