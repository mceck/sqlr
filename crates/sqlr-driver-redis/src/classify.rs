//! Command classes for read-only sessions and confirmations (spec §7.9).
//!
//! A command is a `Read` only when it is on the list below. Anything the list does not know
//! (module commands, scripts) is `Other`, which counts as a write.

use sqlr_core::StatementClass;

/// Commands that only read (or have no effect on data).
const READ: &[&str] = &[
    "GET",
    "GETRANGE",
    "GETBIT",
    "STRLEN",
    "SUBSTR",
    "MGET",
    "EXISTS",
    "TYPE",
    "TTL",
    "PTTL",
    "EXPIRETIME",
    "PEXPIRETIME",
    "SCAN",
    "RANDOMKEY",
    "DBSIZE",
    "DUMP",
    "HGET",
    "HMGET",
    "HGETALL",
    "HKEYS",
    "HVALS",
    "HLEN",
    "HEXISTS",
    "HSTRLEN",
    "HSCAN",
    "HRANDFIELD",
    "LLEN",
    "LRANGE",
    "LINDEX",
    "LPOS",
    "SCARD",
    "SMEMBERS",
    "SISMEMBER",
    "SMISMEMBER",
    "SSCAN",
    "SRANDMEMBER",
    "SINTER",
    "SUNION",
    "SDIFF",
    "SINTERCARD",
    "ZINTERCARD",
    "ZCARD",
    "ZRANGE",
    "ZRANGEBYSCORE",
    "ZRANGEBYLEX",
    "ZREVRANGE",
    "ZREVRANGEBYSCORE",
    "ZREVRANGEBYLEX",
    "ZRANK",
    "ZREVRANK",
    "ZSCORE",
    "ZMSCORE",
    "ZCOUNT",
    "ZLEXCOUNT",
    "ZSCAN",
    "ZRANDMEMBER",
    "ZINTER",
    "ZUNION",
    "ZDIFF",
    "XRANGE",
    "XREVRANGE",
    "XLEN",
    "XPENDING",
    "XREAD",
    "BITCOUNT",
    "BITPOS",
    "BITFIELD_RO",
    "PFCOUNT",
    "GEOPOS",
    "GEODIST",
    "GEOHASH",
    "GEORADIUS_RO",
    "GEORADIUSBYMEMBER_RO",
    "GEOSEARCH",
    "SORT_RO",
    "EVAL_RO",
    "EVALSHA_RO",
    "FCALL_RO",
    "LCS",
    "LOLWUT",
    "ECHO",
    "PING",
    "TIME",
    "INFO",
    "LASTSAVE",
    "COMMAND",
    "PUBSUB",
    "TOUCH",
    // Session commands: no effect on data (the console handles `MULTI` blocks itself, and
    // classifies each queued command on its own).
    "MULTI",
    "EXEC",
    "DISCARD",
    "WATCH",
    "UNWATCH",
    "SELECT",
    "SUBSCRIBE",
    "PSUBSCRIBE",
    "SSUBSCRIBE",
    "UNSUBSCRIBE",
    "PUNSUBSCRIBE",
    "SUNSUBSCRIBE",
    "MONITOR",
    "QUIT",
    "HELLO",
];

