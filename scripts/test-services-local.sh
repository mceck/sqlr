#!/bin/sh
# Starts the fixed test services as native processes, without Docker.
#
# Postgres, MySQL and Redis run from the locally installed binaries on the same ports
# docker-compose.test.yml publishes, so the tests need no environment variables. Data
# lives in .test-services/local and survives runs; `reset` recreates it from empty.
set -eu

cd "$(dirname "$0")/.."

BASE="$PWD/.test-services/local"
CERTS="$PWD/.test-services/certs"
mkdir -p "$BASE"

PG_PORT=55432
PG_GLOBAL_PORT=55433
MYSQL_PORT=53306
MYSQL_GLOBAL_PORT=53310
MYSQL_NOTLS_PORT=53312
REDIS_PORT=56379
REDIS_TLS_PORT=56380

say() {
    printf 'test-services: %s\n' "$*"
}

fail() {
    say "$*" >&2
    exit 1
}

require() {
    command -v "$1" >/dev/null 2>&1 || fail "missing \`$1\`; install it (macOS: brew install postgresql@18 mysql redis) or run \`SQLR_TEST_SERVICES=docker make test\`"
}

wait_for() {
    what=$1
    shift
    deadline=$(( $(date +%s) + ${SQLR_TEST_SERVICE_TIMEOUT:-120} ))
    while ! "$@" >/dev/null 2>&1; do
        [ "$(date +%s)" -lt "$deadline" ] ||
            fail "timed out waiting for $what (see $BASE/*.log)"
        sleep 0.2
    done
}

pg_ready() {
    pg_isready -h 127.0.0.1 -p "$1" -U postgres >/dev/null 2>&1
}

# --- PostgreSQL -----------------------------------------------------------------------

pg_start() {
    name=$1
    port=$2
    tls=$3
    dir="$BASE/$name"

    if [ ! -f "$dir/PG_VERSION" ]; then
        say "initializing $name"
        mkdir -p "$dir"
        pw="$BASE/.$name.pw"
        printf 'postgres\n' >"$pw"
        initdb -D "$dir" -U postgres --pwfile="$pw" \
            --auth-local=trust --auth-host=scram-sha-256 \
            --encoding=UTF8 --locale=C --no-sync >/dev/null
        rm -f "$pw"
        cat >>"$dir/postgresql.conf" <<EOF
listen_addresses = '127.0.0.1'
port = $port
unix_socket_directories = ''
fsync = off
synchronous_commit = off
full_page_writes = off
max_connections = 100
EOF
        if [ "$tls" -eq 1 ]; then
            cp "$CERTS/server.crt" "$dir/server.crt"
            cp "$CERTS/server.key" "$dir/server.key"
            chmod 600 "$dir/server.key"
            cat >>"$dir/postgresql.conf" <<EOF
ssl = on
ssl_cert_file = 'server.crt'
ssl_key_file = 'server.key'
EOF
        fi
    fi

    if pg_ctl -D "$dir" status >/dev/null 2>&1; then
        return
    fi
    ensure_port_free "$port"
    rm -f "$dir/postmaster.pid"
    say "starting $name on $port"
    pg_ctl -D "$dir" -l "$dir/server.log" -w -t 120 start >/dev/null ||
        fail "could not start $name (see $dir/server.log)"
    wait_for "$name" pg_ready "$port"
}

pg_stop() {
    dir="$BASE/$1"
    if [ -f "$dir/PG_VERSION" ] && pg_ctl -D "$dir" status >/dev/null 2>&1; then
        say "stopping $1"
        pg_ctl -D "$dir" stop -m fast -w -t 60 >/dev/null || true
    fi
}

# --- MySQL ----------------------------------------------------------------------------

mysql_alive() {
    dir=$1
    [ -f "$dir/mysqld.pid" ] && kill -0 "$(cat "$dir/mysqld.pid")" 2>/dev/null
}

mysql_ready() {
    mysqladmin --protocol=tcp -h127.0.0.1 -P"$1" -uroot ping --silent >/dev/null 2>&1
}

mysql_start() {
    name=$1
    port=$2
    extra=${3:-}
    dir="$BASE/$name"

    if [ ! -d "$dir/mysql" ]; then
        say "initializing $name"
        mkdir -p "$dir"
        mysqld --initialize-insecure --datadir="$dir" \
            --log-error="$dir/init.log" >/dev/null 2>&1 ||
            fail "could not initialize $name (see $dir/init.log)"
    fi

    if mysql_alive "$dir"; then
        return
    fi
    ensure_port_free "$port"
    rm -f "$dir/mysqld.pid"
    say "starting $name on $port"
    # shellcheck disable=SC2086 # `extra` is a deliberately split list of options.
    mysqld --datadir="$dir" --port="$port" --bind-address=127.0.0.1 \
        --socket="$dir/mysql.sock" --pid-file="$dir/mysqld.pid" \
        --log-error="$dir/server.log" --mysqlx=0 --skip-log-bin \
        --innodb-buffer-pool-size=128M --key-buffer-size=16M \
        $extra --daemonize ||
        fail "could not start $name (see $dir/server.log)"
    wait_for "$name" mysql_ready "$port"

    # The image in docker-compose.test.yml creates these; a fresh --initialize datadir
    # only has root@localhost.
    mysql --protocol=tcp -h127.0.0.1 -P"$port" -uroot <<'SQL' >/dev/null
CREATE USER IF NOT EXISTS 'root'@'%' IDENTIFIED BY '';
ALTER USER 'root'@'%' IDENTIFIED BY '';
GRANT ALL PRIVILEGES ON *.* TO 'root'@'%' WITH GRANT OPTION;
CREATE DATABASE IF NOT EXISTS test;
SQL
}

