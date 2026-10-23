#!/bin/sh
# Starts/stops the fixed services used by `make test`.
set -eu

cd "$(dirname "$0")/.."

if [ -z "${DOCKER:-}" ]; then
    if [ -x "$HOME/.docker/bin/docker" ]; then
        DOCKER="$HOME/.docker/bin/docker"
    else
        DOCKER=docker
    fi
fi

if [ -z "${DOCKER_HOST:-}" ] && [ -S "$HOME/.docker/run/docker.sock" ]; then
    export DOCKER_HOST="unix://$HOME/.docker/run/docker.sock"
fi

compose() {
    "$DOCKER" compose -f docker-compose.test.yml "$@"
}

# A throwaway CA and server certificate for the TLS services. The key is world-readable:
# it only protects a database that listens on 127.0.0.1 during tests.
certificates() {
    dir=".test-services/certs"
    if [ -f "$dir/ca.crt" ] && [ -f "$dir/server.crt" ] && [ -f "$dir/server.key" ]; then
        return
    fi
    mkdir -p "$dir"
    rm -f "$dir/ca.crt" "$dir/ca.key" "$dir/server.crt" "$dir/server.key" \
        "$dir/server.csr" "$dir/ca.srl" "$dir/server.ext"
    cat >"$dir/ca.cnf" <<'EOF'
[req]
distinguished_name = dn
x509_extensions = v3_ca
prompt = no
[dn]
CN = sqlr test CA
[v3_ca]
basicConstraints = critical,CA:TRUE
keyUsage = critical,keyCertSign,cRLSign
subjectKeyIdentifier = hash
EOF
    cat >"$dir/server.ext" <<'EOF'
subjectAltName = DNS:localhost
basicConstraints = CA:FALSE
keyUsage = digitalSignature,keyEncipherment
extendedKeyUsage = serverAuth
EOF
    openssl req -x509 -newkey rsa:2048 -sha256 -nodes -days 2 \
        -config "$dir/ca.cnf" -keyout "$dir/ca.key" -out "$dir/ca.crt" >/dev/null 2>&1
    openssl req -new -newkey rsa:2048 -sha256 -nodes -subj "/CN=localhost" \
        -keyout "$dir/server.key" -out "$dir/server.csr" >/dev/null 2>&1
    openssl x509 -req -sha256 -days 2 -in "$dir/server.csr" \
        -CA "$dir/ca.crt" -CAkey "$dir/ca.key" -CAcreateserial \
        -extfile "$dir/server.ext" -out "$dir/server.crt" >/dev/null 2>&1
    rm -f "$dir/server.csr" "$dir/server.ext" "$dir/ca.cnf"
    chmod 644 "$dir"/*.key "$dir"/*.crt
}

case "${1:-up}" in
    up)
        certificates
        compose up -d --wait --wait-timeout 600
        ;;
    up-core)
        certificates
        compose up -d --wait --wait-timeout 180 postgres postgres-global mysql mariadb \
            mysql-global mariadb-global mysql-notls mariadb-notls
        ;;
    down)
        compose down
        ;;
    reset)
        certificates
        compose down -v
        compose up -d --wait --wait-timeout 600
        ;;
    logs)
        compose logs -f
        ;;
    *)
        echo "usage: $0 {up|up-core|down|reset|logs}" >&2
        exit 2
        ;;
esac
