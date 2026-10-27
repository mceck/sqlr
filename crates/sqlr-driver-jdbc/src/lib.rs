//! The JDBC bridge's launcher (spec §6): the Rust side of the sidecar that runs
//! any database with a JDBC driver. It finds a JRE (17 or newer), assembles the class path
//! from the bridge jar and the driver jars, and starts the Java bridge, which then speaks the
//! sidecar protocol on the inherited stdin and stdout.

#![cfg_attr(not(test), deny(clippy::unwrap_used))]

mod java;
mod presets;
mod protocol;

pub use java::{
    JavaError, bridge_jar, classpath, find_java, java_major, launch_command, system_java_homes,
    well_known_java_homes,
};
pub use presets::{PRESETS, Preset, preset};
pub use protocol::answer_with_error;

/// The oldest Java the bridge runs on.
pub const MIN_JAVA: u32 = 17;
