#!/bin/sh
# Starts/stops the shared services used by `make test`.
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

case "${1:-up}" in
    up)
        compose up -d --wait postgres mysql mariadb
        ;;
    up-all)
        compose up -d --wait
        ;;
    down)
        compose down
        ;;
    reset)
        compose down -v
        compose up -d --wait postgres mysql mariadb
        ;;
    logs)
        compose logs -f
        ;;
    *)
        echo "usage: $0 {up|up-all|down|reset|logs}" >&2
        exit 2
        ;;
esac
