# SAP HANA

A bundled sidecar crate built on `hdbconnect_async` (spec §5.6), installed from a local
driver file through the Driver Manager (Settings → Drivers…):

```sh
scripts/package-driver.sh hana hana.tar.gz      # builds it in release mode and packs it
```

then **Install from file…** in the Driver Manager.

## Connection form

| Field | Meaning |
|---|---|
| Host, Port | `3<instance>15` for a tenant database, `3<instance>13` for SYSTEMDB (HANA Cloud: 443) |
| Tenant database | asks the server to redirect to this tenant |
| User, Password | the password lives in the OS keychain |
| Default schema | `SET SCHEMA` after connecting |
| Use TLS | on by default; certificates are verified |
| CA certificate (PEM) | a custom CA; converted to the DER files `hdbconnect` needs |
| Skip certificate verification | for default HXE certificates, which have no SAN |
| Pin the redirect to this host, Tenant SQL port | see below |

## Driver behavior

- **Custom CAs** only work for `hdbconnect` as DER files; the PEM you pick is converted into a
  temporary directory.
- **No rustls provider** is enabled by the crate; the driver installs `ring`.
- **No cancel API**: cancel opens a second connection and runs
  `ALTER SYSTEM CANCEL SESSION '<connection id>'`.
- **Redirects**: with a tenant name, HANA redirects to the address it advertises, which is
  unreachable behind Docker, NAT or an SSH tunnel, and `hdbconnect` has no hook to change it.
  *Pin the redirect* skips the redirect: sqlr connects to the host you typed on the tenant's
  own SQL port (*Tenant SQL port*). Find it in SYSTEMDB:
  `SELECT DATABASE_NAME, SQL_PORT FROM SYS_DATABASES.M_SERVICES WHERE SERVICE_NAME = 'indexserver'`.

## Limits of this release

- Statements run in auto-commit mode (the driver declares no transactions).
- Large objects (LOB columns) are read whole.
- Spatial types arrive as bytes with the geometry hint.
- The live tests need a HANA and are ignored by default
  (`SQLR_HANA_URL`, `SQLR_HANA_USER`, `SQLR_HANA_PASSWORD`); nothing here has run against a
  native HANA or HANA Cloud yet.
