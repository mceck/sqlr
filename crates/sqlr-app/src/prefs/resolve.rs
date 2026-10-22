//! The effective value of a setting for a connection: the layered store resolved for one
//! connection and its profile, in the shapes the rest of the app consumes.

use std::collections::HashMap;

use sqlr_core::{ConnectionConfig, ConnectionProfile, DataFormats, Level};
use sqlr_engine::ConnectionSettings;
use sqlr_store::LayeredSettings;

/// A connection's id and its profile's id: what [`LayeredSettings::get`] resolves with.
pub(crate) fn conn_key(cfg: &ConnectionConfig) -> (&str, &str) {
    (
        &cfg.id,
        cfg.profile
            .as_deref()
            .unwrap_or(ConnectionProfile::DEVELOPMENT),
    )
}

pub(crate) fn formats_of(layered: &LayeredSettings, conn: Option<(&str, &str)>) -> DataFormats {
    DataFormats::from_values(|id| Some(layered.get(id, conn).0))
}

/// The display formats of every connection that differs from the global ones.
#[derive(Default)]
pub(crate) struct Formats {
    global: DataFormats,
    per_conn: HashMap<String, DataFormats>,
}

impl Formats {
    pub(crate) fn build(layered: &LayeredSettings, connections: &[ConnectionConfig]) -> Self {
        let global = formats_of(layered, None);
        let per_conn = connections
            .iter()
            .filter_map(|cfg| {
                let own = formats_of(layered, Some(conn_key(cfg)));
                (own != global).then(|| (cfg.id.clone(), own))
            })
            .collect();
        Self { global, per_conn }
    }

    pub(crate) fn global(&self) -> &DataFormats {
        &self.global
    }

    /// The formats of a connection; the global ones for none.
    pub(crate) fn for_conn(&self, conn: Option<&str>) -> &DataFormats {
        conn.and_then(|c| self.per_conn.get(c))
            .unwrap_or(&self.global)
    }
}

/// What `cfg` overrides of the settings the engine follows (nothing where the global value
/// applies).
pub(crate) fn engine_overrides(
    layered: &LayeredSettings,
    cfg: &ConnectionConfig,
) -> ConnectionSettings {
    let key = Some(conn_key(cfg));
    let own = |id: &str| {
        let (value, level) = layered.get(id, key);
        (level != Level::Global).then_some(value)
    };
    ConnectionSettings {
        session_per_tab: own("connection.session_per_tab").map(|v| v.as_bool()),
        connect_timeout_s: own("connection.connect_timeout_s").map(|v| v.as_i64() as u32),
        row_cap: own("grid.row_cap").map(|v| v.as_i64() as usize),
    }
}

/// `profile` with the safety flags a profile override or the connection itself switched on.
/// Flags only tighten: nothing here can turn one off.
pub(crate) fn tightened(
    mut profile: ConnectionProfile,
    layered: &LayeredSettings,
    cfg: &ConnectionConfig,
) -> ConnectionProfile {
    let on = |id: &str| layered.get(id, Some(conn_key(cfg))).0.as_bool();
    profile.read_only |= on("safety.read_only");
    profile.confirm_writes |= on("safety.confirm_writes");
    profile.confirm_destructive |= on("safety.confirm_destructive");
    profile
}

/// The profiles and connections a setting can be overridden for.
#[derive(Clone, Debug, Default)]
pub(crate) struct Scopes {
    /// `(id, name)`.
    pub profiles: Vec<(String, String)>,
    /// `(id, name, profile id)`.
    pub connections: Vec<(String, String, String)>,
}

impl Scopes {
    pub(crate) fn new(profiles: &[ConnectionProfile], connections: &[ConnectionConfig]) -> Self {
        Self {
            profiles: profiles
                .iter()
                .map(|p| (p.id.clone(), p.name.clone()))
                .collect(),
            connections: connections
                .iter()
                .map(|c| (c.id.clone(), c.name.clone(), conn_key(c).1.to_string()))
                .collect(),
        }
    }

    /// Every level a setting can be edited at, global first.
    pub(crate) fn levels(&self) -> Vec<Level> {
        std::iter::once(Level::Global)
            .chain(
                self.profiles
                    .iter()
                    .map(|(id, _)| Level::Profile(id.clone())),
            )
            .chain(
                self.connections
                    .iter()
                    .map(|(id, ..)| Level::Connection(id.clone())),
            )
            .collect()
    }

    pub(crate) fn name(&self, level: &Level) -> String {
        match level {
            Level::Global => "Global".into(),
            Level::Profile(id) => {
                let name = self.profiles.iter().find(|(p, _)| p == id);
                format!("Profile: {}", name.map_or(id.as_str(), |(_, n)| n.as_str()))
            }
            Level::Connection(id) => {
                let name = self.connections.iter().find(|(c, ..)| c == id);
                format!(
                    "Connection: {}",
                    name.map_or(id.as_str(), |(_, n, _)| n.as_str())
                )
            }
        }
    }

    /// What [`LayeredSettings::get`] resolves a value at `level` with.
    pub(crate) fn resolve_key<'a>(&'a self, level: &'a Level) -> Option<(&'a str, &'a str)> {
        match level {
            Level::Global => None,
            // A connection id that has no overrides: only the profile layer applies.
            Level::Profile(p) => Some(("", p.as_str())),
            Level::Connection(c) => {
                let profile = self
                    .connections
                    .iter()
                    .find(|(id, ..)| id == c)
                    .map_or(ConnectionProfile::DEVELOPMENT, |(_, _, p)| p.as_str());
                Some((c.as_str(), profile))
            }
        }
    }
}
