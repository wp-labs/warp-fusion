# Kafka Pipeline: wpgen → wparse → Kafka → wfusion → Kafka

End-to-end streaming detection with Kafka as message bus.

## Architecture

```
[wpgen]           [wparse]            [Kafka]              [wfusion]
  │                  │                   │                     │
  │  nginx logs      │  parse → JSON     │  wp_nginx_logs      │  detect
  │  (NDJSON)        │  → Kafka          │                     │  → Kafka
  │                  │                   │                     │
  ▼                  ▼                   ▼                     ▼
conn_events.ndjson  ──read──▶  wp_nginx_logs  ──consume──▶  wp_alerts_*
```

## Prerequisites

- Docker & docker-compose
- wpgen, wparse, wfusion in PATH

## Quick Start

```bash
# 1. Start Kafka
docker-compose up -d

# 2. Init wparse project
cd wparse && wproj init -m full && cd ..

# 3. Generate data + parse → Kafka
cd wparse
wpgen sample -n 5000
wparse batch -p -n 5000 -S 1
cd ..

# 4. Start wfusion (consumes from Kafka, writes alerts to Kafka)
cd wfusion
wfusion run --config conf/wfusion.toml
```