/// Commands that write one key or a few.
const WRITE: &[&str] = &[
    "SET",
    "SETNX",
    "SETEX",
    "PSETEX",
    "MSET",
    "MSETNX",
    "APPEND",
    "SETRANGE",
    "SETBIT",
    "INCR",
    "INCRBY",
    "INCRBYFLOAT",
    "DECR",
    "DECRBY",
    "GETSET",
    "GETDEL",
    "GETEX",
    "EXPIRE",
    "PEXPIRE",
    "EXPIREAT",
    "PEXPIREAT",
    "PERSIST",
    "RENAME",
    "RENAMENX",
    "MOVE",
    "COPY",
    "RESTORE",
    "HSET",
    "HSETNX",
    "HMSET",
    "HDEL",
    "HINCRBY",
    "HINCRBYFLOAT",
    "LPUSH",
    "LPUSHX",
    "RPUSH",
    "RPUSHX",
    "LPOP",
    "RPOP",
    "LSET",
    "LREM",
    "LTRIM",
    "LINSERT",
    "LMOVE",
    "RPOPLPUSH",
    "BLPOP",
    "BRPOP",
    "BLMOVE",
    "BRPOPLPUSH",
    "LMPOP",
    "BLMPOP",
    "SADD",
    "SREM",
    "SPOP",
    "SMOVE",
    "SINTERSTORE",
    "SUNIONSTORE",
    "SDIFFSTORE",
    "ZADD",
    "ZREM",
    "ZINCRBY",
    "ZPOPMIN",
    "ZPOPMAX",
    "BZPOPMIN",
    "BZPOPMAX",
    "ZREMRANGEBYRANK",
    "ZREMRANGEBYSCORE",
    "ZREMRANGEBYLEX",
    "ZINTERSTORE",
    "ZUNIONSTORE",
    "ZDIFFSTORE",
    "ZRANGESTORE",
    "ZMPOP",
    "BZMPOP",
    "XADD",
    "XDEL",
    "XTRIM",
    "XGROUP",
    "XACK",
    "XCLAIM",
    "XAUTOCLAIM",
    "XREADGROUP",
    "XSETID",
    "PFADD",
    "PFMERGE",
    "GEOADD",
    "GEORADIUS",
    "GEORADIUSBYMEMBER",
    "GEOSEARCHSTORE",
    "BITOP",
    "BITFIELD",
    "PUBLISH",
    "SPUBLISH",
    "WAIT",
];

/// Commands that delete keys: destructive from two keys on.
const DELETE: &[&str] = &["DEL", "UNLINK"];

/// Commands that wipe data or stop the server.
const WIPE: &[&str] = &["FLUSHDB", "FLUSHALL", "SHUTDOWN"];

/// Whether `flag` is a `MIGRATE` option: it comes after the fixed arguments and before
/// any `KEYS` keys, and is not the argument of `AUTH`/`AUTH2`.
fn migrate_option(word: &dyn Fn(usize) -> String, len: usize, flag: &str) -> bool {
    let mut i = 6;
    while i < len {
        match word(i).as_str() {
            "AUTH" => i += 2,
            "AUTH2" => i += 3,
            "KEYS" => return false,
            other if other == flag => return true,
            _ => i += 1,
        }
    }
    false
}

/// Why a command is destructive beyond the wipe and delete commands: it overwrites data,
/// changes the server's role, stops it, blocks it, or runs code whose effects are unknown.
fn dangerous(name: &str, len: usize, word: &dyn Fn(usize) -> String) -> Option<String> {
    // Options start after the command's fixed arguments; a key or a password that happens
    // to spell an option is not that option.
    let has_from = |start: usize, flag: &str| (start..len).any(|i| word(i) == flag);
    let sub = word(1);
    let reason = match name {
        "REPLICAOF" | "SLAVEOF" => "changes the server's role",
        "SWAPDB" => "swaps two databases",
        "FAILOVER" => "fails the server over",
        "DEBUG" => "DEBUG can crash or stall the server",
        "KEYS" => "KEYS blocks the server",
        "MODULE" if matches!(sub.as_str(), "LOAD" | "LOADEX" | "UNLOAD") => {
            "loads or unloads a module"
        }
        "SCRIPT" if sub == "FLUSH" => "SCRIPT FLUSH",
        "MIGRATE" if !migrate_option(word, len, "COPY") => "MIGRATE moves the key off this server",
        // `RESTORE key ttl payload [REPLACE] …`, `COPY source destination [DB db] [REPLACE]`.
        "RESTORE" if has_from(4, "REPLACE") => "overwrites an existing key",
        "COPY" if has_from(3, "REPLACE") => "overwrites an existing key",
        // The read-only variants (`EVAL_RO`, ...) cannot write and are reads.
        "EVAL" | "EVALSHA" | "FCALL" => "script: effects unknown",
        _ => return None,
    };
    Some(format!("{name}: {reason}"))
}

