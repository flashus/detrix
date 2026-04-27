FROM golang:1.24.13-bookworm AS builder
WORKDIR /src
COPY go.mod main.go ./
RUN --mount=type=cache,target=/root/.cache/go-build \
    --mount=type=cache,target=/go/pkg/mod \
    CGO_ENABLED=0 go build -ldflags="-s -w" -o /build/pricing-api .

FROM scratch
COPY --from=builder /build/pricing-api /usr/local/bin/pricing-api
EXPOSE 8080
CMD ["/usr/local/bin/pricing-api"]
