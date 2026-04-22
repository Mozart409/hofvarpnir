# Hofvarpnir Kubernetes Deployment

Kubernetes manifests for deploying Hofvarpnir video archival service.

## Quick Start

```bash
# Apply all resources using Kustomize
kubectl apply -k example/kubernetes/

# Or apply individual files
kubectl apply -f example/kubernetes/namespace.yaml
kubectl apply -f example/kubernetes/configmap.yaml
kubectl apply -f example/kubernetes/secret.yaml
kubectl apply -f example/kubernetes/postgres.yaml
kubectl apply -f example/kubernetes/hofvarpnir.yaml
```

## Components

| File | Description |
|------|-------------|
| `namespace.yaml` | Creates the `hofvarpnir` namespace |
| `configmap.yaml` | Application configuration (non-sensitive) |
| `secret.yaml` | Database credentials (use proper secret management in production) |
| `postgres.yaml` | PostgreSQL StatefulSet with persistent storage |
| `hofvarpnir.yaml` | Main application Deployment with PVC for downloads |
| `ingress.yaml` | Optional Ingress for external access |
| `kustomization.yaml` | Kustomize configuration for easy deployment |

## Configuration

### Environment Variables

Edit `configmap.yaml` to adjust:

- `MAX_CONCURRENT_DOWNLOADS` - Number of parallel downloads (default: 3)
- `DOWNLOAD_TIMEOUT_HOURS` - Timeout for long downloads (default: 4)
- `MAX_DOWNLOAD_ATTEMPTS` - Retry attempts for failed downloads (default: 5)
- `RATE_LIMIT_DELAY_SECS` - Delay between rate-limited requests (default: 60)
- `RUST_LOG` - Logging level configuration

#### OIDC Authentication (Optional)

To enable OIDC single sign-on, add these to `configmap.yaml` (for non-sensitive values) or `secret.yaml` (for `OIDC_CLIENT_SECRET`):

| Variable | Description | Example |
|----------|-------------|---------|
| `OIDC_ISSUER` | OIDC provider URL | `https://auth.example.com` |
| `OIDC_CLIENT_ID` | OAuth2 client ID | `hofvarpnir` |
| `OIDC_CLIENT_SECRET` | OAuth2 client secret | (add to secret.yaml) |
| `OIDC_SCOPES` | Requested scopes | `openid,profile,email` |
| `OIDC_AUTO_PROVISION` | Auto-create users | `true` |
| `OIDC_REDIRECT_BASE_URL` | Callback base URL | `https://hof.example.com` |
| `OIDC_LOGOUT_REDIRECT` | Enable provider logout | `false` |
| `OIDC_DISCOVERY_TIMEOUT` | Discovery timeout (sec) | `30` |

### Secrets

**For production**, replace `secret.yaml` with a proper secret management solution:

- [Sealed Secrets](https://sealed-secrets.netlify.app/)
- [External Secrets Operator](https://external-secrets.io/)
- [HashiCorp Vault](https://www.vaultproject.io/)

### Storage

Adjust PVC sizes in:

- `postgres.yaml` - Database storage (default: 10Gi)
- `hofvarpnir.yaml` - Download storage (default: 100Gi)

## Exposing the Service

### Port Forward (Development)

```bash
kubectl port-forward -n hofvarpnir svc/hofvarpnir 8080:8080
```

### Ingress (Production)

1. Edit `ingress.yaml` with your domain and ingress controller annotations
2. Uncomment the ingress in `kustomization.yaml`
3. Re-apply: `kubectl apply -k example/kubernetes/`

### LoadBalancer

```bash
kubectl patch svc hofvarpnir -n hofvarpnir -p '{"spec": {"type": "LoadBalancer"}}'
```

## Monitoring

```bash
# Check pod status
kubectl get pods -n hofvarpnir

# View logs
kubectl logs -n hofvarpnir -l app.kubernetes.io/name=hofvarpnir -f

# Describe deployment
kubectl describe deployment hofvarpnir -n hofvarpnir
```

## Cleanup

```bash
kubectl delete -k example/kubernetes/
```
