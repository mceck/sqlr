

.PHONY: test test-docker test-fast test-services test-services-docker test-services-reset test-services-reset-docker check-nextest macos install install-fast bump coverage coverage-fast coverage-open

$(shell mkdir -p dist)

# Requires cargo-nextest (`cargo install cargo-nextest --locked`).
check-nextest:
	@cargo nextest --version >/dev/null 2>&1 || { \
		cargo install cargo-nextest --locked; \
	}

# Every service in docker-compose.test.yml.
SERVICES_ENV = \
	SQLR_TEST_POSTGRES=127.0.0.1:55432 \
	SQLR_TEST_POSTGRES_GLOBAL=127.0.0.1:55433 \
	SQLR_TEST_MYSQL=127.0.0.1:53306 \
	SQLR_TEST_MYSQL_GLOBAL=127.0.0.1:53310 \
	SQLR_TEST_MYSQL_NOTLS=127.0.0.1:53312 \
	SQLR_TEST_MARIADB=127.0.0.1:53307 \
	SQLR_TEST_MARIADB_GLOBAL=127.0.0.1:53311 \
	SQLR_TEST_MARIADB_NOTLS=127.0.0.1:53313 \
	SQLR_TEST_MONGO=127.0.0.1:57017 \
	SQLR_TEST_MONGO_AUTH=127.0.0.1:57018 \
	SQLR_TEST_ELASTIC=127.0.0.1:59200 \
	SQLR_TEST_ELASTIC7=127.0.0.1:59201 \
	SQLR_TEST_ELASTIC_AUTH=127.0.0.1:59202 \
	SQLR_TEST_MSSQL=127.0.0.1:51433 \
	SQLR_TEST_ORACLE=127.0.0.1:51521 \
	SQLR_TEST_REDIS=127.0.0.1:56379 \
	SQLR_TEST_REDIS_TLS=127.0.0.1:56380 \
	SQLR_TEST_CA=$(CURDIR)/.test-services/certs/ca.crt

# Set to 1 while iterating to reuse the running services between full test runs.
KEEP_TEST_SERVICES ?= 0

# The whole matrix in containers; by default the services stop at the end.
test: check-nextest
	@if [ "$(KEEP_TEST_SERVICES)" != 1 ]; then \
		trap 'scripts/test-services.sh down' EXIT INT TERM; \
	fi; \
	scripts/test-services.sh up && \
	$(SERVICES_ENV) cargo nextest run --workspace --profile containers

test-docker: test

# No Docker needed; everything else runs.
test-fast: check-nextest
	SQLR_SKIP_CONTAINER_TESTS=1 cargo nextest run --workspace

test-services:
	scripts/test-services.sh up

test-services-docker: test-services

# Recreates the Docker volumes from empty.
test-services-reset:
	scripts/test-services.sh reset

test-services-reset-docker: test-services-reset

# Coverage of every crate except SAP HANA; the Docker services start and stop here.
coverage:
	scripts/coverage.sh

# No Docker: UI and unit tests only; the 80% check is informational.
coverage-fast:
	scripts/coverage.sh --fast

coverage-open:
	scripts/coverage.sh --open

macos:
	scripts/package-macos.sh

# Builds and installs for this platform: /Applications on macOS, ~/.local on Linux, cargo install elsewhere.
install:
	scripts/install.sh

# Aligns every crate version: make bump V=0.2.0 (or V=patch|minor|major; default patch).
V ?= patch
bump:
	scripts/bump-version.sh $(V)

# Keep release behavior while favoring compilation speed over runtime performance.
install-fast: export CARGO_PROFILE_RELEASE_OPT_LEVEL = 0
install-fast: export CARGO_PROFILE_RELEASE_LTO = false
install-fast: export CARGO_PROFILE_RELEASE_CODEGEN_UNITS = 256
install-fast: install

clean:
	rm -rf dist && rm -rf target
