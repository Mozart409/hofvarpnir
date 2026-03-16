FROM postgres:17-trixie

# Install all contrib modules
RUN apt-get update && apt-get install -y \
  postgresql-contrib \
  && rm -rf /var/lib/apt/lists/*

# Copy initialization script to enable all extensions
COPY docker/init.sql /docker-entrypoint-initdb.d/

# Copy always-run initialization script for config updates
COPY docker/postgres-always-init.sh /docker-entrypoint-initdb.d/

# Configure shared_preload_libraries for pg_stat_statements
RUN echo "shared_preload_libraries = 'pg_stat_statements'" >> /usr/share/postgresql/postgresql.conf.sample && \
    echo "pg_stat_statements.track = all" >> /usr/share/postgresql/postgresql.conf.sample && \
    echo "pg_stat_statements.max = 10000" >> /usr/share/postgresql/postgresql.conf.sample

# Healthcheck to verify PostgreSQL is accepting connections
HEALTHCHECK --interval=10s --timeout=5s --start-period=30s --retries=3 \
  CMD pg_isready -U postgres || exit 1
