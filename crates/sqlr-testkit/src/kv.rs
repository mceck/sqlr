//! A fake key-value driver: keys held in memory, with scripted pages.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::Mutex;
use sqlr_core::{
    CancelHandle, Capabilities, Connection, ConnectionConfig, Driver, DriverError, DriverManifest,
    ErrorKind, Family, KeyInfo, KeyType, KeyValueConn, KvCursor, KvEdit, KvPage, Node, NodePath,
    RespValue, STRING_PAGE_BYTES, Secrets, ServerInfo, StatementClass, read_only_error,
};

#[derive(Clone, Default)]
pub struct FakeKvScript {
    /// The keys of database 0, in scan order, with their whole value (the first page).
    pub keys: Vec<(Vec<u8>, KvPage)>,
    /// `databases()`; 0 means 16.
    pub databases: u32,
    /// `browse(path)` returns the nodes whose path is a direct child of `path`, in order.
    pub tree: Vec<Node>,
}

type Store = Vec<(u32, Vec<u8>, KvPage)>;

struct Shared {
    tree: Vec<Node>,
    keys: Mutex<Store>,
    scans: Mutex<Vec<(u32, String, u64)>>,
    edits: Mutex<Vec<(u32, Vec<u8>, KvEdit)>>,
    commands: Mutex<Vec<(u32, Vec<Vec<u8>>)>>,
    databases: u32,
    read_only: AtomicBool,
    /// Console commands whose name is this block until `release_blocked`.
    block: Mutex<Option<String>>,
    released: tokio::sync::Notify,
}

pub struct FakeKvDriver {
    manifest: DriverManifest,
    shared: Arc<Shared>,
}

impl FakeKvDriver {
    pub fn new(script: FakeKvScript) -> Arc<Self> {
        let keys = script
            .keys
            .into_iter()
            .map(|(name, page)| (0, name, page))
            .collect();
        Arc::new(Self {
            manifest: DriverManifest {
                id: "fakekv".into(),
                name: "Fake key-value".into(),
                family: Family::KeyValue,
                capabilities: Capabilities::default(),
                fields: Vec::new(),
                dialect: "redis".into(),
                url_schemes: Vec::new(),
                default_port: None,
                editing: None,
            },
            shared: Arc::new(Shared {
                tree: script.tree,
                keys: Mutex::new(keys),
                scans: Mutex::new(Vec::new()),
                edits: Mutex::new(Vec::new()),
                commands: Mutex::new(Vec::new()),
                databases: if script.databases == 0 {
                    16
                } else {
                    script.databases
                },
                read_only: AtomicBool::new(false),
                block: Mutex::new(None),
                released: tokio::sync::Notify::new(),
            }),
        })
    }

    /// Every `scan` call as `(database, pattern, cursor)`, in order.
    pub fn scans(&self) -> Vec<(u32, String, u64)> {
        self.shared.scans.lock().clone()
    }

    /// Every `edit` call as `(database, key, edit)`, in order.
    pub fn edits(&self) -> Vec<(u32, Vec<u8>, KvEdit)> {
        self.shared.edits.lock().clone()
    }

    /// Every console `command` call as `(database, arguments)`, in order.
    pub fn commands(&self) -> Vec<(u32, Vec<Vec<u8>>)> {
        self.shared.commands.lock().clone()
    }

    /// The whole value of a key as it is now.
    pub fn key(&self, db: u32, name: &[u8]) -> Option<KvPage> {
        self.shared
            .keys
            .lock()
            .iter()
            .find(|(d, n, _)| *d == db && n == name)
            .map(|(_, _, page)| page.clone())
    }

    /// Console commands named `name` (any case) wait until [`Self::release_blocked`].
    pub fn block_commands_matching(&self, name: &str) {
        *self.shared.block.lock() = Some(name.to_uppercase());
    }

    /// Lets the blocked commands finish.
    pub fn release_blocked(&self) {
        *self.shared.block.lock() = None;
        self.shared.released.notify_waiters();
    }

