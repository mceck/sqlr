//! The key-value family of the driver API (Redis): spec §5.2, §5.6.
//!
//! Key names and values are bytes (Redis is binary-safe). Browsing uses `SCAN` with a
//! `COUNT` hint and never `KEYS`; values are read one page at a time.

use std::collections::HashSet;
use std::fmt::Write as _;

use crate::classify::StatementClass;
use crate::error::DriverError;
use crate::node::NodePath;

/// Keys a `SCAN` call asks for (its `COUNT` hint).
pub const SCAN_COUNT: usize = 500;
/// Elements of a hash, list, set, sorted set or stream page.
pub const VALUE_PAGE: usize = 200;
/// Bytes of a string page: a string preview is capped at this, with "load more".
pub const STRING_PAGE_BYTES: usize = 1 << 20;

/// The type of a key (`TYPE`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum KeyType {
    String,
    Hash,
    List,
    Set,
    ZSet,
    Stream,
    /// RedisJSON (`ReJSON-RL`); shown raw, read-only.
    Json,
    /// Any other module type, by the name `TYPE` returned.
    Other(String),
}

impl KeyType {
    /// The type for a `TYPE` reply.
    pub fn from_type_name(name: &str) -> Self {
        match name {
            "string" => KeyType::String,
            "hash" => KeyType::Hash,
            "list" => KeyType::List,
            "set" => KeyType::Set,
            "zset" => KeyType::ZSet,
            "stream" => KeyType::Stream,
            "ReJSON-RL" => KeyType::Json,
            other => KeyType::Other(other.to_string()),
        }
    }

    /// The short lowercase name shown in type badges.
    pub fn label(&self) -> &str {
        match self {
            KeyType::String => "string",
            KeyType::Hash => "hash",
            KeyType::List => "list",
            KeyType::Set => "set",
            KeyType::ZSet => "zset",
            KeyType::Stream => "stream",
            KeyType::Json => "json",
            KeyType::Other(name) => name,
        }
    }
}

/// A key as the browser and the value view show it.
#[derive(Clone, Debug, PartialEq)]
pub struct KeyInfo {
    pub name: Vec<u8>,
    pub ty: KeyType,
    /// Milliseconds to live; `None` when the key does not expire.
    pub ttl_ms: Option<i64>,
    /// Bytes the key takes in memory (`MEMORY USAGE`), when asked for.
    pub size: Option<u64>,
    /// Elements (hash fields, list items, members, stream entries) or string bytes, when
    /// asked for.
    pub length: Option<u64>,
}

impl KeyInfo {
    /// A key known only by name and type (no TTL, size or length).
    pub fn new(name: impl Into<Vec<u8>>, ty: KeyType) -> Self {
        Self {
            name: name.into(),
            ty,
            ttl_ms: None,
            size: None,
            length: None,
        }
    }
}

/// Where a value page starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KvCursor {
    Start,
    /// An `HSCAN`/`SSCAN` cursor.
    Scan(u64),
    /// A list or sorted-set index, or a string byte offset.
    Index(i64),
    /// The stream entry id the next page starts after.
    After(String),
}

/// A stream entry: its id and its field/value pairs.
pub type StreamEntry = (String, Vec<(Vec<u8>, Vec<u8>)>);

/// One page of a value; the engine appends later pages to the first.
#[derive(Clone, Debug, PartialEq)]
pub enum KvPage {
    String {
        bytes: Vec<u8>,
        total_len: u64,
    },
    Hash {
        fields: Vec<(Vec<u8>, Vec<u8>)>,
        /// The `HSCAN` cursor of the next page; 0 once complete.
        cursor: u64,
    },
    List {
        items: Vec<Vec<u8>>,
        start: i64,
        total: u64,
    },
    Set {
        members: Vec<Vec<u8>>,
        cursor: u64,
    },
    ZSet {
        members: Vec<(Vec<u8>, f64)>,
        start: i64,
        total: u64,
    },
    Stream {
        entries: Vec<StreamEntry>,
        /// The id the next page starts after; `None` once complete.
        next: Option<String>,
        total: u64,
    },
}