/// The class of the console command `args` (case-insensitive; the first argument is the
/// command name).
pub(crate) fn classify(args: &[Vec<u8>]) -> StatementClass {
    let word = |i: usize| -> String {
        args.get(i)
            .map(|a| String::from_utf8_lossy(a).to_ascii_uppercase())
            .unwrap_or_default()
    };
    let name = word(0);
    let name = name.as_str();
    if WIPE.contains(&name) {
        return StatementClass::Destructive(name.to_string());
    }
    if let Some(reason) = dangerous(name, args.len(), &word) {
        return StatementClass::Destructive(reason);
    }
    if DELETE.contains(&name) {
        return match args.len() {
            0..=2 => StatementClass::Write,
            n => StatementClass::Destructive(format!("{name} {} keys", n - 1)),
        };
    }
    if READ.contains(&name) {
        return StatementClass::Read;
    }
    if WRITE.contains(&name) {
        return StatementClass::Write;
    }
    let sub = word(1);
    let sub = sub.as_str();
    match name {
        // `SORT … STORE dest` writes.
        "SORT" => {
            if (1..args.len()).any(|i| word(i) == "STORE") {
                StatementClass::Write
            } else {
                StatementClass::Read
            }
        }
        "OBJECT" => StatementClass::Read,
        "MEMORY" if matches!(sub, "USAGE" | "STATS" | "DOCTOR" | "MALLOC-STATS") => {
            StatementClass::Read
        }
        "CONFIG" if sub == "GET" => StatementClass::Read,
        "CLIENT" if matches!(sub, "ID" | "INFO" | "LIST" | "GETNAME" | "GETREDIR") => {
            StatementClass::Read
        }
        "SLOWLOG" if matches!(sub, "GET" | "LEN") => StatementClass::Read,
        "LATENCY" if matches!(sub, "LATEST" | "HISTORY" | "GRAPH" | "DOCTOR") => {
            StatementClass::Read
        }
        "XINFO" => StatementClass::Read,
        "ACL"
            if matches!(
                sub,
                "WHOAMI" | "LIST" | "USERS" | "GETUSER" | "CAT" | "GENPASS"
            ) =>
        {
            StatementClass::Read
        }
        "ACL" if matches!(sub, "SETUSER" | "DELUSER" | "LOAD" | "SAVE") => StatementClass::Ddl,
        "FUNCTION" if matches!(sub, "LIST" | "STATS" | "DUMP") => StatementClass::Read,
        "FUNCTION" if matches!(sub, "LOAD" | "DELETE" | "RESTORE") => StatementClass::Ddl,
        "FUNCTION" if sub == "FLUSH" => StatementClass::Destructive("FUNCTION FLUSH".into()),
        "SCRIPT" if sub == "EXISTS" => StatementClass::Read,
        "SCRIPT" if sub == "LOAD" => StatementClass::Write,
        _ => StatementClass::Other,
    }
}

