# Crawler Analytics Dashboard

A monitoring stack with **Grafana** (visualisation) and **ClickHouse** (analytics database).
Everything is configured as code: running `docker compose up` on a fresh machine will create the database, tables, datasource, and two dashboards automatically.

## Prerequisites

- [Docker Engine](https://docs.docker.com/engine/install/) (v20.10+)
- [Docker Compose](https://docs.docker.com/compose/install/) (v2 — ships with Docker Desktop; on Linux install the `docker-compose-plugin` package)

Verify both are installed:

```bash
docker --version
docker compose version
```

## Quick start

```bash
# 1. Define clickouse username and password:

## 1.1 In the ./docker-compose.yml file
CLICKHOUSE_USER: "" # Add here your clickhouse user
CLICKHOUSE_PASSWORD: "" # Add here your clickhouse password

## 1.2 In the ./docker/grafana/provisioning/datasources/clickhouse.yml file:
basicAuthUser: # Add here your clickhouse user (no "" needed)
basicAuthPassword: # Add here your clickhouse password (no "" needed)

# 2. Start everything (first run will download images — may take a few minutes)
docker compose up -d

# 3. Wait ~30 seconds for Grafana to install its plugins and start
```

That's it. Two containers will be running:

| Service    | Container name | URL                          | Purpose                       |
|------------|---------------|------------------------------|-------------------------------|
| Grafana    | `grafana`     | http://localhost:3000        | Dashboard UI                  |
| ClickHouse | `clickhouse`  | http://localhost:8123 (HTTP) | Analytics database            |

## Accessing Grafana

1. Open http://localhost:3000 in your browser.
2. Log in with the default credentials:
   - **Username:** `admin`
   - **Password:** `admin`
3. Grafana will ask you to change the password on first login. Choose a new one or click "Skip".
4. Two dashboards are already available — find them via the menu on the left: **Dashboards** (the four-squares icon).

### Pre-installed dashboards

- **Magento Crawler Analytics** — general crawl metrics (total URLs, durations, status codes, slow URLs, error log) plus a cache hit/miss ratio panel.

The dashboard has variable dropdowns at the top to filter by crawl ID, host, path, and URL. They will show data once rows are inserted into ClickHouse.

## Accessing ClickHouse

ClickHouse is the database that stores all crawl data. You normally don't need to interact with it directly — Grafana reads from it automatically.

If you do need to run queries manually:

```bash
# Open an interactive SQL shell inside the container
docker exec -it clickhouse clickhouse-client --user your_desired_username --password your_desired_password
```

Useful commands inside the shell:

```sql
SHOW DATABASES;           -- list databases
USE crawler;              -- switch to the crawler database
SHOW TABLES;              -- list tables
SELECT count() FROM crawl_logs;  -- count rows
```

Press `Ctrl+D` to exit the shell.

### Database credentials

| Setting  | Value       |
|----------|-------------|
| User     | `your_desired_username`   |
| Password | `your_desired_password` |
| Database | `crawler`   |
| HTTP port| `8123`      |
| Native port (mapped) | `9900` |

### Schema

The init script (`docker/clickhouse/init/001-init.sql`) creates:

- **`crawl_logs`** — raw log of every crawled URL (timestamp, status, duration, cache flag, etc.). Partitioned by day with a 30-day TTL.
- **`crawl_kpis`** — aggregated daily KPIs per crawl and host. 365-day TTL.
- **`mv_crawl_kpis`** — materialized view that automatically populates `crawl_kpis` from new rows inserted into `crawl_logs`.

## Inserting data

To see data in the dashboards you need to insert rows into `crawler.crawl_logs`. Example:

```bash
docker exec -it clickhouse clickhouse-client --user your_desired_username --password your_desired_password --query "
INSERT INTO crawler.crawl_logs (ts, crawl_id, status, cached, url, duration_ms, varnish_tags, cookie_hash)
VALUES
  (now(), 'my-first-crawl', 200, 0, 'https://example.com/page1', 120, '', ''),
  (now(), 'my-first-crawl', 404, 0, 'https://example.com/missing', 45, '', ''),
  (now(), 'my-first-crawl', 200, 1, 'https://example.com/page2', 8, '', '')
"
```

After inserting, open a dashboard in Grafana, select the crawl ID from the dropdown at the top, and adjust the time range to include your data.

## Common operations

### Stop the stack

```bash
docker compose down
```

Data is preserved in Docker volumes and the `.volumes/` directory. Next `docker compose up -d` will pick up where you left off.

### Stop and delete all data

```bash
docker compose down -v
rm -rf .volumes/
```

This removes everything — database files, Grafana settings, and all stored data. The next `docker compose up -d` will start completely fresh.

### View logs

```bash
# Both services
docker compose logs -f

# Only Grafana
docker compose logs -f grafana

# Only ClickHouse
docker compose logs -f ch-server
```

### Restart a single service

```bash
docker compose restart grafana
docker compose restart ch-server
```

## Project structure

```
.
├── docker-compose.yml
├── docker/
│   ├── clickhouse/
│   │   └── init/
│   │       └── 001-init.sql            # Creates database, tables, and materialized view
│   └── grafana/
│       └── provisioning/
│           ├── datasources/
│           │   └── clickhouse.yml      # ClickHouse datasource config
│           └── dashboards/
│               ├── dashboards.yml      # Tells Grafana where to find dashboard JSON files
│               └── json/
│                   ├── crawler-analytics.json
│                   └── magento-crawler-analytics.json
└── .volumes/                           # Created at runtime — ClickHouse data and logs
```

## Troubleshooting

**Grafana shows "No data" on dashboards**
- Make sure you've inserted rows into `crawler.crawl_logs` (see "Inserting data" above).
- Check that the time range picker (top-right in Grafana) covers the timestamps of your data.
- Select a crawl ID from the dropdown at the top of the dashboard.

**Grafana shows "Datasource not found" or "Plugin not found"**
- The ClickHouse plugin takes ~20-30 seconds to install on first boot. Wait and refresh the page.
- Check Grafana logs: `docker compose logs grafana`

**ClickHouse tables don't exist**
- The init script only runs on a fresh data directory. If you started ClickHouse before the init script was mounted, clear the data and restart:
  ```bash
  docker compose down -v
  rm -rf .volumes/
  docker compose up -d
  ```

**Port conflicts**
- If ports 3000, 8123, or 9900 are already in use, edit the left side of the port mappings in `docker-compose.yml` (e.g., change `'3000:3000'` to `'3001:3000'`).
