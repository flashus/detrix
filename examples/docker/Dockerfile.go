# Go Test App Docker Image
# Multi-stage: build fixture + Delve, slim runtime

# ---- Build Stage ----
FROM golang:1.24 AS builder

WORKDIR /src

# Copy Go client
COPY clients/go /src/clients/go

# Copy fixture (its go.mod has replace directive to local client)
COPY fixtures/go /src/fixtures/go

WORKDIR /src/fixtures/go

# Build with debug symbols (required for Delve)
RUN go build -gcflags="all=-N -l" -o /build/detrix_example_app .

# Install Delve
RUN go install github.com/go-delve/delve/cmd/dlv@latest

# ---- Runtime Stage ----
FROM debian:trixie-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Copy binary and Delve
COPY --from=builder /build/detrix_example_app /usr/local/bin/detrix_example_app
COPY --from=builder /go/bin/dlv /usr/local/bin/dlv

CMD ["detrix_example_app"]
