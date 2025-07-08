# Stitch Server Helm Chart

A minimal Helm chart for deploying the Stitch gRPC server.

## Quick Start

```bash
# Install the chart
helm install stitch ./helm

# Forward gRPC port for testing
kubectl port-forward svc/stitch 50051:50051
```

## Configuration

Key configuration values:

```yaml
# values.yaml
image:
  repository: stitch-server
  tag: "latest"

config:
  server:
    port: "50051"
  database:
    url: ""  # Set to your external PostgreSQL connection string
  twitch:
    clientId: "your-client-id"
    clientSecret: "your-client-secret"
    webhookUrl: "https://your-domain.com/webhooks/twitch"
  webhook:
    secret: "your-webhook-secret"
    port: "50052"
    url: "https://your-domain.com/webhooks/twitch"
  discord:
    token: "your-discord-token"
    channel: "your-discord-channel-id"

# PostgreSQL is no longer managed by this chart. Please install and configure it separately.
```

## Usage

```bash
# Install with custom values
helm install stitch ./helm -f my-values.yaml

# Upgrade
helm upgrade stitch ./helm

# Uninstall
helm uninstall stitch
```

The following table lists the configurable parameters of the stitch chart and their default values.

| Key | Type | Default | Description |
|---|---|---|---|
| image.repository | string | stitch-server | Container image repository |
