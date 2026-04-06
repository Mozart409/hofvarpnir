#!/bin/bash
# Configure PostgreSQL for monitoring extensions on existing database
set -e

PG_CONF="$PGDATA/postgresql.conf"

# Add pg_stat_statements to shared_preload_libraries if not already present
if ! grep -q "pg_stat_statements" "$PG_CONF"; then
    echo "shared_preload_libraries = 'pg_stat_statements'" >> "$PG_CONF"
    echo "pg_stat_statements.track = all" >> "$PG_CONF"
    echo "pg_stat_statements.max = 10000" >> "$PG_CONF"
    echo "PostgreSQL configuration updated. Restart required."
fi
