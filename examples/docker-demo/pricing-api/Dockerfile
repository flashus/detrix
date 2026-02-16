FROM golang:1.24 AS builder
WORKDIR /src
COPY go.mod main.go ./
RUN CGO_ENABLED=0 go build -ldflags="-s -w" -o /build/pricing-api .

FROM scratch
COPY --from=builder /build/pricing-api /usr/local/bin/pricing-api
EXPOSE 8080
CMD ["/usr/local/bin/pricing-api"]
