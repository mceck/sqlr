

.PHONY: test test-fast test-services test-services-reset check-nextest macos install coverage coverage-fast coverage-open

$(shell mkdir -p dist)

# Requires cargo-nextest (`cargo install cargo-nextest --locked`).
check-nextest:
	@cargo nextest --version >/dev/null 2>&1 || { \
		cargo install cargo-nextest --locked; \
	}

# All Docker-backed tests use fixed services from docker-compose.test.yml.
TEST_SERVICES_ENV = \
	SQLR_TEST_POSTGRES=127.0.0.1:55432 \
	SQLR_TEST_POSTGRES_GLOBAL=127.0.0.1:55433 \
	SQLR_TEST_MYSQL=127.0.0.1:53306 \
	SQLR_TEST_MARIADB=127.0.0.1:53307 \
	SQLR_TEST_MYSQL_GLOBAL=127.0.0.1:53310 \
	SQLR_TEST_MARIADB_GLOBAL=127.0.0.1:53311 \
	SQLR_TEST_MYSQL_NOTLS=127.0.0.1:53312 \
	SQLR_TEST_MARIADB_NOTLS=127.0.0.1:53313 \
	SQLR_TEST_MONGO=127.0.0.1:57017 \
	SQLR_TEST_MONGO_AUTH=127.0.0.1:57018 \
	SQLR_TEST_REDIS=127.0.0.1:56379 \
	SQLR_TEST_REDIS_TLS=127.0.0.1:56380 \
	SQLR_TEST_ELASTIC=127.0.0.1:59200 \
	SQLR_TEST_ELASTIC7=127.0.0.1:59201 \
	SQLR_TEST_ELASTIC_AUTH=127.0.0.1:59202 \
	SQLR_TEST_MSSQL=127.0.0.1:51433 \
	SQLR_TEST_ORACLE=127.0.0.1:51521 \
	SQLR_TEST_CA=$(CURDIR)/.test-services/certs/ca.crt

test: check-nextest
	@trap 'scripts/test-services.sh down' EXIT INT TERM; \
	scripts/test-services.sh up; \
	$(TEST_SERVICES_ENV) cargo nextest run --workspace --profile containers

# Coverage of every crate except SAP HANA; the Docker services start and stop here.
coverage:
	scripts/coverage.sh

# No Docker: UI and unit tests only; the 80% check is informational.
coverage-fast:
	scripts/coverage.sh --fast

coverage-open:
	scripts/coverage.sh --open

test-services:
	scripts/test-services.sh up

# Recreates the shared servers from empty volumes.
test-services-reset:
	scripts/test-services.sh reset

# No Docker needed; everything else runs.
test-fast: check-nextest
	SQLR_SKIP_CONTAINER_TESTS=1 cargo nextest run --workspace

macos:
	scripts/package-macos.sh

install: macos
	cp -R dist/sqlr.app /Applications/