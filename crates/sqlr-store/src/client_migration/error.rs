//! The failures of importing connections from, and exporting them to, other clients.
//!
//! No variant carries the text of a file, a URL or a lower-level error: those may hold
//! credentials, so every message is fixed.

/// Why a connection file from another client could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ImportError {
    #[error(
        "Unrecognized connection file. Encrypted TablePlus archives are not supported; choose Connections.plist or connection URLs instead."
    )]
    EncryptedTablePlus,
    #[error("This XML file does not contain JetBrains data sources or a TablePlus property list.")]
    UnknownXml,
    #[error("Invalid connection JSON file.")]
    InvalidJson,
    #[error(
        "Encrypted connection backups are not supported. Export plaintext JSON from Tabularis first."
    )]
    EncryptedBackup,
    #[error(
        "Unrecognized connection file. Choose DBeaver data-sources.json, DataGrip XML, TablePlus Connections.plist / URLs, pgAdmin servers.json, or a sqlr backup."
    )]
    Unrecognized,
    #[error("Unsupported sqlr backup version.")]
    UnsupportedSqlrVersion,
    #[error("Invalid sqlr connection definitions.")]
    InvalidSqlrConnections,
    #[error("Invalid backup credentials.")]
    InvalidBackupCredentials,
    #[error("Unsupported Oracle URL.")]
    UnsupportedOracleUrl,
    #[error("Unsupported connection URL.")]
    UnsupportedUrl,
    #[error("MongoDB SRV connections require manual setup.")]
    MongoSrv,
    #[error("Unsupported or credential-bearing URL; use an export without passwords.")]
    CredentialBearingUrl,
    #[error("No DBeaver connections map found.")]
    NoDbeaverConnections,
    #[error("Invalid XML encoding.")]
    InvalidXmlEncoding,
    #[error("Invalid DataGrip XML file.")]
    InvalidDatagripXml,
    #[error("Invalid local DataGrip XML encoding.")]
    InvalidLocalDatagripEncoding,
    #[error("Invalid local DataGrip XML file.")]
    InvalidLocalDatagripXml,
    #[error(
        "Invalid TablePlus property list. Encrypted .tableplusconnection archives are not supported."
    )]
    InvalidTablePlusPlist,
    #[error("Invalid TablePlus JSON file.")]
    InvalidTablePlusJson,
    #[error(
        "No connection URLs found. Choose Connections.plist or a text file containing connection URLs."
    )]
    NoConnectionUrls,
    #[error("Expected a TablePlus connection array.")]
    ExpectedTablePlusArray,
    #[error("No pgAdmin Servers map found.")]
    NoPgAdminServers,
    #[error("Connection groups contain a cycle.")]
    GroupCycle,
    #[error("Unsupported or encrypted Tabularis backup. Export plaintext JSON first.")]
    UnsupportedTabularis,
    #[error("Invalid Tabularis connections.")]
    InvalidTabularis,
    #[error("No supported connections found in the file.")]
    NoSupportedConnections,
    #[error("Could not open the Beekeeper database read-only.")]
    BeekeeperOpen,
    #[error("Could not read the Beekeeper database.")]
    BeekeeperRead,
    #[error("This file is not a supported Beekeeper Studio database.")]
    BeekeeperUnsupported,
    #[error("Could not read Beekeeper connections.")]
    BeekeeperConnections,
    #[error("Could not read Beekeeper connection settings.")]
    BeekeeperSettings,
    #[error("Too many Beekeeper connections to import.")]
    BeekeeperTooMany,
    #[error("Could not open the connection file. Check its location and permissions.")]
    CannotOpenFile,
    #[error("Could not read the connection file.")]
    CannotReadFile,
    #[error("Connection file exceeds the 16 MiB limit.")]
    FileTooLarge,
}

/// Why connections could not be written in another client's format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ExportError {
    #[error("Invalid connection URL.")]
    InvalidUrl,
    #[error("Invalid JDBC URL.")]
    InvalidJdbcUrl,
    #[error("SQL Server database name needs manual JDBC URL configuration.")]
    SqlServerJdbc,
    #[error("This driver cannot be represented by this client's portable connection format.")]
    UnsupportedDriver,
    #[error("Cannot build connection URL.")]
    BuildUrl,
    #[error("Cannot build SQLite URL.")]
    BuildSqliteUrl,
    #[error("Invalid connection host.")]
    InvalidHost,
    #[error("Invalid connection port.")]
    InvalidPort,
    #[error("Invalid connection user.")]
    InvalidUser,
    #[error("This export format does not support credentials.")]
    NoCredentials,
    #[error("Select at least one connection.")]
    NothingSelected,
    #[error(
        "None of the selected connections can be exported to this format. Choose sqlr to back up all drivers and portable settings."
    )]
    NothingExportable,
    #[error("None of the selected connections can be exported to this format.")]
    NothingExportableHere,
    #[error("No selected connection has a supported DBeaver driver.")]
    NoDbeaverDriver,
    #[error("Could not serialize connections.")]
    SerializeConnections,
    #[error("Could not serialize connection.")]
    SerializeConnection,
    #[error("Cannot export a password for this connection URL.")]
    PasswordInUrl,
    #[error("Choose a new folder for the Beekeeper export.")]
    ChooseNewFolder,
    #[error("Could not create the export folder.")]
    CreateFolder,
    #[error("Could not save the connection export.")]
    SaveConnectionExport,
    #[error("Could not save the export folder. Choose a new destination.")]
    SaveFolder,
    #[error("Could not save the export. Check the destination and permissions.")]
    SaveFile,
}
