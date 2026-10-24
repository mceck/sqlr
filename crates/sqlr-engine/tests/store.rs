//! Keychain secrets and connection profiles are read and written by the engine, off the UI
//! thread.

use std::sync::Arc;

use parking_lot::Mutex;
use sqlr_core::{CommitMode, ConnectionProfile, Secrets};
use sqlr_engine::{DriverRegistry, Engine, SessionStatus};
use sqlr_store::{MemorySecretStore, SecretStore, StoreError};
use sqlr_testkit::{FakeDriver, FakeScript};

use crate::support;
use support::{TIMEOUT, config, dev, no_repaint, wait_until};

/// A secret store that records the thread of every read.
#[derive(Default)]
struct Recording {
    inner: MemorySecretStore,
    reads: Mutex<Vec<String>>,
}

impl SecretStore for Recording {
    fn get(&self, conn_id: &str, field: &str) -> Result<Option<String>, StoreError> {
        let thread = std::thread::current().name().unwrap_or("?").to_string();
        self.reads.lock().push(thread);
        self.inner.get(conn_id, field)
    }
    fn set(&self, conn_id: &str, field: &str, value: &str) -> Result<(), StoreError> {
        self.inner.set(conn_id, field, value)
    }
    fn delete(&self, conn_id: &str, field: &str) -> Result<(), StoreError> {
        self.inner.delete(conn_id, field)
    }
}

fn engine() -> (Engine, Arc<FakeDriver>, Arc<Recording>) {
    let fake = FakeDriver::new(FakeScript::default());
    let mut drivers = DriverRegistry::default();
    drivers.register(fake.clone());
    let engine = Engine::new(drivers, no_repaint());
    let store = Arc::new(Recording::default());
    store.set("c", "password", "from-keychain").unwrap();
    engine.set_secret_store(store.clone());
    (engine, fake, store)
}

fn connected(engine: &Engine) {
    assert!(wait_until(TIMEOUT, || matches!(
        engine.session_status("c"),
        Some(SessionStatus::Connected(_))
    )));
}

fn password(s: &Secrets) -> Option<&str> {
    s.0.get("password").map(String::as_str)
}

#[test]
fn secrets_come_from_the_keychain_off_the_ui_thread() {
    let (engine, fake, store) = engine();
    engine.connect(config("c", "fake"), Secrets::default(), dev());
    connected(&engine);
    assert_eq!(password(&fake.connects()[0].1), Some("from-keychain"));
    let reads = store.reads.lock().clone();
    assert_eq!(reads.len(), 1, "{reads:?}");
    let here = std::thread::current().name().unwrap_or("?").to_string();
    assert_ne!(
        reads[0], here,
        "the keychain was read on the caller's thread"
    );

    // A tab session reuses what was read: the keychain is not asked again.
    let tab = engine.session("c", Some(1));
    let q = engine.execute_in(&tab, None, "select 1".into(), 10);
    assert!(wait_until(TIMEOUT, || fake.connects().len() == 2));
    assert!(wait_until(TIMEOUT, || engine
        .result(q)
        .is_some_and(
            |r| r.read().status != sqlr_engine::QueryStatus::Running
        )));
    assert_eq!(password(&fake.connects()[1].1), Some("from-keychain"));
    assert_eq!(store.reads.lock().len(), 1);
}

#[test]
fn entered_secrets_win_and_unsaved_ones_skip_the_keychain() {
    let (engine, fake, store) = engine();
    let typed = Secrets([("password".to_string(), "typed".to_string())].into());
    engine.connect(config("c", "fake"), typed, dev());
    connected(&engine);
    assert_eq!(password(&fake.connects()[0].1), Some("typed"));

    let mut unsaved = config("c", "fake");
    unsaved.save_secrets = false;
    let before = store.reads.lock().len();
    engine.connect(unsaved, Secrets::default(), dev());
    assert!(wait_until(TIMEOUT, || fake.connects().len() == 2));
    assert_eq!(password(&fake.connects()[1].1), None);
    assert_eq!(store.reads.lock().len(), before);
}

#[test]
fn profiles_load_and_save_in_the_background() {
    let dir = tempfile::tempdir().unwrap();
    let (engine, _, _) = engine();
    assert!(engine.profiles().is_none(), "nothing loaded yet");
    engine.load_profiles(dir.path().to_path_buf());
    assert!(wait_until(TIMEOUT, || engine.profiles().is_some()));
    assert_eq!(engine.profiles().unwrap().len(), 3);

    let rev = engine.profiles_revision();
    let mine = ConnectionProfile {
        id: "u1".into(),
        name: "Staging".into(),
        commit_mode: CommitMode::Smart,
        ..dev()
    };
    engine.save_profile(dir.path().to_path_buf(), mine.clone());
    assert!(wait_until(TIMEOUT, || engine.profiles_revision() > rev));
    assert_eq!(engine.profiles().unwrap()[3], mine);
    assert!(dir.path().join("profiles.toml").exists());

    engine.delete_profile(dir.path().to_path_buf(), "u1".into());
    assert!(wait_until(TIMEOUT, || engine.profiles().unwrap().len() == 3));
}