mysql_stop() {
    dir="$BASE/$1"
    port=$2
    if mysql_alive "$dir"; then
        say "stopping $1"
        mysqladmin --protocol=tcp -h127.0.0.1 -P"$port" -uroot shutdown >/dev/null 2>&1 ||
            kill "$(cat "$dir/mysqld.pid")" 2>/dev/null || true
        deadline=$(( $(date +%s) + 30 ))
        while mysql_alive "$dir" && [ "$(date +%s)" -lt "$deadline" ]; do
            sleep 0.2
        done
    fi
}

# --- Redis ----------------------------------------------------------------------------

redis_alive() {
    dir=$1
    [ -f "$dir/redis.pid" ] && kill -0 "$(cat "$dir/redis.pid")" 2>/dev/null
}

redis_start() {
    name=$1
    port=$2
    dir="$BASE/$name"
    shift 2
    if redis_alive "$dir"; then
        return
    fi
    ensure_port_free "$port"
    rm -f "$dir/redis.pid"
    mkdir -p "$dir"
    say "starting $name on $port"
    redis-server "$@" --bind 127.0.0.1 \
        --save '' --appendonly no --dir "$dir" --daemonize yes \
        --pidfile "$dir/redis.pid" --logfile "$dir/redis.log"
    deadline=$(( $(date +%s) + 60 ))
    while ! redis_ready "$name" "$port"; do
        [ "$(date +%s)" -lt "$deadline" ] ||
            fail "timed out waiting for $name (see $dir/redis.log)"
        sleep 0.2
    done
}

redis_ready() {
    if [ "$1" = "redis-tls" ]; then
        redis-cli --tls --cacert "$CERTS/ca.crt" -h localhost -p "$2" \
            --user alice --pass s3cret ping >/dev/null 2>&1
    else
        redis-cli -h 127.0.0.1 -p "$2" ping >/dev/null 2>&1
    fi
}

redis_stop() {
    dir="$BASE/$1"
    if redis_alive "$dir"; then
        say "stopping $1"
        kill "$(cat "$dir/redis.pid")" 2>/dev/null || true
    fi
}

redis_tls_start() {
    redis_start redis-tls "$REDIS_TLS_PORT" \
        --port 0 --tls-port "$REDIS_TLS_PORT" \
        --tls-cert-file "$CERTS/server.crt" \
        --tls-key-file "$CERTS/server.key" \
        --tls-ca-cert-file "$CERTS/ca.crt" \
        --tls-auth-clients no \
        --user alice on '>s3cret' '~*' '&*' '+@all'
}

# --- Commands -------------------------------------------------------------------------

port_in_use() {
    command -v lsof >/dev/null 2>&1 &&
        lsof -nP -iTCP:"$1" -sTCP:LISTEN >/dev/null 2>&1
}

ensure_port_free() {
    if port_in_use "$1"; then
        fail "port $1 is already in use; if the Docker test services are up, stop them first with \`SQLR_TEST_SERVICES=docker scripts/test-services.sh down\`"
    fi
}

up() {
    require initdb
    require pg_ctl
    require postgres
    require pg_isready
    require mysqld
    require mysql
    require mysqladmin
    require redis-server
    require redis-cli

    pg_start postgres "$PG_PORT" 0
    pg_start postgres-global "$PG_GLOBAL_PORT" 1
    mysql_start mysql "$MYSQL_PORT" ""
    mysql_start mysql-global "$MYSQL_GLOBAL_PORT" ""
    mysql_start mysql-notls "$MYSQL_NOTLS_PORT" "--tls-version="
    redis_start redis "$REDIS_PORT" --port "$REDIS_PORT"
    redis_tls_start
    say "ready (postgres $PG_PORT/$PG_GLOBAL_PORT, mysql $MYSQL_PORT/$MYSQL_GLOBAL_PORT/$MYSQL_NOTLS_PORT, redis $REDIS_PORT/$REDIS_TLS_PORT)"
}

down() {
    redis_stop redis
    redis_stop redis-tls
    mysql_stop mysql "$MYSQL_PORT"
    mysql_stop mysql-global "$MYSQL_GLOBAL_PORT"
    mysql_stop mysql-notls "$MYSQL_NOTLS_PORT"
    pg_stop postgres-global
    pg_stop postgres
}

logs() {
    for file in "$BASE"/*/*.log; do
        [ -f "$file" ] || continue
        say "== $file"
        tail -n 40 "$file"
    done
}

case "${1:-up}" in
    up | up-core)
        up
        ;;
    down)
        down
        ;;
    reset)
        down
        rm -rf "$BASE"
        up
        ;;
    logs)
        logs
        ;;
    *)
        echo "usage: $0 {up|down|reset|logs}" >&2
        exit 2
        ;;
esac
