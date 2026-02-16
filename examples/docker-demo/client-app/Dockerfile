FROM golang:1.24 AS builder

WORKDIR /src

# Copy Go client (referenced via replace directive)
COPY clients/go /src/clients/go

# Copy order-service source
COPY examples/docker-demo/client-app /src/examples/docker-demo/client-app

WORKDIR /src/examples/docker-demo/client-app

# Build with debug symbols (required for Delve), static binary
RUN CGO_ENABLED=0 go build -gcflags="all=-N -l" -o /build/order-service .

# Install Delve (static binary)
RUN CGO_ENABLED=0 go install github.com/go-delve/delve/cmd/dlv@latest

FROM scratch

# TLS certificates for HTTPS connections
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/

# Copy binary and Delve
COPY --from=builder /build/order-service /usr/local/bin/order-service
COPY --from=builder /go/bin/dlv /usr/local/bin/dlv

CMD ["/usr/local/bin/order-service"]
