CREATE DATABASE IF NOT EXISTS crawler ENGINE = Atomic;

CREATE TABLE IF NOT EXISTS crawler.crawl_logs
(
    `ts`           DateTime64(3, 'UTC'),
    `crawl_id`     String,
    `status`       UInt16,
    `cached`       UInt8,
    `url`          String,
    `host`         String MATERIALIZED domain(url),
    `path`         String MATERIALIZED path(url),
    `duration_ms`  UInt32,
    `varnish_tags` String,
    `cookie_hash`  String,
    INDEX bf_path path TYPE tokenbf_v1(256, 2, 0) GRANULARITY 4
)
ENGINE = MergeTree
PARTITION BY toDate(ts)
ORDER BY (host, path, ts)
TTL ts + toIntervalDay(30)
SETTINGS index_granularity = 8192;

CREATE TABLE IF NOT EXISTS crawler.crawl_kpis
(
    `day`      Date,
    `crawl_id` String,
    `host`     String,
    `total`    UInt64,
    `s200`     UInt64,
    `s404`     UInt64,
    `s410`     UInt64,
    `s500`     UInt64,
    `s5xx`     UInt64,
    `avg_ms`   Float32
)
ENGINE = SummingMergeTree
PARTITION BY day
ORDER BY (day, crawl_id, host)
TTL day + toIntervalDay(365)
SETTINGS index_granularity = 8192;

CREATE MATERIALIZED VIEW IF NOT EXISTS crawler.mv_crawl_kpis TO crawler.crawl_kpis
(
    `day`      Date,
    `crawl_id` String,
    `host`     String,
    `total`    UInt64,
    `s200`     UInt64,
    `s404`     UInt64,
    `s410`     UInt64,
    `s5xx`     UInt64,
    `avg_ms`   Float64
)
AS SELECT
    toDate(ts)                                AS day,
    crawl_id,
    host,
    count()                                   AS total,
    sum(status = 200)                         AS s200,
    sum(status = 404)                         AS s404,
    sum(status = 410)                         AS s410,
    sum((status >= 500) AND (status < 600))   AS s5xx,
    avg(duration_ms)                          AS avg_ms
FROM crawler.crawl_logs
GROUP BY
    day,
    crawl_id,
    host;
