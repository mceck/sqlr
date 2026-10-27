//! Connection profiles (§7.9): commit mode, read-only and the confirmations a connection
//! starts with, and its tint color.

/// How statements on a session are committed.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, Default,
)]
pub enum CommitMode {
    /// Every statement commits on its own.
    #[default]
    Auto,
    /// The engine opens a transaction before the first statement; the user commits.
    Manual,
    /// Auto until the first statement that writes, then manual until commit or rollback.
    Smart,
}

/// The settings a connection starts with: how it commits, what it confirms and whether it is read-only.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ConnectionProfile {
    /// Stable identifier; the built-in profiles use the `DEVELOPMENT`, `TEST` and `PRODUCTION` ids.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Tints the connection wherever it shows; overrides the connection's own color.
    #[serde(default)]
    pub color: Option<[u8; 3]>,
    /// How statements are committed.
    #[serde(default)]
    pub commit_mode: CommitMode,
    /// Ask before running statements that modify data.
    #[serde(default)]
    pub confirm_writes: bool,
    /// Ask before running destructive statements (DROP, TRUNCATE, DELETE without WHERE…).
    #[serde(default)]
    pub confirm_destructive: bool,
    /// Refuse statements that may modify data.
    #[serde(default)]
    pub read_only: bool,
    /// Minutes an open transaction may stay idle before the user is asked about it.
    #[serde(default)]
    pub idle_tx_timeout_min: Option<u32>,
}

impl ConnectionProfile {
    /// Id of the built-in development profile.
    pub const DEVELOPMENT: &'static str = "development";
    /// Id of the built-in test profile.
    pub const TEST: &'static str = "test";
    /// Id of the built-in production profile.
    pub const PRODUCTION: &'static str = "production";

    /// The built-in profiles, with their fixed ids, in this order: development, test,
    /// production.
    pub fn builtins() -> Vec<ConnectionProfile> {
        vec![
            ConnectionProfile {
                id: Self::DEVELOPMENT.into(),
                name: "Development".into(),
                color: None,
                commit_mode: CommitMode::Auto,
                confirm_writes: false,
                confirm_destructive: true,
                read_only: false,
                idle_tx_timeout_min: None,
            },
            ConnectionProfile {
                id: Self::TEST.into(),
                name: "Test".into(),
                color: Some([0xE0, 0xA0, 0x30]),
                commit_mode: CommitMode::Smart,
                confirm_writes: false,
                confirm_destructive: true,
                read_only: false,
                idle_tx_timeout_min: None,
            },
            ConnectionProfile {
                id: Self::PRODUCTION.into(),
                name: "Production".into(),
                color: Some([0xE0, 0x4F, 0x4F]),
                commit_mode: CommitMode::Manual,
                confirm_writes: true,
                confirm_destructive: true,
                read_only: false,
                idle_tx_timeout_min: Some(10),
            },
        ]
    }

    /// Whether `id` names a built-in profile.
    pub fn is_builtin(id: &str) -> bool {
        [Self::DEVELOPMENT, Self::TEST, Self::PRODUCTION].contains(&id)
    }

    /// The built-in development profile, used when a connection names none.
    pub fn development() -> ConnectionProfile {
        Self::builtins().remove(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_values() {
        let b = ConnectionProfile::builtins();
        let ids: Vec<&str> = b.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["development", "test", "production"]);
        assert_eq!(b[0].commit_mode, CommitMode::Auto);
        assert!(b[0].confirm_destructive && !b[0].confirm_writes);
        assert_eq!(b[1].commit_mode, CommitMode::Smart);
        assert_eq!(b[1].color, Some([0xE0, 0xA0, 0x30]));
        assert_eq!(b[2].commit_mode, CommitMode::Manual);
        assert_eq!(b[2].color, Some([0xE0, 0x4F, 0x4F]));
        assert!(b[2].confirm_writes && b[2].confirm_destructive && !b[2].read_only);
        assert_eq!(b[2].idle_tx_timeout_min, Some(10));
        assert!(ConnectionProfile::is_builtin("test"));
        assert!(!ConnectionProfile::is_builtin("mine"));
    }
}
