# Magento Crawler

A fast, configurable crawler to **warm Magento cache**.

It crawls URLs listed across one or more CSV files and send requests with all provided `x-magento-vary` cookie combinations (customer groups, store views, currencies, etc.). This ensures pages are cached not only for anonymous users but also for the segments you care about (e.g., logged-in customers).

The repository also includes a `docker-compose.yml` to spin up **ClickHouse + Grafana** so you can observe the crawler in real time—errors, latency, throughput, and percent of valid pages.

---

## Table of Contents

- [Features](#features)
- [How it Works (x-magento-vary in a nutshell)](#how-it-works-x-magento-vary-in-a-nutshell)
- [Cache Hit Detection](#cache-hit-detection)
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
- Send email with 404, 500, 502 and 503 errors inside a csv report

---

## How it Works (x-magento-vary in a nutshell)

Magento uses the `x-magento-vary` cookie to encode the **cache context** — things like store, currency, theme, and customer segment. When paired with Varnish, caches may be **segmented** by these values. If you only warm URLs without setting the correct vary cookies, you risk caching **only the anonymous/default segment**.  
This crawler **replays your URLs** with each configured `x-magento-vary` value so that **every segment** gets a warm page in Varnish/Magento, reducing cold-cache latency spikes for real users.

> Note: Logged-in users are typically handled via ESI/hole-punching. For layouts that are cache-compatible, warming with appropriate vary values maximizes cache hits while keeping personalized fragments dynamic.

---

## Cache Hit Detection

Each response is flagged as cached or not:

- **With `cache_warmer_secret` set:** the crawler sends it in an `X-Cache-Warmer` request header, and a response is cached when Varnish answers `X-Cache: HIT` or `HIT-STALE`. This needs the VCL below.
- **Without it:** a response is cached when its `Age` header is above 0. Magento's default VCL removes `Age` outside developer mode, so keep it in your VCL if you rely on this. A hit within the first second after the page was stored also has `Age: 0` and counts as not cached.

Put this in `custom.vcl` next to your main VCL, with the same secret as `cache_warmer_secret`. Don't add a `vcl 4.0;` line to it. Then add `include "custom.vcl";` to the main VCL after the `backend`/`acl`/`import` declarations and before the first `sub`, so it runs before Magento's subroutines return.

```vcl
# custom.vcl
# Cache status header for the cache warmer (magento_crawler).
# Included from the main VCL before Magento's subroutines, so these run first.
# Requests carrying the secret X-Cache-Warmer header get X-Cache: HIT / HIT-STALE / MISS.

sub vcl_recv {
    # Remove any client-supplied copy of the internal flag so it can't be spoofed
    unset req.http.X-Cache-Debug;

    if (req.http.X-Cache-Warmer == "change-me-long-random-secret") {
        set req.http.X-Cache-Debug = "1";
    }

    # The secret never goes further than Varnish
    unset req.http.X-Cache-Warmer;
}

sub vcl_backend_fetch {
    # Don't forward the internal flag to Magento on a cache miss
    unset bereq.http.X-Cache-Debug;
}

sub vcl_deliver {
    # Set per response only, never stored with the cached object
    if (req.http.X-Cache-Debug) {
        if (obj.hits > 0) {
            if (obj.ttl <= 0s) {
                # Served past its TTL (grace) while Varnish refreshes it in the background
                set resp.http.X-Cache = "HIT-STALE";
            } else {
                set resp.http.X-Cache = "HIT";
            }
        } else {
            set resp.http.X-Cache = "MISS";
        }
    }
}
```

Check and reload without dropping the cache:

```bash
varnishd -C -f /etc/varnish/default.vcl > /dev/null
varnishreload # or: systemctl reload varnish
```

Requests without the secret never get `X-Cache`, so cache status isn't visible to the public.

> If the VCL is re-exported from Magento, re-add the `include` line.

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
   magento_crawler
   ```

4. Config can be overwritten via env variables. (see [ENV](#ENV)):

```bash 
# ovrwrites settings for concurrent requests
APP_APPLICATION__CONCURRENCY=50 magento_crawler

# ovrwrites settings for environment
APP_ENVIRONMENT=production magento_crawler
```
### Option B: Build From Source (Rust)

1. Install Rust (stable) via https://rustup.rs/

2. Clone the repository and build:

   ```bash
   cargo build --release
   ```

3. Copy compiled binary:

```bash
cp ./target/release/magento_crawler /usr/local/bin
chmod +x /usr/local/bin/magento_crawler
```

4. Run the compiled binary:

   ```bash
   magento_crawler
   ```

---

## Configuration

Create a file at:

| OS      | Location                                                            |
| ------- | ------------------------------------------------------------------- |
| Linux   | /home/user/.config/magento_crawler/config.toml                      |
| Windows | C:\Users\User\AppData\Roamingmagento_crawler/config.toml            |
| Mac     | /Users/user/Library/Application Support/magento_crawler/config.toml |

### Example Config

```toml
[application]
input_dir = "/home/arch/app/software/magento-crawler/.csv" # Location where our url CSVs will be placed
cookies = "208c4117651f131f68ee0849123a5e67a033e2b577d8f5d235158bad8229a9,5cb00000dc458965713a88114127d01051035195e2dad54194e7148a59195cb8" # All x-magento-vary cookies you need to warm, separated by comma
concurrency = 100 # How many queues will be running requests concurrently
save_to_clickhouse = false # If true, will add log requests to clickhouse database created with docker-compose
save_errors = true # If set to true, will create csv with errors in the server
send_email = true # Can send emails via sendmail (production env) or smtp unsafe (local env for tests). Save errors must be set to true
reports_folder = "/some/folder/to/saveReports/folderWithFiles" # where CSV reports will be stored
reports_server = "http://your-ip-or-url/folderWithFiles" # The url sent in the emails to open the reports, the folder in the reports_folder and server must be always the same (e.g. folderWithFiles)
sleep_timeout_seconds = 120 # 2 minutes
allowed_hosts = "example.com,www.example.com" # Required. Only http(s) URLs with exactly one of these hosts are crawled (comma separated, subdomains must be listed)
cache_warmer_secret = "change-me-long-random-secret" # Optional. Same secret as in custom.vcl, to detect cache hits with X-Cache. Without it, the Age header is used (see Cache Hit Detection)

[clickhouse]
clickhouse_client = "http://localhost:8123" # set in the .dashboard/docker-compose.yml - No need to change
clickhouse_user = "your_clickhouse_user" # set in the .dashboard/docker-compose.yml
clickhouse_pwd = "your_clickhouse_password" # set in the .dashboard/docker-compose.yml
clickhouse_db = "crawler" # always crawler

[telemetry]
enable_logging = true # Should output requests logs
simplified_logging = true # If set to true, outputs in one line with less info

[email]
send_to = "some@email.com" # sender for the reports email
send_from = "some@email.com" # receiver for the reports email
subject = "Finished running cache warmer for file" # The subject of the reports email
send_bcc= "email1@onedirect.com,email2@onedirect.com,email3@onedirect.com" # comma separated emails to receive the reports
```

## ENV
### Available Environment Variables

All settings from the config.toml can be overwritten with the following env variables

```bash
APP_ENVIRONMENT # local or production
APP_APPLICATION__INPUT_DIR
APP_APPLICATION__COOKIES
APP_APPLICATION__CONCURRENCY
APP_APPLICATION__SAVE_TO_CLICKHOUSE
APP_APPLICATION__SAVE_ERRORS
APP_APPLICATION__SEND_EMAIL
APP_APPLICATION__REPORTS_SERVER
APP_APPLICATION__REPORTS_FOLDER
APP_APPLICATION__SLEEP_TIMEOUT_SECONDS
APP_APPLICATION__ALLOWED_HOSTS
APP_APPLICATION__CACHE_WARMER_SECRET
APP_CLICKHOUSE__CLICKHOUSE_CLIENT
APP_CLICKHOUSE__CLICKHOUSE_USER
APP_CLICKHOUSE__CLICKHOUSE_PWD
APP_CLICKHOUSE__CLICKHOUSE_DB
APP_TELEMETRY__ENABLE_LOGGING
APP_TELEMETRY__SIMPLIFIED_LOGGING
APP_EMAIL__SEND_TO
APP_EMAIL__SEND_FROM
APP_EMAIL__SUBJECT
APP_EMAIL__SEND_BCC
```

### Input CSVs

- Place any number of `.csv` or `.txt` files under the folder defined at `input_dir`. Other extensions, hidden files (starting with `.`) and symlinks are skipped and left in place.
- Each file: **one URL per line**, no header.
- URLs that aren't `http`/`https` or whose host isn't in `allowed_hosts` are skipped with a warning.
- Example:
  ```
  https://example.com/
  https://example.com/category/phones
  https://example.com/product/sku-123
  ```

---

## Running the Crawler

```bash
magento_crawler
```

Exit code will be non-zero on fatal errors. When `save_to_clickhouse = true`, request/response metrics are written as the crawl proceeds.

---

## Observability Stack (ClickHouse + Grafana)

See information [@here](./.dashboard/README.md)

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


