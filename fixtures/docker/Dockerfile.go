# Go Test App Docker Image
# Multi-stage: static build of fixture + Delve, scratch runtime

# ---- Build Stage ----
FROM golang:1.24 AS builder

WORKDIR /src

# Copy Go client
COPY clients/go /src/clients/go

# Copy fixture (its go.mod has replace directive to local client)
COPY fixtures/go /src/fixtures/go

WORKDIR /src/fixtures/go

# Build with debug symbols (required for Delve), static binary (no CGO)
RUN CGO_ENABLED=0 go build -gcflags="all=-N -l" -o /build/detrix_example_app .

# Install Delve (static binary)
RUN CGO_ENABLED=0 go install github.com/go-delve/delve/cmd/dlv@latest

# ---- Runtime Stage (scratch — no OS, ~30MB total) ----
FROM scratch

# TLS certificates for HTTPS connections
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/

# Copy binary and Delve
COPY --from=builder /build/detrix_example_app /usr/local/bin/detrix_example_app
COPY --from=builder /go/bin/dlv /usr/local/bin/dlv

CMD ["/usr/local/bin/detrix_example_app"]
