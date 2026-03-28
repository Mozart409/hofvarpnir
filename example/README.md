# Hofvarpnir Deployment Examples

Example deployment configurations for Hofvarpnir video archival service.

## Options

| Directory | Description | Best For |
|-----------|-------------|----------|
| [`docker/`](docker/) | Docker Compose | Local development, single-server deployments |
| [`kubernetes/`](kubernetes/) | Kubernetes manifests | Production, scalable deployments |

## Quick Start

### Docker Compose

```bash
cd example/docker
cp .env.example .env
docker compose up -d
```

Access at: http://localhost:8080

### Kubernetes

```bash
kubectl apply -k example/kubernetes/
kubectl port-forward -n hofvarpnir svc/hofvarpnir 8080:8080
```

Access at: http://localhost:8080

## Requirements

- **Docker**: Docker Engine 20.10+ and Docker Compose v2
- **Kubernetes**: kubectl 1.25+, cluster with PVC support
