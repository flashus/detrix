# syntax=docker/dockerfile:1
# check=skip=InvalidDefaultArgInFrom
ARG GO_IMAGE
FROM ${GO_IMAGE} AS builder

WORKDIR /src

# Install Delve — cache mount keeps the module download across base image updates
RUN --mount=type=cache,target=/root/.cache/go-build \
    --mount=type=cache,target=/go/pkg/mod \
    CGO_ENABLED=0 go install github.com/go-delve/delve/cmd/dlv@v1.27.0

# Copy Go client (referenced via replace directive, no external deps)
COPY clients/go /src/clients/go

# Copy order-service source
COPY examples/docker-demo/client-app /src/examples/docker-demo/client-app

WORKDIR /src/examples/docker-demo/client-app

# Build with debug symbols (required for Delve), static binary
RUN --mount=type=cache,target=/root/.cache/go-build \
    --mount=type=cache,target=/go/pkg/mod \
    CGO_ENABLED=0 go build \
        -gcflags="all=-N -l" \
        -ldflags="-compressdwarf=false" \
        -o /build/order-service .

FROM scratch

# TLS certificates for HTTPS connections
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/

# Copy binary and Delve
COPY --from=builder /build/order-service /usr/local/bin/order-service
COPY --from=builder /go/bin/dlv /usr/local/bin/dlv

CMD ["/usr/local/bin/order-service"]
