

.PHONY: test test-fast test-services test-services-reset check-nextest macos install

$(shell mkdir -p dist)

# Requires cargo-nextest (`cargo install cargo-nextest --locked`).
check-nextest:
	@cargo nextest --version >/dev/null 2>&1 || { \
		cargo install cargo-nextest --locked; \
	}

# Postgres, MySQL and MariaDB are shared services; tests get a fresh database each.
TEST_SERVICES_ENV = \
	SQLR_TEST_POSTGRES=127.0.0.1:55432 \
	SQLR_TEST_MYSQL=127.0.0.1:53306 \
	SQLR_TEST_MARIADB=127.0.0.1:53307

test: check-nextest
	scripts/test-services.sh up
	$(TEST_SERVICES_ENV) cargo nextest run --workspace --profile containers

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