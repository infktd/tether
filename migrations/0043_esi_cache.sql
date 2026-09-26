-- ESI responses cached for eve-esi-client (its `EsiCache`), shared by every
-- worker and kept across restarts: ESI asks that a route isn't requested
-- again before its Expires, and stale entries are revalidated with their
-- ETag. Host-only: plugins never read it.
--
-- `principal` is the `sub` of the bearer token a response was fetched
-- with, or '' for an unauthenticated request, so one character's data is
-- never served to another. Pruned by expiry and a row cap
-- (`maintenance.prune`).
CREATE TABLE core.esi_cache (
    url text NOT NULL,
    principal text NOT NULL DEFAULT '',
    status smallint NOT NULL,
    -- Header names and values, pairwise (a name may repeat). Values are
    -- bytes: HTTP doesn't promise UTF-8.
    header_names text[] NOT NULL,
    header_values bytea[] NOT NULL,
    body bytea NOT NULL,
    etag text,
    -- NULL: never fresh, always revalidated with the ETag.
    expires_at timestamptz,
    stored_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (url, principal),
    CHECK (cardinality(header_names) = cardinality(header_values))
);

-- Bodies are read whole and replaced often: keep large ones out of line
-- and skip compressing them.
ALTER TABLE core.esi_cache ALTER COLUMN body SET STORAGE EXTERNAL;

-- For pruning: oldest first.
CREATE INDEX esi_cache_stored_at ON core.esi_cache (stored_at);