    /// Whether the connections are in read-only mode.
    pub fn read_only(&self) -> bool {
        self.shared.read_only.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl Driver for FakeKvDriver {
    fn manifest(&self) -> &DriverManifest {
        &self.manifest
    }

    async fn connect(
        &self,
        _cfg: &ConnectionConfig,
        _secrets: &Secrets,
    ) -> Result<Arc<dyn Connection>, DriverError> {
        Ok(Arc::new(FakeKvConnection {
            shared: self.shared.clone(),
        }))
    }
}

struct FakeKvConnection {
    shared: Arc<Shared>,
}

struct NoCancel;

#[async_trait::async_trait]
impl CancelHandle for NoCancel {
    async fn cancel(&self) -> Result<(), DriverError> {
        Ok(())
    }
}

#[async_trait::async_trait]
impl Connection for FakeKvConnection {
    async fn ping(&self) -> Result<ServerInfo, DriverError> {
        Ok(ServerInfo {
            product: "FakeKv".into(),
            version: "1".into(),
        })
    }

    async fn browse(&self, path: &NodePath) -> Result<Vec<Node>, DriverError> {
        let depth = path.segments().len();
        Ok(self
            .shared
            .tree
            .iter()
            .filter(|n| {
                n.path.segments().len() == depth + 1
                    && n.path.segments()[..depth] == *path.segments()
            })
            .cloned()
            .collect())
    }

    fn cancel_handle(&self) -> Arc<dyn CancelHandle> {
        Arc::new(NoCancel)
    }

    fn key_value(&self) -> Option<&dyn KeyValueConn> {
        Some(self)
    }
}

fn type_of(page: &KvPage) -> KeyType {
    match page {
        KvPage::String { .. } => KeyType::String,
        KvPage::Hash { .. } => KeyType::Hash,
        KvPage::List { .. } => KeyType::List,
        KvPage::Set { .. } => KeyType::Set,
        KvPage::ZSet { .. } => KeyType::ZSet,
        KvPage::Stream { .. } => KeyType::Stream,
    }
}

/// `*` and `?` globs, nothing else.
fn glob(pattern: &[u8], text: &[u8]) -> bool {
    match pattern.split_first() {
        None => text.is_empty(),
        Some((b'*', rest)) => (0..=text.len()).any(|i| glob(rest, &text[i..])),
        Some((b'?', rest)) => !text.is_empty() && glob(rest, &text[1..]),
        Some((c, rest)) => text.first() == Some(c) && glob(rest, &text[1..]),
    }
}

fn already_exists() -> DriverError {
    DriverError::new(ErrorKind::Other, "The new key name already exists")
}

fn missing() -> DriverError {
    DriverError::new(ErrorKind::Other, "The key does not exist")
}

fn wrong_type() -> DriverError {
    DriverError::new(ErrorKind::Other, "WRONGTYPE Operation against a key")
}

impl FakeKvConnection {
    fn check_write(&self) -> Result<(), DriverError> {
        if self.shared.read_only.load(Ordering::SeqCst) {
            Err(read_only_error())
        } else {
            Ok(())
        }
    }

    fn run(&self, db: u32, args: &[Vec<u8>]) -> RespValue {
        let name = String::from_utf8_lossy(args.first().map_or(&[][..], |a| a)).to_uppercase();
        match name.as_str() {
            "PING" => RespValue::Simple("PONG".into()),
            "SET" if args.len() >= 3 => {
                let value = args[2].clone();
                let mut keys = self.shared.keys.lock();
                let page = KvPage::String {
                    total_len: value.len() as u64,
                    bytes: value,
                };
                match keys.iter_mut().find(|(d, n, _)| *d == db && *n == args[1]) {
                    Some(entry) => entry.2 = page,
                    None => keys.push((db, args[1].clone(), page)),
                }
                RespValue::Simple("OK".into())
            }
            "GET" if args.len() == 2 => match self
                .shared
                .keys
                .lock()
                .iter()
                .find(|(d, n, _)| *d == db && *n == args[1])
            {
                Some((_, _, KvPage::String { bytes, .. })) => RespValue::Bulk(bytes.clone()),
                Some(_) => RespValue::Error("WRONGTYPE Operation against a key".into()),
                None => RespValue::Nil,
            },
            _ => RespValue::Simple("OK".into()),
        }
    }
}

#[async_trait::async_trait]
impl KeyValueConn for FakeKvConnection {
    /// The cursor counts the matches already returned (a real server's is opaque).
    async fn scan(
        &self,
        db: u32,
        pattern: &[u8],
        cursor: u64,
        count: usize,
    ) -> Result<(Vec<KeyInfo>, u64), DriverError> {
        self.shared
            .scans
            .lock()
            .push((db, String::from_utf8_lossy(pattern).into_owned(), cursor));
        let keys = self.shared.keys.lock();
        let matching: Vec<&(u32, Vec<u8>, KvPage)> = keys
            .iter()
            .filter(|(d, n, _)| *d == db && glob(pattern, n))
            .collect();
        let from = (cursor as usize).min(matching.len());
        let to = (from + count.max(1)).min(matching.len());
        let found = matching[from..to]
            .iter()
            .map(|(_, name, page)| KeyInfo::new(name.clone(), type_of(page)))
            .collect();
        Ok((found, if to < matching.len() { to as u64 } else { 0 }))
    }

    async fn key_info(&self, db: u32, key: &[u8]) -> Result<Option<KeyInfo>, DriverError> {
        Ok(self.shared.keys.lock().iter().find_map(|(d, n, page)| {
            (*d == db && n == key).then(|| KeyInfo {
                size: Some(page.len() as u64),
                length: Some(page.len() as u64),
                ..KeyInfo::new(n.clone(), type_of(page))
            })
        }))
    }

    async fn page(
        &self,
        db: u32,
        key: &[u8],
        from: KvCursor,
        count: usize,
    ) -> Result<KvPage, DriverError> {
        let page = self
            .shared
            .keys
            .lock()
            .iter()
            .find(|(d, n, _)| *d == db && n == key)
            .map(|(_, _, page)| page.clone())
            .ok_or_else(missing)?;
        let start = match from {
            KvCursor::Start | KvCursor::After(_) => 0,
            KvCursor::Scan(i) => i as usize,
            KvCursor::Index(i) => i.max(0) as usize,
        };
        let count = count.max(1);
        Ok(match page {
            KvPage::String { bytes, total_len } => {
                let end = (start + STRING_PAGE_BYTES).min(bytes.len());
                KvPage::String {
                    bytes: bytes[start.min(end)..end].to_vec(),
                    total_len,
                }
            }
            KvPage::Hash { fields, .. } => {
                let end = (start + count).min(fields.len());
                KvPage::Hash {
                    cursor: if end < fields.len() { end as u64 } else { 0 },
                    fields: fields[start.min(end)..end].to_vec(),
                }
            }
            KvPage::Set { members, .. } => {
                let end = (start + count).min(members.len());
                KvPage::Set {
                    cursor: if end < members.len() { end as u64 } else { 0 },
                    members: members[start.min(end)..end].to_vec(),
                }
            }
            KvPage::List { items, .. } => {
                let end = (start + count).min(items.len());
                KvPage::List {
                    total: items.len() as u64,
                    start: start.min(end) as i64,
                    items: items[start.min(end)..end].to_vec(),
                }
            }
            KvPage::ZSet { members, .. } => {
                let end = (start + count).min(members.len());
                KvPage::ZSet {
                    total: members.len() as u64,
                    start: start.min(end) as i64,
                    members: members[start.min(end)..end].to_vec(),
                }
            }
            KvPage::Stream { entries, .. } => {
                let end = (start + count).min(entries.len());
                let page = entries[start.min(end)..end].to_vec();
                KvPage::Stream {
                    next: (end < entries.len())
                        .then(|| page.last().map(|(id, _)| id.clone()))
                        .flatten(),
                    total: entries.len() as u64,
                    entries: page,
                }
            }
        })
    }

    async fn edit(&self, db: u32, key: &[u8], edit: KvEdit) -> Result<(), DriverError> {
        self.check_write()?;
        self.shared
            .edits
            .lock()
            .push((db, key.to_vec(), edit.clone()));
        let mut keys = self.shared.keys.lock();
        let at = keys.iter().position(|(d, n, _)| *d == db && n == key);
        let edit = match edit {
            KvEdit::Create(_) if at.is_some() => {
                return Err(DriverError::new(ErrorKind::Other, "The key already exists"));
            }
            KvEdit::Create(first) => *first,
            other => other,
        };
        match edit {
            KvEdit::Delete => {
                if let Some(at) = at {
                    keys.remove(at);
                }
            }
            KvEdit::Rename(new) => {
                let at = at.ok_or_else(missing)?;
                if keys.iter().any(|(d, n, _)| *d == db && *n == new) {
                    return Err(already_exists());
                }
                keys[at].1 = new;
            }
            KvEdit::Expire(_) => {
                at.ok_or_else(missing)?;
            }
            KvEdit::SetString(value) => {
                let page = KvPage::String {
                    total_len: value.len() as u64,
                    bytes: value,
                };
                match at {
                    Some(at) => keys[at].2 = page,
                    None => keys.push((db, key.to_vec(), page)),
                }
            }
            KvEdit::HashSet(field, value) => {
                let at = at.unwrap_or_else(|| {
                    keys.push((
                        db,
                        key.to_vec(),
                        KvPage::Hash {
                            fields: Vec::new(),
                            cursor: 0,
                        },
                    ));
                    keys.len() - 1
                });
                let KvPage::Hash { fields, .. } = &mut keys[at].2 else {
                    return Err(wrong_type());
                };
                match fields.iter_mut().find(|(f, _)| *f == field) {
                    Some(entry) => entry.1 = value,
                    None => fields.push((field, value)),
                }
            }
            KvEdit::HashDel(field) => {
                if let Some(at) = at {
                    let KvPage::Hash { fields, .. } = &mut keys[at].2 else {
                        return Err(wrong_type());
                    };
                    fields.retain(|(f, _)| *f != field);
                }
            }
            // The remaining edits are recorded; the tests that use them read `edits()`.
            _ => {}
        }
        Ok(())
    }

    async fn command(&self, db: u32, args: Vec<Vec<u8>>) -> Result<RespValue, DriverError> {
        if self.classify(&args).modifies() {
            self.check_write()?;
        }
        self.shared.commands.lock().push((db, args.clone()));
        let name = String::from_utf8_lossy(args.first().map_or(&[][..], |a| a)).to_uppercase();
        if self.shared.block.lock().as_deref() == Some(name.as_str()) {
            self.shared.released.notified().await;
        }
        Ok(self.run(db, &args))
    }

    async fn exec_block(
        &self,
        db: u32,
        commands: Vec<Vec<Vec<u8>>>,
    ) -> Result<RespValue, DriverError> {
        if commands.iter().any(|c| self.classify(c).modifies()) {
            self.check_write()?;
        }
        Ok(RespValue::Array(
            commands.iter().map(|c| self.run(db, c)).collect(),
        ))
    }

    fn classify(&self, args: &[Vec<u8>]) -> StatementClass {
        let name = String::from_utf8_lossy(args.first().map_or(&[][..], |a| a)).to_uppercase();
        match name.as_str() {
            "GET" | "MGET" | "EXISTS" | "PING" | "TTL" | "TYPE" => StatementClass::Read,
            "FLUSHDB" | "FLUSHALL" => StatementClass::Destructive(name),
            _ => StatementClass::Write,
        }
    }

    fn databases(&self) -> u32 {
        self.shared.databases
    }

    fn set_read_only(&self, on: bool) {
        self.shared.read_only.store(on, Ordering::SeqCst);
    }
}
