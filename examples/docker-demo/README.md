# Docker Demo

This demo runs a small pricing API, an order service, and a Detrix daemon.

## Embedded Client Mode

The default mode keeps the existing behavior: `order-service` embeds the Detrix Go client and registers with the daemon directly.

```sh
./examples/docker-demo/run.sh --mode client
```

## Agent / eBPF Mode

Agent mode disables the embedded Go client and starts a privileged `detrix-agent` sidecar. The sidecar shares the `order-service` PID namespace and uses eBPF uprobes to observe the Go process.

```sh
./examples/docker-demo/run.sh --mode agent
```

This mode needs a Docker runtime that supports privileged Linux containers and eBPF. If the agent starts but probes fail, check the agent logs first:

```sh
docker compose -f examples/docker-demo/client-app/docker-compose.yml logs -f detrix-agent
```

## Stop

```sh
./examples/docker-demo/run.sh --down
```