/// Why the console cannot run `args` on its shared connection, when it cannot: the command
/// would change the connection's state or take it over.
pub(crate) fn refused_in_console(args: &[Vec<u8>]) -> Option<&'static str> {
    let word = |i: usize| -> String {
        args.get(i)
            .map(|a| String::from_utf8_lossy(a).to_ascii_uppercase())
            .unwrap_or_default()
    };
    match word(0).as_str() {
        "SUBSCRIBE" | "PSUBSCRIBE" | "SSUBSCRIBE" | "UNSUBSCRIBE" | "PUNSUBSCRIBE"
        | "SUNSUBSCRIBE" => Some("subscriptions are not supported in the console"),
        "MONITOR" => Some("MONITOR is not supported in the console"),
        "QUIT" => Some("close the tab to disconnect"),
        "SELECT" => Some("SELECT is handled by the console itself"),
        "HELLO" | "RESET" => Some("the console keeps its own protocol and session"),
        "SYNC" | "PSYNC" => Some("replication takes over the console's connection"),
        "MULTI" | "EXEC" | "DISCARD" | "WATCH" | "UNWATCH" => {
            Some("MULTI blocks are handled by the console itself")
        }
        "CLIENT" if matches!(word(1).as_str(), "REPLY" | "TRACKING" | "CACHING" | "KILL") => {
            Some("this CLIENT command would break the console's connection")
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn class(line: &[&str]) -> StatementClass {
        classify(
            &line
                .iter()
                .map(|a| a.as_bytes().to_vec())
                .collect::<Vec<_>>(),
        )
    }

    #[test]
    fn reads_writes_and_unknowns() {
        assert_eq!(class(&["get", "k"]), StatementClass::Read);
        assert_eq!(class(&["HGETALL", "k"]), StatementClass::Read);
        assert_eq!(class(&["SET", "k", "v"]), StatementClass::Write);
        assert_eq!(class(&["BLPOP", "q", "0"]), StatementClass::Write);
        assert_eq!(class(&["MYMODULE.DO"]), StatementClass::Other);
        assert_eq!(class(&[]), StatementClass::Other);
        assert!(class(&["EVAL", "return 1", "0"]).modifies());
        assert!(!class(&["EVAL_RO", "return 1", "0"]).modifies());
        assert!(class(&["FCALL", "f", "0"]).modifies());
    }

    #[test]
    fn reads_added_in_redis_7_are_reads() {
        assert_eq!(class(&["LCS", "a", "b"]), StatementClass::Read);
        assert_eq!(
            class(&["ZINTERCARD", "2", "z1", "z2"]),
            StatementClass::Read
        );
    }

    #[test]
    fn deletes_are_destructive_from_two_keys() {
        assert_eq!(class(&["DEL", "a"]), StatementClass::Write);
        assert!(matches!(
            class(&["DEL", "a", "b"]),
            StatementClass::Destructive(_)
        ));
        assert!(
            matches!(class(&["unlink", "a", "b", "c"]), StatementClass::Destructive(r) if r.contains("3 keys"))
        );
        for wipe in ["FLUSHDB", "FLUSHALL", "SHUTDOWN"] {
            assert!(
                matches!(class(&[wipe]), StatementClass::Destructive(_)),
                "{wipe}"
            );
        }
        assert!(matches!(
            class(&["FUNCTION", "FLUSH"]),
            StatementClass::Destructive(_)
        ));
    }

    #[test]
    fn subcommands_decide() {
        assert_eq!(class(&["CONFIG", "GET", "maxmemory"]), StatementClass::Read);
        assert_eq!(
            class(&["CONFIG", "SET", "maxmemory", "1"]),
            StatementClass::Other
        );
        assert_eq!(class(&["MEMORY", "USAGE", "k"]), StatementClass::Read);
        assert_eq!(class(&["MEMORY", "PURGE"]), StatementClass::Other);
        assert_eq!(class(&["SORT", "k"]), StatementClass::Read);
        assert_eq!(class(&["SORT", "k", "STORE", "d"]), StatementClass::Write);
        assert_eq!(class(&["ACL", "SETUSER", "u"]), StatementClass::Ddl);
        assert_eq!(class(&["ACL", "WHOAMI"]), StatementClass::Read);
        assert_eq!(class(&["CLIENT", "KILL", "ID", "1"]), StatementClass::Other);
        assert_eq!(class(&["OBJECT", "ENCODING", "k"]), StatementClass::Read);
    }

    #[test]
    fn console_refuses_commands_that_take_the_connection_over() {
        let refused = |line: &[&str]| {
            refused_in_console(
                &line
                    .iter()
                    .map(|a| a.as_bytes().to_vec())
                    .collect::<Vec<_>>(),
            )
        };
        for line in [
            &["SUBSCRIBE", "c"][..],
            &["psubscribe", "c*"],
            &["MONITOR"],
            &["QUIT"],
            &["SELECT", "1"],
            &["HELLO", "2"],
            &["MULTI"],
            &["CLIENT", "REPLY", "OFF"],
            &["SYNC"],
            &["PSYNC", "?", "-1"],
        ] {
            assert!(refused(line).is_some(), "{line:?}");
        }
        assert!(refused(&["GET", "k"]).is_none());
        assert!(refused(&["CLIENT", "LIST"]).is_none());
        assert!(refused(&["BLPOP", "q", "0"]).is_none());
    }
}

#[cfg(test)]
mod dangerous {
    use super::*;

    fn class(line: &[&str]) -> StatementClass {
        classify(
            &line
                .iter()
                .map(|a| a.as_bytes().to_vec())
                .collect::<Vec<_>>(),
        )
    }

    fn destructive(line: &[&str]) -> bool {
        matches!(class(line), StatementClass::Destructive(_))
    }

    #[test]
    fn server_wide_and_irreversible_commands_are_destructive() {
        for line in [
            &["REPLICAOF", "host", "6379"][..],
            &["slaveof", "no", "one"],
            &["SWAPDB", "0", "1"],
            &["FAILOVER"],
            &["MODULE", "UNLOAD", "m"],
            &["module", "load", "/x.so"],
            &["MODULE", "LOADEX", "/x.so"],
            &["DEBUG", "SLEEP", "0"],
            &["SCRIPT", "FLUSH"],
            &["script", "flush", "async"],
            &["MIGRATE", "h", "6379", "k", "0", "5"],
            &["RESTORE", "k", "0", "payload", "REPLACE"],
            &["restore", "k", "0", "payload", "replace", "ABSTTL"],
            &["COPY", "a", "b", "REPLACE"],
            &["copy", "a", "b", "db", "1", "replace"],
        ] {
            assert!(destructive(line), "{line:?} is {:?}", class(line));
        }
    }

    #[test]
    fn destructive_options_are_read_from_their_positions() {
        // `COPY` is the key here, not the MIGRATE option: the key still leaves the server.
        assert!(destructive(&["MIGRATE", "h", "6379", "COPY", "0", "5000"]));
        // `COPY` is the AUTH password, not the option.
        assert!(destructive(&[
            "MIGRATE", "h", "6379", "k", "0", "5000", "AUTH", "COPY"
        ]));
        // `COPY` after `KEYS` is a key, not the option.
        assert!(destructive(&[
            "MIGRATE", "h", "6379", "k", "0", "5000", "KEYS", "COPY"
        ]));
        // A key named `REPLACE` is not the COPY/RESTORE option.
        assert!(!destructive(&["COPY", "a", "REPLACE"]));
        assert!(!destructive(&["RESTORE", "REPLACE", "0", "payload"]));
    }

    #[test]
    fn the_safe_forms_of_those_commands_are_not() {
        assert!(!destructive(&[
            "MIGRATE", "h", "6379", "k", "0", "5", "COPY"
        ]));
        assert!(!destructive(&["restore", "k", "0", "payload"]));
        assert!(!destructive(&["COPY", "a", "b"]));
        assert_eq!(class(&["COPY", "a", "b"]), StatementClass::Write);
        assert_eq!(class(&["MODULE", "LIST"]), StatementClass::Other);
        assert_eq!(class(&["SCRIPT", "EXISTS", "x"]), StatementClass::Read);
    }

    #[test]
    fn scripts_have_effects_nobody_can_tell() {
        for line in [
            &["EVAL", "return 1", "0"][..],
            &["evalsha", "abc", "0"],
            &["FCALL", "f", "0"],
        ] {
            assert!(
                matches!(class(line), StatementClass::Destructive(ref r) if r.contains("script")),
                "{line:?}"
            );
        }
        // The read-only variants cannot write.
        for line in [
            &["EVAL_RO", "return 1", "0"][..],
            &["evalsha_ro", "abc", "0"],
            &["FCALL_RO", "f", "0"],
        ] {
            assert_eq!(class(line), StatementClass::Read, "{line:?}");
        }
    }

    #[test]
    fn keys_blocks_the_server() {
        assert!(matches!(
            class(&["KEYS", "*"]),
            StatementClass::Destructive(ref r) if r.contains("KEYS")
        ));
    }
}