impl KvPage {
    /// Where the next page starts; `None` once the value is complete.
    pub fn next(&self) -> Option<KvCursor> {
        match self {
            KvPage::String { bytes, total_len } => {
                ((bytes.len() as u64) < *total_len).then_some(KvCursor::Index(bytes.len() as i64))
            }
            KvPage::Hash { cursor, .. } | KvPage::Set { cursor, .. } => {
                (*cursor != 0).then_some(KvCursor::Scan(*cursor))
            }
            KvPage::List {
                items,
                start,
                total,
            } => {
                let end = *start + items.len() as i64;
                (end < *total as i64).then_some(KvCursor::Index(end))
            }
            KvPage::ZSet {
                members,
                start,
                total,
            } => {
                let end = *start + members.len() as i64;
                (end < *total as i64).then_some(KvCursor::Index(end))
            }
            KvPage::Stream { next, .. } => next.clone().map(KvCursor::After),
        }
    }

    /// Elements (or string bytes) loaded.
    pub fn len(&self) -> usize {
        match self {
            KvPage::String { bytes, .. } => bytes.len(),
            KvPage::Hash { fields, .. } => fields.len(),
            KvPage::List { items, .. } => items.len(),
            KvPage::Set { members, .. } => members.len(),
            KvPage::ZSet { members, .. } => members.len(),
            KvPage::Stream { entries, .. } => entries.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Appends `next`, the page that follows this one. Fields and members a `SCAN`-family
    /// cursor returns twice (it may, while the server rehashes) are kept once. A page of
    /// another kind replaces this one.
    pub fn append(&mut self, next: KvPage) {
        let mut seen = HashSet::new();
        self.names(&mut seen);
        self.append_dedup(next, &mut seen);
    }

    /// Adds the hash fields or set members of this page to `seen`.
    pub fn names(&self, seen: &mut HashSet<Vec<u8>>) {
        match self {
            KvPage::Hash { fields, .. } => seen.extend(fields.iter().map(|(f, _)| f.clone())),
            KvPage::Set { members, .. } => seen.extend(members.iter().cloned()),
            _ => {}
        }
    }

    /// Like [`KvPage::append`], with the names already loaded kept in `seen` between calls,
    /// so a long value is not indexed again for every page. `seen` is updated.
    pub fn append_dedup(&mut self, next: KvPage, seen: &mut HashSet<Vec<u8>>) {
        match (&mut *self, next) {
            (
                KvPage::String { bytes, total_len },
                KvPage::String {
                    bytes: more,
                    total_len: total,
                },
            ) => {
                bytes.extend(more);
                *total_len = total;
            }
            (
                KvPage::Hash { fields, cursor },
                KvPage::Hash {
                    fields: more,
                    cursor: next_cursor,
                },
            ) => {
                fields.extend(more.into_iter().filter(|(f, _)| seen.insert(f.clone())));
                *cursor = next_cursor;
            }
            (
                KvPage::Set { members, cursor },
                KvPage::Set {
                    members: more,
                    cursor: next_cursor,
                },
            ) => {
                members.extend(more.into_iter().filter(|m| seen.insert(m.clone())));
                *cursor = next_cursor;
            }
            (
                KvPage::List { items, total, .. },
                KvPage::List {
                    items: more,
                    total: t,
                    ..
                },
            ) => {
                items.extend(more);
                *total = t;
            }
            (
                KvPage::ZSet { members, total, .. },
                KvPage::ZSet {
                    members: more,
                    total: t,
                    ..
                },
            ) => {
                members.extend(more);
                *total = t;
            }
            (
                KvPage::Stream {
                    entries,
                    next,
                    total,
                },
                KvPage::Stream {
                    entries: more,
                    next: n,
                    total: t,
                },
            ) => {
                entries.extend(more);
                *next = n;
                *total = t;
            }
            (this, other) => {
                *this = other;
                seen.clear();
                this.names(seen);
            }
        }
    }
}

/// A change to one key.
#[derive(Clone, Debug, PartialEq)]
pub enum KvEdit {
    SetString(Vec<u8>),
    HashSet(Vec<u8>, Vec<u8>),
    HashDel(Vec<u8>),
    ListSet(i64, Vec<u8>),
    ListPush {
        left: bool,
        value: Vec<u8>,
    },
    /// Removes the item at `index`, when it still equals `value`: the edit is refused if the
    /// list changed since the item was shown.
    ListRem {
        index: i64,
        value: Vec<u8>,
    },
    SetAdd(Vec<u8>),
    SetRem(Vec<u8>),
    ZAdd(Vec<u8>, f64),
    ZRem(Vec<u8>),
    XAdd(Vec<(Vec<u8>, Vec<u8>)>),
    XDel(String),
    /// Milliseconds to live; `None` removes the expiry (`PERSIST`).
    Expire(Option<i64>),
    /// Renames the key; refused when the new name exists.
    Rename(Vec<u8>),
    Delete,
    /// Creates the key with the first element `0` gives (`SetString`, `HashSet`,
    /// `ListPush`, `SetAdd`, `ZAdd` or `XAdd`); refused when the key already exists.
    Create(Box<KvEdit>),
}

/// A RESP2/RESP3 reply.
#[derive(Clone, Debug, PartialEq)]
pub enum RespValue {
    Nil,
    Int(i64),
    Simple(String),
    Bulk(Vec<u8>),
    Array(Vec<RespValue>),
    Map(Vec<(RespValue, RespValue)>),
    Double(f64),
    Bool(bool),
    Error(String),
    Verbatim(String),
}

/// The key-value family (Redis). `db` is the logical database (`SELECT`).
#[async_trait::async_trait]
pub trait KeyValueConn: Send + Sync {
    /// One `SCAN` call: the keys it returned (with type and TTL) and the next cursor
    /// (0 once the scan is complete). Never `KEYS`.
    async fn scan(
        &self,
        db: u32,
        pattern: &[u8],
        cursor: u64,
        count: usize,
    ) -> Result<(Vec<KeyInfo>, u64), DriverError>;
    /// The key's type, TTL, memory size and length; `None` when it does not exist.
    async fn key_info(&self, db: u32, key: &[u8]) -> Result<Option<KeyInfo>, DriverError>;
    /// One page of the key's value starting at `from`: `count` elements, or
    /// [`STRING_PAGE_BYTES`] of a string.
    async fn page(
        &self,
        db: u32,
        key: &[u8],
        from: KvCursor,
        count: usize,
    ) -> Result<KvPage, DriverError>;
    async fn edit(&self, db: u32, key: &[u8], edit: KvEdit) -> Result<(), DriverError>;
    /// Runs one console command. A server error reply is `Ok(RespValue::Error)`.
    async fn command(&self, db: u32, args: Vec<Vec<u8>>) -> Result<RespValue, DriverError>;
    /// Runs `commands` as one `MULTI`/`EXEC` block; the `EXEC` reply.
    async fn exec_block(
        &self,
        db: u32,
        commands: Vec<Vec<Vec<u8>>>,
    ) -> Result<RespValue, DriverError>;
    /// The class of a console command, for read-only sessions and confirmations.
    fn classify(&self, args: &[Vec<u8>]) -> StatementClass;
    /// The number of logical databases.
    fn databases(&self) -> u32;
    /// Turns read-only mode on or off: commands and edits that may write are refused with
    /// [`crate::read_only_error`].
    fn set_read_only(&self, on: bool);
}

/// `name` for display: as is when it is valid UTF-8 without control characters; otherwise
/// each invalid byte and control character escaped as `\xNN` (and `\` as `\\`).
pub fn display_key(name: &[u8]) -> String {
    let plain = std::str::from_utf8(name)
        .ok()
        .filter(|text| !text.chars().any(char::is_control));
    if let Some(text) = plain {
        return text.to_string();
    }
    let mut out = String::with_capacity(name.len());
    for chunk in name.utf8_chunks() {
        for c in chunk.valid().chars() {
            match c {
                '\\' => out.push_str("\\\\"),
                c if c.is_control() => {
                    let mut buf = [0; 4];
                    for b in c.encode_utf8(&mut buf).bytes() {
                        let _ = write!(out, "\\x{b:02x}");
                    }
                }
                c => out.push(c),
            }
        }
        for b in chunk.invalid() {
            let _ = write!(out, "\\x{b:02x}");
        }
    }
    out
}

/// `bytes` as a glob that matches them literally (`*`, `?`, `[`, `]` and `\` escaped).
pub fn glob_escape(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    for &b in bytes {
        if matches!(b, b'*' | b'?' | b'[' | b']' | b'\\') {
            out.push(b'\\');
        }
        out.push(b);
    }
    out
}

/// Splits a console line into arguments like `redis-cli`: whitespace separates them;
/// double quotes take `\n`, `\r`, `\t`, `\b`, `\a`, `\\`, `\"` and `\xHH` escapes; single
/// quotes take `\'`. A closing quote must end the argument.
pub fn split_command_line(line: &str) -> Result<Vec<Vec<u8>>, String> {
    fn push(arg: &mut Vec<u8>, c: char) {
        let mut buf = [0; 4];
        arg.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
    }
    let mut args = Vec::new();
    let mut chars = line.chars().peekable();
    loop {
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        let Some(first) = chars.peek().copied() else {
            return Ok(args);
        };
        let mut arg: Vec<u8> = Vec::new();
        if first == '"' || first == '\'' {
            let quote = first;
            chars.next();
            loop {
                let Some(c) = chars.next() else {
                    return Err("unbalanced quotes in the command".into());
                };
                if c == quote {
                    if chars.peek().is_some_and(|n| !n.is_whitespace()) {
                        return Err("a closing quote must be followed by a space".into());
                    }
                    break;
                }
                if c != '\\' {
                    push(&mut arg, c);
                    continue;
                }
                let Some(e) = chars.next() else {
                    return Err("unbalanced quotes in the command".into());
                };
                if quote == '\'' {
                    if e != '\'' {
                        arg.push(b'\\');
                    }
                    push(&mut arg, e);
                    continue;
                }
                match e {
                    'n' => arg.push(b'\n'),
                    'r' => arg.push(b'\r'),
                    't' => arg.push(b'\t'),
                    'b' => arg.push(8),
                    'a' => arg.push(7),
                    'x' => {
                        let mut ahead = chars.clone();
                        let byte = ahead.next().and_then(|h| h.to_digit(16)).and_then(|hi| {
                            let lo = ahead.next()?.to_digit(16)?;
                            Some((hi * 16 + lo) as u8)
                        });
                        match byte {
                            Some(byte) => {
                                chars.next();
                                chars.next();
                                arg.push(byte);
                            }
                            None => arg.push(b'x'),
                        }
                    }
                    other => push(&mut arg, other),
                }
            }
        } else {
            while let Some(c) = chars.next_if(|c| !c.is_whitespace()) {
                push(&mut arg, c);
            }
        }
        args.push(arg);
    }
}

/// A key name typed in a field, read with the `redis-cli` quoting rules (the inverse of
/// [`quote_bytes`]): one argument, bare or quoted. Bytes that are not UTF-8 are written
/// `\xNN` inside double quotes.
pub fn parse_key_name(text: &str) -> Result<Vec<u8>, String> {
    let mut args = split_command_line(text)?;
    match args.len() {
        0 => Ok(Vec::new()),
        1 => Ok(args.remove(0)),
        _ => Err("Put the name in double quotes when it has spaces".into()),
    }
}

/// `args` as one line for the console output and the history, with secrets (passwords
/// of `AUTH`, `HELLO … AUTH`, `ACL SETUSER`, `CONFIG SET requirepass|masterauth`,
/// `MIGRATE … AUTH|AUTH2`) replaced by `***`.
pub fn redact_command(args: &[Vec<u8>]) -> String {
    let word = |i: usize| -> String {
        args.get(i)
            .map(|a| String::from_utf8_lossy(a).to_ascii_uppercase())
            .unwrap_or_default()
    };
    let mut secret = vec![false; args.len()];
    match word(0).as_str() {
        // AUTH [user] password
        "AUTH" => {
            if args.len() > 1 {
                secret[args.len() - 1] = true;
            }
        }
        // HELLO [version [AUTH user password] [SETNAME name]]
        "HELLO" => {
            if let Some(at) = (1..args.len()).find(|&i| word(i) == "AUTH")
                && let Some(slot) = secret.get_mut(at + 2)
            {
                *slot = true;
            }
        }
        "CONFIG" if word(1) == "SET" => {
            let mut i = 2;
            while i + 1 < args.len() {
                if matches!(
                    word(i).as_str(),
                    "REQUIREPASS" | "MASTERAUTH" | "TLS-KEY-FILE-PASS" | "TLS-CLIENT-KEY-FILE-PASS"
                ) {
                    secret[i + 1] = true;
                }
                i += 2;
            }
        }
        "ACL" if word(1) == "SETUSER" => {
            for (i, a) in args.iter().enumerate().skip(3) {
                if matches!(a.first(), Some(b'>' | b'<' | b'#' | b'!')) {
                    secret[i] = true;
                }
            }
        }
        // MIGRATE host port key db timeout [AUTH password | AUTH2 user password]
        "MIGRATE" => {
            for i in 1..args.len() {
                let hidden = match word(i).as_str() {
                    "AUTH" => i + 1,
                    "AUTH2" => i + 2,
                    _ => continue,
                };
                if let Some(slot) = secret.get_mut(hidden) {
                    *slot = true;
                }
            }
        }
        _ => {}
    }
    args.iter()
        .zip(&secret)
        .map(|(a, hidden)| {
            if *hidden {
                "***".to_string()
            } else {
                quote_arg(a)
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// `arg` as a console word: bare when it needs no quoting, else quoted by [`quote_bytes`].
fn quote_arg(arg: &[u8]) -> String {
    let bare = !arg.is_empty()
        && std::str::from_utf8(arg).is_ok_and(|t| {
            !t.chars()
                .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '"' | '\'' | '\\'))
        });
    if bare {
        return String::from_utf8_lossy(arg).into_owned();
    }
    quote_bytes(arg)
}

/// `bytes` in double quotes with `redis-cli` escapes: `\"`, `\\`, `\n`, `\r`, `\t`, and
/// `\xNN` for control characters and bytes that are not UTF-8.
pub fn quote_bytes(arg: &[u8]) -> String {
    let mut out = String::from("\"");
    for chunk in arg.utf8_chunks() {
        for c in chunk.valid().chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if c.is_control() => {
                    let mut buf = [0; 4];
                    for b in c.encode_utf8(&mut buf).bytes() {
                        let _ = write!(out, "\\x{b:02x}");
                    }
                }
                c => out.push(c),
            }
        }
        for b in chunk.invalid() {
            let _ = write!(out, "\\x{b:02x}");
        }
    }
    out.push('"');
    out
}

/// Explorer paths of a key-value connection: `["db<n>", group…, key]`, where a group
/// segment is `g:` plus the hex of its prefix (separator included) and a key segment is
/// `k:` plus the hex of its name, so binary names survive.
pub struct KvNodePath;

impl KvNodePath {
    pub fn database(db: u32) -> NodePath {
        NodePath::root().child(&format!("db{db}"))
    }

    pub fn group(parent: &NodePath, prefix: &[u8]) -> NodePath {
        parent.child(&format!("g:{}", hex(prefix)))
    }

    pub fn key(parent: &NodePath, name: &[u8]) -> NodePath {
        parent.child(&format!("k:{}", hex(name)))
    }

    /// The database a path is under.
    pub fn db_of(path: &NodePath) -> Option<u32> {
        path.segments().first()?.strip_prefix("db")?.parse().ok()
    }

    /// The prefix of a group path (its last segment).
    pub fn prefix_of(path: &NodePath) -> Option<Vec<u8>> {
        unhex(path.last()?.strip_prefix("g:")?)
    }

    /// The key name of a key path (its last segment).
    pub fn key_of(path: &NodePath) -> Option<Vec<u8>> {
        unhex(path.last()?.strip_prefix("k:")?)
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.is_ascii() || !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<Vec<u8>> {
        list.iter().map(|a| a.as_bytes().to_vec()).collect()
    }

    #[test]
    fn command_lines_split_like_redis_cli() {
        assert_eq!(
            split_command_line("  SET  k  v ").unwrap(),
            args(&["SET", "k", "v"])
        );
        assert_eq!(
            split_command_line(r#"SET "a b" 'c d'"#).unwrap(),
            args(&["SET", "a b", "c d"])
        );
        assert_eq!(
            split_command_line(r#"SET k "line\nnext\t\"q\" \x41\xff""#).unwrap(),
            vec![
                b"SET".to_vec(),
                b"k".to_vec(),
                b"line\nnext\t\"q\" A\xff".to_vec()
            ]
        );
        assert_eq!(
            split_command_line(r"SET k 'it\'s \n'").unwrap(),
            args(&["SET", "k", "it's \\n"])
        );
        assert_eq!(
            split_command_line("SET k \"\"").unwrap(),
            args(&["SET", "k", ""])
        );
        assert_eq!(split_command_line("   ").unwrap(), Vec::<Vec<u8>>::new());
        assert!(split_command_line("SET \"open").is_err());
        assert!(split_command_line("SET 'open").is_err());
        assert!(split_command_line("SET \"a\"b").is_err());
        // Multibyte text stays intact.
        assert_eq!(
            split_command_line("SET clé \"été\"").unwrap(),
            args(&["SET", "clé", "été"])
        );
    }

    #[test]
    fn binary_names_display_escaped() {
        assert_eq!(display_key(b"user:1"), "user:1");
        assert_eq!(display_key("clé".as_bytes()), "clé");
        assert_eq!(display_key(b"a\xffb"), "a\\xffb");
        assert_eq!(display_key(b"a\\b\xfe"), "a\\\\b\\xfe");
        assert_eq!(display_key(b"tab\there"), "tab\\x09here");
        // A valid name with a backslash shows as is.
        assert_eq!(display_key(b"a\\b"), "a\\b");
    }

    #[test]
    fn globs_match_literally() {
        assert_eq!(glob_escape(b"a*b?[c]\\"), b"a\\*b\\?\\[c\\]\\\\".to_vec());
        assert_eq!(glob_escape(b"user:"), b"user:".to_vec());
    }

    #[test]
    fn secrets_are_redacted() {
        assert_eq!(redact_command(&args(&["AUTH", "pw"])), "AUTH ***");
        assert_eq!(
            redact_command(&args(&["auth", "user", "pw"])),
            "auth user ***"
        );
        assert_eq!(
            redact_command(&args(&["HELLO", "3", "AUTH", "u", "pw", "SETNAME", "x"])),
            "HELLO 3 AUTH u *** SETNAME x"
        );
        assert_eq!(
            redact_command(&args(&["CONFIG", "SET", "requirepass", "pw"])),
            "CONFIG SET requirepass ***"
        );
        assert_eq!(
            redact_command(&args(&["CONFIG", "SET", "maxmemory", "1gb"])),
            "CONFIG SET maxmemory 1gb"
        );
        assert_eq!(
            redact_command(&args(&["ACL", "SETUSER", "bob", "on", ">pw", "#abc", "~*"])),
            "ACL SETUSER bob on *** *** ~*"
        );
        assert_eq!(
            redact_command(&args(&[
                "MIGRATE", "h", "1", "k", "0", "5", "AUTH2", "u", "pw"
            ])),
            "MIGRATE h 1 k 0 5 AUTH2 u ***"
        );
        // Arguments that need quoting are quoted; binary bytes are escaped.
        assert_eq!(
            redact_command(&[b"SET".to_vec(), b"a b".to_vec(), b"\xff".to_vec()]),
            "SET \"a b\" \"\\xff\""
        );
        assert_eq!(redact_command(&args(&["SET", "k", ""])), "SET k \"\"");
    }

    #[test]
    fn a_seen_set_carries_over_between_pages() {
        let mut seen = HashSet::new();
        let mut page = KvPage::Hash {
            fields: vec![(b"a".to_vec(), b"1".to_vec())],
            cursor: 5,
        };
        page.names(&mut seen);
        for (cursor, names) in [(7, ["a", "b"]), (0, ["b", "c"])] {
            page.append_dedup(
                KvPage::Hash {
                    fields: names
                        .iter()
                        .map(|n| (n.as_bytes().to_vec(), b"v".to_vec()))
                        .collect(),
                    cursor,
                },
                &mut seen,
            );
        }
        let KvPage::Hash { fields, cursor } = &page else {
            panic!()
        };
        let names: Vec<&[u8]> = fields.iter().map(|(f, _)| f.as_slice()).collect();
        assert_eq!(names, [b"a".as_slice(), b"b", b"c"]);
        assert_eq!(*cursor, 0);
        assert_eq!(seen.len(), 3);
        // A page of another kind starts over.
        page.append_dedup(
            KvPage::Set {
                members: vec![b"m".to_vec()],
                cursor: 0,
            },
            &mut seen,
        );
        assert_eq!(seen, HashSet::from([b"m".to_vec()]));
    }

    #[test]
    fn key_names_parse_back_from_their_quoted_form() {
        for name in [
            &b"plain"[..],
            b"a\xffb",
            b"sp ace",
            b"q\"uote\\",
            b"\x00\n",
            "été".as_bytes(),
        ] {
            assert_eq!(parse_key_name(&quote_bytes(name)).unwrap(), name);
        }
        assert_eq!(parse_key_name("bare").unwrap(), b"bare");
        assert_eq!(parse_key_name("").unwrap(), b"");
        assert!(parse_key_name("two words").is_err());
        assert!(parse_key_name("\"open").is_err());
    }

    #[test]
    fn bytes_quote_like_redis_cli() {
        assert_eq!(quote_bytes(b"a b"), "\"a b\"");
        assert_eq!(quote_bytes(b"q\"\\\n"), "\"q\\\"\\\\\\n\"");
        assert_eq!(quote_bytes(b"\xff\x00"), "\"\\xff\\x00\"");
        assert_eq!(quote_bytes(b""), "\"\"");
        assert_eq!(quote_bytes("été".as_bytes()), "\"été\"");
    }

    #[test]
    fn node_paths_round_trip_binary_names() {
        let db = KvNodePath::database(3);
        assert_eq!(KvNodePath::db_of(&db), Some(3));
        let group = KvNodePath::group(&db, b"user:");
        assert_eq!(KvNodePath::prefix_of(&group), Some(b"user:".to_vec()));
        let key = KvNodePath::key(&group, b"user:\xff");
        assert_eq!(KvNodePath::key_of(&key), Some(b"user:\xff".to_vec()));
        assert_eq!(KvNodePath::db_of(&key), Some(3));
        assert_eq!(KvNodePath::key_of(&group), None);
        assert_eq!(KvNodePath::db_of(&NodePath::root().child("x")), None);
    }

    #[test]
    fn pages_append_and_say_where_the_next_starts() {
        let mut hash = KvPage::Hash {
            fields: vec![(b"a".to_vec(), b"1".to_vec())],
            cursor: 7,
        };
        assert_eq!(hash.next(), Some(KvCursor::Scan(7)));
        hash.append(KvPage::Hash {
            fields: vec![
                (b"a".to_vec(), b"1".to_vec()),
                (b"b".to_vec(), b"2".to_vec()),
            ],
            cursor: 0,
        });
        assert_eq!(hash.len(), 2);
        assert_eq!(hash.next(), None);

        let mut list = KvPage::List {
            items: vec![b"x".to_vec(); 2],
            start: 0,
            total: 3,
        };
        assert_eq!(list.next(), Some(KvCursor::Index(2)));
        list.append(KvPage::List {
            items: vec![b"y".to_vec()],
            start: 2,
            total: 3,
        });
        assert_eq!(list.len(), 3);
        assert_eq!(list.next(), None);

        let mut text = KvPage::String {
            bytes: b"ab".to_vec(),
            total_len: 4,
        };
        assert_eq!(text.next(), Some(KvCursor::Index(2)));
        text.append(KvPage::String {
            bytes: b"cd".to_vec(),
            total_len: 4,
        });
        assert_eq!(
            text,
            KvPage::String {
                bytes: b"abcd".to_vec(),
                total_len: 4
            }
        );
        assert_eq!(text.next(), None);

        let mut stream = KvPage::Stream {
            entries: vec![("1-0".into(), vec![])],
            next: Some("1-0".into()),
            total: 2,
        };
        assert_eq!(stream.next(), Some(KvCursor::After("1-0".into())));
        stream.append(KvPage::Stream {
            entries: vec![("2-0".into(), vec![])],
            next: None,
            total: 2,
        });
        assert_eq!(stream.len(), 2);
        assert_eq!(stream.next(), None);

        let mut zset = KvPage::ZSet {
            members: vec![(b"m".to_vec(), 1.0)],
            start: 0,
            total: 1,
        };
        assert_eq!(zset.next(), None);
        // A page of another kind (the key changed type) replaces the value.
        zset.append(KvPage::Set {
            members: vec![b"s".to_vec()],
            cursor: 0,
        });
        assert!(matches!(zset, KvPage::Set { .. }));
    }
}
