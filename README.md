# Magento Crawler

A fast, configurable crawler to **warm Magento cache**.

It crawls URLs listed across one or more CSV files and send requests with all provided `x-magento-vary` cookie combinations (customer groups, store views, currencies, etc.). This ensures pages are cached not only for anonymous users but also for the segments you care about (e.g., logged-in customers).

The repository also includes a `docker-compose.yml` to spin up **ClickHouse + Grafana** so you can observe the crawler in real time—errors, latency, throughput, and percent of valid pages.

---

## Table of Contents

- [Features](#features)
- [How it Works (x-magento-vary in a nutshell)](#how-it-works-x-magento-vary-in-a-nutshell)
- [Quick Start](#quick-start)
    - [Option A: Use a Release Binary](#option-a-use-a-release-binary)
    - [Option B: Build From Source (Rust)](#option-b-build-from-source-rust)
- [Configuration](#configuration)
    - [Config JSON Schema](#config-json-schema)
    - [Example Config](#example-config)
    - [Input CSVs](#input-csvs)
- [Running the Crawler](#running-the-crawler)
- [Observability Stack (ClickHouse + Grafana)](#observability-stack-clickhouse--grafana)
    - [Bring Up the Stack](#bring-up-the-stack)
    - [Default ClickHouse Settings](#default-clickhouse-settings)
    - [Suggested Table (DDL)](#suggested-table-ddl)
    - [Grafana](#grafana)
- [Operational Tips](#operational-tips)
- [FAQ](#faq)
- [License](#license)

---

## Features

- Reads **all CSV files** in a given folder (one URL per line).
- Sends requests with **all configured `x-magento-vary` cookie values** to warm each relevant cache segment (customer groups, store view/currency variants, and both logged-in/logged-out compatible layouts).
- **Concurrent** fetches (you control the concurrency).
- Optional **ClickHouse logging** for detailed metrics and **Grafana dashboards** via `docker-compose.yml`.
- Tracks **errors, latency, throughput, and percent of valid pages**.

---

## How it Works (x-magento-vary in a nutshell)

Magento uses the `x-magento-vary` cookie to encode the **cache context**—things like store, currency, theme, and customer segment. When paired with Varnish, caches may be **segmented** by these values. If you only warm URLs without setting the correct vary cookies, you risk caching **only the anonymous/default segment**.  
This crawler **replays your URLs** with each configured `x-magento-vary` value so that **every segment** gets a warm page in Varnish/Magento, reducing cold-cache latency spikes for real users.

> Note: Logged-in users are typically handled via ESI/hole-punching. For layouts that are cache-compatible, warming with appropriate vary values maximizes cache hits while keeping personalized fragments dynamic.

---

## Quick Start

### Option A: Use a Release Binary

1. Download the release binary for your platform:
    - Ubuntu/Linux (glibc)
    - macOS (Apple Silicon / Intel)
    - Amazon Linux (AMLinux)

2. Copy the binary into your system path. For Ubuntu:
   ```bash
   sudo cp magento_crawler /usr/local/bin/
   sudo chmod +x /usr/local/bin/magento_crawler
   ```

3. Prepare a config file (see [Configuration](#configuration)) and run:
   ```bash
   magento_crawler -s /path/to/config.json
   ```

### Option B: Build From Source (Rust)

1. Install Rust (stable) via https://rustup.rs/
2. Clone the repository and build:
   ```bash
   cargo build --release
   ```
3. Run the compiled binary:
   ```bash
   ./target/release/magento_crawler -s /path/to/config.json
   ```

---

## Configuration

Create a JSON config file (any filename, any location) and pass it with `-s /path/to/config.json`.

### Config JSON Schema

| Field                  | Type     | Required | Description                                                                                                                      |
|----------------------- |----------|----------|----------------------------------------------------------------------------------------------------------------------------------|
| `input_dir`            | string   | yes      | Absolute path of the folder containing CSV files. Each CSV must list one URL per line.                                           |
| `cookies`              | string   | yes      | **Comma-separated** `x-magento-vary` cookie values to warm. Example: `"cookieA,cookieB,cookieC"`                                 |
| `concurrency`          | integer  | yes      | Number of simultaneous requests (e.g., `30`). Tune to your infra.                                                                |
| `save_to_clickhouse`   | boolean  | yes      | If `true`, crawler writes metrics to ClickHouse.                                                                                 |
| `clickhouse_client`    | string   | yes*     | ClickHouse HTTP endpoint. With default `docker-compose.yml`: `http://localhost:8123`. Required when `save_to_clickhouse = true`. |
| `clickhouse_user`      | string   | yes*     | Username defined in `docker-compose.yml`. Required when saving to ClickHouse.                                                    |
| `clickhouse_pwd`       | string   | yes*     | Password defined in `docker-compose.yml`. Required when saving to ClickHouse.                                                    |
| `clickhouse_db`        | string   | yes*     | Database name defined in `docker-compose.yml`. Required when saving to ClickHouse.                                               |
| `enable_logging`       | string   | yes*     | If set to true, will output logs                                                                                                 |
| `simplified_logging`   | string   | yes*     | If set to true, will output a simplified log version to use less disk space                                                      |
### Example Config

```json
{
  "input_dir": "/var/data/crawler/urls",
  "cookies": "customerGroupDefault,customerGroupVIP,storeEN,storeES,currencyEUR",
  "concurrency": 30,
  "save_to_clickhouse": true,
  "clickhouse_client": "http://localhost:8123",
  "clickhouse_user": "crawler",
  "clickhouse_pwd": "crawler_pwd",
  "clickhouse_db": "crawler_metrics",
  "enable_logging": true,
  "simplified_logging": true
}
```

### Input CSVs

- Place any number of CSV files under `input_dir`.
- Each file: **one URL per line**, no header.
- Example:
  ```
  https://example.com/
  https://example.com/category/phones
  https://example.com/product/sku-123
  ```

---

## Running the Crawler

```bash
# Using a prebuilt binary placed in your PATH:
magento_crawler -s /path/to/config.json

# Or from a local build:
./target/release/magento_crawler -s /path/to/config.json
```

Exit code will be non-zero on fatal errors. When `save_to_clickhouse = true`, request/response metrics are written as the crawl proceeds.

---

## Observability Stack (ClickHouse + Grafana)

The repo ships with a `docker-compose.yml` that brings up **ClickHouse** and **Grafana** for real-time visibility:
- **Errors**: HTTP status codes, timeouts, connection errors.
- **Latency**: p50/p90/p99 response times.
- **Throughput**: requests per second.
- **Validity**: % of responses considered **valid** (e.g., 2xx/3xx or custom rule).

### Bring Up the Stack

```bash
docker compose up -d
```

This starts:
- ClickHouse server on `http://localhost:8123`
- Grafana on `http://localhost:3000`

> Credentials and database defaults match the example config above (adjust in `docker-compose.yml` as needed).

### Default ClickHouse Settings

- **Client URL**: `http://localhost:8123`
- **User**: `crawler`
- **Password**: `crawler_pwd`
- **Database**: `crawler_metrics`

### Suggested Table (DDL)

If the crawler does not auto-provision, you can create a table like:

```sql
CREATE DATABASE IF NOT EXISTS crawler_metrics;

CREATE TABLE IF NOT EXISTS crawler_metrics.events (
  ts                DateTime DEFAULT now(),
  url               String,
  cookie_value      String,      -- x-magento-vary value used for the request
  status_code       UInt16,
  ok                UInt8,       -- 1 if valid page (configurable rule), else 0
  latency_ms        UInt32,
  bytes_received    UInt64,
  error_message     String       -- empty if none
)
ENGINE = MergeTree
PARTITION BY toDate(ts)
ORDER BY (ts, url, cookie_value);
```

> Feel free to extend with fields like `method`, `attempt`, `retry`, `trace_id`, etc.

### Grafana

- Open Grafana at `http://localhost:3000` and add ClickHouse as a data source.
- Import or create dashboards with panels for:
    - **Requests over time** (rate)
    - **Latency** (p50/p90/p99)
    - **Error rate** by status code
    - **% Valid pages** over time
    - **Top slow URLs** and **Top erroring URLs**
    - **Breakdowns** by `cookie_value` (to spot problematic segments)

---

## Operational Tips

- **Concurrency**: Start conservatively (e.g., `10–30`). Increase gradually while monitoring error rates, backend CPU, and DB load.
- **Cookie Coverage**: Keep `cookies` current with the segments your storefront uses. Remove obsolete values to avoid waste.
- **Health Checks**: Warm a representative subset frequently (cron) and run full warms off-peak.
- **Respect Robots/Rate Limits**: This tool targets your own Magento; still, avoid saturating upstreams (payment/CDN/3rd-party APIs).
- **Logged-In Views**: For layouts that support caching with ESI/hole-punch, warming by `x-magento-vary` helps ensure the **outer** page is hot while dynamic fragments remain personalized.

---

## FAQ

**Does this cache logged-in pages?**  
Magento typically uses ESI/hole-punch for personalized fragments. The crawler warms the cacheable **shell** for each segment via `x-magento-vary`. Personalized blocks are fetched separately at request time.

**Do I need ClickHouse/Grafana?**  
No. Set `save_to_clickhouse = false` to disable. It’s recommended for visibility during rollouts/tuning.

**What’s considered a “valid” page?**  
By default, 2xx/3xx responses count as valid. You can refine this in the code (e.g., verify specific headers or body markers).

---

## License

MIT (or your chosen license). See `LICENSE` file.