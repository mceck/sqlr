#!/bin/sh
# Runs inside the postgres-global container before the server starts.
set -eu

cp /certs/server.crt /certs/server.key "$PGDATA"/
chmod 600 "$PGDATA/server.key"
{
    echo "ssl = on"
    echo "ssl_cert_file = 'server.crt'"
    echo "ssl_key_file = 'server.key'"
} >>"$PGDATA/postgresql.conf"
