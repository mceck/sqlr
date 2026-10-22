//! The Redis console: a command line with completion and history (↑/↓) under the output
//! of earlier commands, each reply drawn by its RESP type the way `redis-cli` does.

use std::ops::Range;
use std::sync::Arc;

use egui::{
    Align, Align2, Area, Button, CornerRadius, Frame, Id, Key, Margin, Modifiers, Order, Rect,
    RichText, ScrollArea, Stroke, TextEdit,
};
use sqlr_core::{RespValue, quote_bytes};

use crate::key_browser::name_edit;
use crate::theme::{self, Palette, Spacing};

/// What a line of output means, for its color.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RespLineKind {
    Plain,
    Number,
    Nil,
    Error,
    Status,
}

/// One displayed line of a reply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RespLine {
    pub text: String,
    pub kind: RespLineKind,
}

fn line(text: impl Into<String>, kind: RespLineKind) -> RespLine {
    RespLine {
        text: text.into(),
        kind,
    }
}

/// `first` before the first line, `rest` spaces before the others.
fn attach(first: &str, lines: Vec<RespLine>) -> Vec<RespLine> {
    let pad = " ".repeat(first.chars().count());
    lines
        .into_iter()
        .enumerate()
        .map(|(i, l)| RespLine {
            text: format!("{}{}", if i == 0 { first } else { &pad }, l.text),
            kind: l.kind,
        })
        .collect()
}

/// A reply as lines: `(integer) 5`, `"text"`, `(nil)`, `1) …` for arrays (nested ones
/// indented), `1# key => value` for maps, `(error) …` for errors.
pub fn resp_lines(value: &RespValue) -> Vec<RespLine> {
    use RespLineKind::*;
    match value {
        RespValue::Nil => vec![line("(nil)", Nil)],
        RespValue::Int(i) => vec![line(format!("(integer) {i}"), Number)],
        RespValue::Simple(s) => vec![line(s.clone(), Status)],
        RespValue::Bulk(bytes) => vec![line(quote_bytes(bytes), Plain)],
        RespValue::Double(d) => vec![line(format!("(double) {d}"), Number)],
        RespValue::Bool(b) => vec![line(format!("({b})"), Number)],
        RespValue::Error(e) => vec![line(format!("(error) {e}"), Error)],
        RespValue::Verbatim(text) => text
            .lines()
            .map(|l| line(l.to_string(), Plain))
            .chain(text.is_empty().then(|| line("", Plain)))
            .collect(),
        RespValue::Array(items) if items.is_empty() => vec![line("(empty array)", Nil)],
        RespValue::Array(items) => items
            .iter()
            .enumerate()
            .flat_map(|(i, item)| attach(&format!("{}) ", i + 1), resp_lines(item)))
            .collect(),
        RespValue::Map(pairs) if pairs.is_empty() => vec![line("(empty map)", Nil)],
        RespValue::Map(pairs) => pairs
            .iter()
            .enumerate()
            .flat_map(|(i, (k, v))| {
                let mut entry = resp_lines(k);
                entry.extend(attach("=> ", resp_lines(v)));
                attach(&format!("{}# ", i + 1), entry)
            })
            .collect(),
    }
}

/// A displayed row of the output: a command or a line of its reply.
struct Row {
    text: String,
    kind: Option<RespLineKind>,
}

struct Cache {
    /// Identity of the output the rows were made from: its address and length.
    print: (usize, usize),
    rows: Vec<Row>,
}

/// One command the console offers while typing.
#[derive(Clone, Copy)]
struct Cmd {
    name: &'static str,
    /// Its arguments, as a usage hint; empty for a command without arguments.
    args: &'static str,
    summary: &'static str,
}

const fn cmd(name: &'static str, args: &'static str, summary: &'static str) -> Cmd {
    Cmd {
        name,
        args,
        summary,
    }
}

/// The commands completion knows, alphabetically. A command missing here still runs; it
/// only has no suggestion.
const COMMANDS: &[Cmd] = &[
    cmd("ACL", "subcommand [args…]", "Manage access control lists"),
    cmd("APPEND", "key value", "Append to a string"),
    cmd("ASKING", "", "Ask a cluster node to serve a key"),
    cmd(
        "AUTH",
        "[username] password",
        "Authenticate with the server",
    ),
    cmd("BGREWRITEAOF", "", "Rewrite the append-only file"),
    cmd("BGSAVE", "[SCHEDULE]", "Save the dataset in the background"),
    cmd("BITCOUNT", "key [start end [BYTE|BIT]]", "Count set bits"),
    cmd(
        "BITFIELD",
        "key [GET|SET|INCRBY type offset …]",
        "Operate on bitfields",
    ),
    cmd(
        "BITOP",
        "AND|OR|XOR|NOT dest key [key …]",
        "Combine strings bitwise",
    ),
    cmd(
        "BITPOS",
        "key bit [start [end [BYTE|BIT]]]",
        "Find the first bit",
    ),
    cmd(
        "BLMOVE",
        "src dst LEFT|RIGHT LEFT|RIGHT timeout",
        "Move between lists, blocking",
    ),
    cmd(
        "BLMPOP",
        "timeout numkeys key … LEFT|RIGHT",
        "Pop from a list, blocking",
    ),
    cmd(
        "BLPOP",
        "key [key …] timeout",
        "Pop from a list head, blocking",
    ),
    cmd(
        "BRPOP",
        "key [key …] timeout",
        "Pop from a list tail, blocking",
    ),
    cmd(
        "BRPOPLPUSH",
        "src dst timeout",
        "Move between lists, blocking",
    ),
    cmd(
        "BZMPOP",
        "timeout numkeys key … MIN|MAX",
        "Pop from a sorted set, blocking",
    ),
    cmd(
        "BZPOPMAX",
        "key [key …] timeout",
        "Pop the highest member, blocking",
    ),
    cmd(
        "BZPOPMIN",
        "key [key …] timeout",
        "Pop the lowest member, blocking",
    ),
    cmd("CLIENT", "subcommand [args…]", "Manage client connections"),
    cmd("CLUSTER", "subcommand [args…]", "Manage a Redis Cluster"),
    cmd("COMMAND", "subcommand [args…]", "Inspect server commands"),
    cmd(
        "CONFIG",
        "subcommand [args…]",
        "Read or change configuration",
    ),
    cmd("COPY", "source destination [DB db] [REPLACE]", "Copy a key"),
    cmd("DBSIZE", "", "Count the keys"),
    cmd("DEBUG", "subcommand [args…]", "Debugging helpers"),
    cmd("DECR", "key", "Decrement by one"),
    cmd("DECRBY", "key decrement", "Decrement an integer"),
    cmd("DEL", "key [key …]", "Delete keys"),
    cmd("DISCARD", "", "Discard a MULTI block"),
    cmd("DUMP", "key", "Serialize a key"),
    cmd("ECHO", "message", "Echo a message"),
    cmd("EVAL", "script numkeys key … [arg …]", "Run a Lua script"),
    cmd(
        "EVAL_RO",
        "script numkeys key … [arg …]",
        "Run a read-only Lua script",
    ),
    cmd(
        "EVALSHA",
        "sha1 numkeys key … [arg …]",
        "Run a cached Lua script",
    ),
    cmd(
        "EVALSHA_RO",
        "sha1 numkeys key … [arg …]",
        "Run a cached read-only script",
    ),
    cmd("EXEC", "", "Run a MULTI block"),
    cmd("EXISTS", "key [key …]", "Count existing keys"),
    cmd("EXPIRE", "key seconds [NX|XX|GT|LT]", "Set a key's TTL"),
    cmd(
        "EXPIREAT",
        "key unix-time [NX|XX|GT|LT]",
        "Set an expiration time",
    ),
    cmd("EXPIRETIME", "key", "Get the expiration time"),
    cmd(
        "FAILOVER",
        "[TO host port] [FORCE] [TAKEOVER]",
        "Fail over a replica",
    ),
    cmd("FCALL", "function numkeys key … [arg …]", "Call a function"),
    cmd(
        "FCALL_RO",
        "function numkeys key … [arg …]",
        "Call a read-only function",
    ),
    cmd(
        "FLUSHALL",
        "[ASYNC|SYNC]",
        "Delete all keys in all databases",
    ),
    cmd(
        "FLUSHDB",
        "[ASYNC|SYNC]",
        "Delete all keys in this database",
    ),
    cmd("FUNCTION", "subcommand [args…]", "Manage functions"),
    cmd(
        "GEOADD",
        "key [NX|XX] [CH] lon lat member …",
        "Add geospatial members",
    ),
    cmd(
        "GEODIST",
        "key member1 member2 [M|KM|FT|MI]",
        "Distance between members",
    ),
    cmd("GEOHASH", "key member [member …]", "Geohash strings"),
    cmd("GEOPOS", "key member [member …]", "Coordinates of members"),
    cmd(
        "GEORADIUS",
        "key lon lat radius unit [options]",
        "Find members in a radius",
    ),
    cmd(
        "GEORADIUSBYMEMBER",
        "key member radius unit [options]",
        "Find members near a member",
    ),
    cmd(
        "GEOSEARCH",
        "key FROMMEMBER|FROMLONLAT … BYRADIUS|BYBOX …",
        "Search geospatial members",
    ),
    cmd(
        "GEOSEARCHSTORE",
        "destination source … [STOREDIST]",
        "Store a geospatial search",
    ),
    cmd("GET", "key", "Get the value of a key"),
    cmd("GETBIT", "key offset", "Get a bit"),
    cmd("GETDEL", "key", "Get a value and delete the key"),
    cmd(
        "GETEX",
        "key [EX|PX|EXAT|PXAT time|PERSIST]",
        "Get a value and set its TTL",
    ),
    cmd("GETRANGE", "key start end", "Get a substring"),
    cmd("GETSET", "key value", "Set a value, return the old one"),
    cmd("HDEL", "key field [field …]", "Delete hash fields"),
    cmd(
        "HELLO",
        "[protocol [AUTH user pass]]",
        "Handshake with the server",
    ),
    cmd("HEXISTS", "key field", "Check a hash field"),
    cmd("HGET", "key field", "Get a hash field"),
    cmd("HGETALL", "key", "Get all hash fields and values"),
    cmd("HINCRBY", "key field increment", "Increment a hash field"),
    cmd(
        "HINCRBYFLOAT",
        "key field increment",
        "Increment a hash field by a float",
    ),
    cmd("HKEYS", "key", "Get all hash fields"),
    cmd("HLEN", "key", "Count hash fields"),
    cmd("HMGET", "key field [field …]", "Get several hash fields"),
    cmd(
        "HMSET",
        "key field value [field value …]",
        "Set several hash fields",
    ),
    cmd(
        "HRANDFIELD",
        "key [count [WITHVALUES]]",
        "Get random hash fields",
    ),
    cmd(
        "HSCAN",
        "key cursor [MATCH pattern] [COUNT n]",
        "Scan hash fields",
    ),
    cmd("HSET", "key field value [field value …]", "Set hash fields"),
    cmd(
        "HSETNX",
        "key field value",
        "Set a field if it does not exist",
    ),
    cmd("HSTRLEN", "key field", "Length of a hash field"),
    cmd("HVALS", "key", "Get all hash values"),
    cmd("INCR", "key", "Increment by one"),
    cmd("INCRBY", "key increment", "Increment an integer"),
    cmd("INCRBYFLOAT", "key increment", "Increment by a float"),
    cmd("INFO", "[section …]", "Server information"),
    cmd("KEYS", "pattern", "List keys matching a pattern"),
    cmd("LASTSAVE", "", "Time of the last save"),
    cmd("LATENCY", "subcommand [args…]", "Latency diagnostics"),
    cmd("LINDEX", "key index", "Get a list element"),
    cmd(
        "LINSERT",
        "key BEFORE|AFTER pivot element",
        "Insert into a list",
    ),
    cmd("LLEN", "key", "Count list elements"),
    cmd(
        "LMOVE",
        "src dst LEFT|RIGHT LEFT|RIGHT",
        "Move a list element",
    ),
    cmd(
        "LMPOP",
        "numkeys key … LEFT|RIGHT [COUNT n]",
        "Pop list elements",
    ),
    cmd("LOLWUT", "[VERSION version]", "Show a Redis artwork"),
    cmd("LPOP", "key [count]", "Pop from the list head"),
    cmd(
        "LPOS",
        "key element [RANK r] [COUNT n]",
        "Find an element's index",
    ),
    cmd(
        "LPUSH",
        "key element [element …]",
        "Push onto the list head",
    ),
    cmd(
        "LPUSHX",
        "key element [element …]",
        "Push if the list exists",
    ),
    cmd("LRANGE", "key start stop", "Get a list range"),
    cmd("LREM", "key count element", "Remove list elements"),
    cmd("LSET", "key index element", "Set a list element"),
    cmd("LTRIM", "key start stop", "Trim a list"),
    cmd("MEMORY", "subcommand [args…]", "Memory diagnostics"),
    cmd("MGET", "key [key …]", "Get several values"),
    cmd(
        "MIGRATE",
        "host port key db timeout [options]",
        "Move a key to another server",
    ),
    cmd("MODULE", "subcommand [args…]", "Manage server modules"),
    cmd("MONITOR", "", "Stream server commands"),
    cmd("MOVE", "key db", "Move a key to another database"),
    cmd("MSET", "key value [key value …]", "Set several values"),
    cmd(
        "MSETNX",
        "key value [key value …]",
        "Set values if none exist",
    ),
    cmd("MULTI", "", "Start a transaction"),
    cmd("OBJECT", "subcommand key", "Inspect a key object"),
    cmd("PERSIST", "key", "Remove the TTL"),
    cmd(
        "PEXPIRE",
        "key milliseconds [NX|XX|GT|LT]",
        "Set a TTL in milliseconds",
    ),
    cmd(
        "PEXPIREAT",
        "key unix-time-ms [NX|XX|GT|LT]",
        "Set an expiration in ms",
    ),
    cmd("PEXPIRETIME", "key", "Get the expiration time in ms"),
    cmd("PFADD", "key [element …]", "Add to a HyperLogLog"),
    cmd("PFCOUNT", "key [key …]", "Count a HyperLogLog"),
    cmd(
        "PFMERGE",
        "destkey sourcekey [sourcekey …]",
        "Merge HyperLogLogs",
    ),
    cmd("PING", "[message]", "Check the connection"),
    cmd("PSETEX", "key milliseconds value", "Set a value and TTL"),
    cmd("PSUBSCRIBE", "pattern [pattern …]", "Subscribe to patterns"),
    cmd("PSYNC", "replicationid offset", "Start replication"),
    cmd("PTTL", "key", "Get the TTL in milliseconds"),
    cmd("PUBLISH", "channel message", "Publish a message"),
    cmd("PUBSUB", "subcommand [args…]", "Inspect pub/sub state"),
    cmd("PUNSUBSCRIBE", "[pattern …]", "Unsubscribe from patterns"),
    cmd("QUIT", "", "Close the connection"),
    cmd("RANDOMKEY", "", "Get a random key"),
    cmd("READONLY", "", "Read from a cluster replica"),
    cmd("READWRITE", "", "Read and write in a cluster"),
    cmd("RENAME", "key newkey", "Rename a key"),
    cmd("RENAMENX", "key newkey", "Rename if the new key is free"),
    cmd(
        "REPLICAOF",
        "host port|NO ONE",
        "Change the replication source",
    ),
    cmd("RESET", "", "Reset the connection"),
    cmd(
        "RESTORE",
        "key ttl payload [options]",
        "Restore a serialized key",
    ),
    cmd("ROLE", "", "Replication role"),
    cmd("RPOP", "key [count]", "Pop from the list tail"),
    cmd("RPOPLPUSH", "src dst", "Move a list element"),
    cmd(
        "RPUSH",
        "key element [element …]",
        "Push onto the list tail",
    ),
    cmd(
        "RPUSHX",
        "key element [element …]",
        "Push if the list exists",
    ),
    cmd("SADD", "key member [member …]", "Add set members"),
    cmd("SAVE", "", "Save the dataset"),
    cmd(
        "SCAN",
        "cursor [MATCH pattern] [COUNT n] [TYPE type]",
        "Scan keys",
    ),
    cmd("SCARD", "key", "Count set members"),
    cmd("SCRIPT", "subcommand [args…]", "Manage Lua scripts"),
    cmd("SDIFF", "key [key …]", "Set difference"),
    cmd(
        "SDIFFSTORE",
        "destination key [key …]",
        "Store a set difference",
    ),
    cmd("SELECT", "index", "Switch database"),
    cmd(
        "SET",
        "key value [EX|PX time] [NX|XX] [KEEPTTL] [GET]",
        "Set a value",
    ),
    cmd("SETBIT", "key offset value", "Set a bit"),
    cmd("SETEX", "key seconds value", "Set a value and TTL"),
    cmd("SETNX", "key value", "Set a value if the key is free"),
    cmd("SETRANGE", "key offset value", "Overwrite part of a string"),
    cmd(
        "SHUTDOWN",
        "[NOSAVE|SAVE] [NOW] [FORCE] [ABORT]",
        "Stop the server",
    ),
    cmd("SINTER", "key [key …]", "Set intersection"),
    cmd(
        "SINTERCARD",
        "numkeys key … [LIMIT n]",
        "Count a set intersection",
    ),
    cmd(
        "SINTERSTORE",
        "destination key [key …]",
        "Store a set intersection",
    ),
    cmd("SISMEMBER", "key member", "Check a set member"),
    cmd(
        "SLAVEOF",
        "host port|NO ONE",
        "Change the replication source",
    ),
    cmd("SLOWLOG", "subcommand [args…]", "Slow query log"),
    cmd("SMEMBERS", "key", "Get all set members"),
    cmd(
        "SMISMEMBER",
        "key member [member …]",
        "Check several set members",
    ),
    cmd("SMOVE", "source destination member", "Move a set member"),
    cmd(
        "SORT",
        "key [BY pattern] [LIMIT o c] [GET p] [ASC|DESC] [ALPHA] [STORE d]",
        "Sort a list, set or zset",
    ),
    cmd("SORT_RO", "key [options]", "Read-only SORT"),
    cmd("SPOP", "key [count]", "Remove random set members"),
    cmd("SPUBLISH", "shardchannel message", "Publish to a shard"),
    cmd("SRANDMEMBER", "key [count]", "Get random set members"),
    cmd("SREM", "key member [member …]", "Remove set members"),
    cmd(
        "SSCAN",
        "key cursor [MATCH pattern] [COUNT n]",
        "Scan set members",
    ),
    cmd(
        "SSUBSCRIBE",
        "shardchannel [shardchannel …]",
        "Subscribe to shards",
    ),
    cmd("STRLEN", "key", "Length of a string"),
    cmd("SUBSCRIBE", "channel [channel …]", "Subscribe to channels"),
    cmd("SUBSTR", "key start end", "Get a substring"),
    cmd("SUNION", "key [key …]", "Set union"),
    cmd(
        "SUNIONSTORE",
        "destination key [key …]",
        "Store a set union",
    ),
    cmd("SUNSUBSCRIBE", "[channel …]", "Unsubscribe from shards"),
    cmd("SWAPDB", "index1 index2", "Swap two databases"),
    cmd("SYNC", "", "Start replication"),
    cmd("TIME", "", "Server time"),
    cmd("TOUCH", "key [key …]", "Update access time"),
    cmd("TTL", "key", "Get the TTL"),
    cmd("TYPE", "key", "Key type"),
    cmd("UNLINK", "key [key …]", "Delete keys asynchronously"),
    cmd("UNSUBSCRIBE", "[channel …]", "Unsubscribe from channels"),
    cmd("UNWATCH", "", "Forget watched keys"),
    cmd(
        "WAIT",
        "numreplicas timeout",
        "Wait for replica acknowledgment",
    ),
    cmd("WAITAOF", "numlocal numreplicas timeout", "Wait for fsync"),
    cmd("WATCH", "key [key …]", "Watch keys for a transaction"),
    cmd("XACK", "key group id [id …]", "Acknowledge stream entries"),
    cmd(
        "XADD",
        "key [NOMKSTREAM] *|id field value …",
        "Add a stream entry",
    ),
    cmd(
        "XAUTOCLAIM",
        "key group consumer min-idle-time start [COUNT n]",
        "Claim pending entries",
    ),
    cmd(
        "XCLAIM",
        "key group consumer min-idle-time id …",
        "Claim a pending entry",
    ),
    cmd("XDEL", "key id [id …]", "Delete stream entries"),
    cmd("XGROUP", "subcommand [args…]", "Manage consumer groups"),
    cmd("XINFO", "subcommand [args…]", "Stream information"),
    cmd("XLEN", "key", "Count stream entries"),
    cmd(
        "XPENDING",
        "key group [[IDLE ms] start end count [consumer]]",
        "Pending entries",
    ),
    cmd("XRANGE", "key start end [COUNT n]", "Stream range"),
    cmd(
        "XREAD",
        "[COUNT n] [BLOCK ms] STREAMS key … id …",
        "Read stream entries",
    ),
    cmd(
        "XREADGROUP",
        "GROUP group consumer [COUNT n] [BLOCK ms] STREAMS key … id …",
        "Read as a consumer group",
    ),
    cmd(
        "XREVRANGE",
        "key end start [COUNT n]",
        "Stream range, newest first",
    ),
    cmd(
        "XSETID",
        "key id [ENTRIESADDED n] [MAXDELETEDID id]",
        "Set the last stream id",
    ),
    cmd(
        "XTRIM",
        "key MAXLEN|MINID [~|=] threshold [LIMIT n]",
        "Trim a stream",
    ),
    cmd(
        "ZADD",
        "key [NX|XX] [GT|LT] [CH] [INCR] score member …",
        "Add sorted-set members",
    ),
    cmd("ZCARD", "key", "Count sorted-set members"),
    cmd("ZCOUNT", "key min max", "Count members in a score range"),
    cmd(
        "ZDIFF",
        "numkeys key … [WITHSCORES]",
        "Sorted-set difference",
    ),
    cmd(
        "ZDIFFSTORE",
        "destination numkeys key …",
        "Store a sorted-set difference",
    ),
    cmd(
        "ZINCRBY",
        "key increment member",
        "Increment a member's score",
    ),
    cmd(
        "ZINTER",
        "numkeys key … [WEIGHTS …] [AGGREGATE …]",
        "Sorted-set intersection",
    ),
    cmd(
        "ZINTERCARD",
        "numkeys key … [LIMIT n]",
        "Count a sorted-set intersection",
    ),
    cmd(
        "ZINTERSTORE",
        "destination numkeys key …",
        "Store a sorted-set intersection",
    ),
    cmd("ZLEXCOUNT", "key min max", "Count members in a lex range"),
    cmd(
        "ZMPOP",
        "numkeys key … MIN|MAX [COUNT n]",
        "Pop sorted-set members",
    ),
    cmd("ZMSCORE", "key member [member …]", "Get member scores"),
    cmd("ZPOPMAX", "key [count]", "Pop the highest members"),
    cmd("ZPOPMIN", "key [count]", "Pop the lowest members"),
    cmd(
        "ZRANDMEMBER",
        "key [count [WITHSCORES]]",
        "Get random members",
    ),
    cmd(
        "ZRANGE",
        "key start stop [BYSCORE|BYLEX] [REV] [LIMIT o c] [WITHSCORES]",
        "Get a sorted-set range",
    ),
    cmd("ZRANGEBYLEX", "key min max [LIMIT o c]", "Get a lex range"),
    cmd(
        "ZRANGEBYSCORE",
        "key min max [WITHSCORES] [LIMIT o c]",
        "Get a score range",
    ),
    cmd(
        "ZRANGESTORE",
        "dst src min max [BYSCORE|BYLEX] [REV] [LIMIT o c]",
        "Store a sorted-set range",
    ),
    cmd("ZRANK", "key member [WITHSCORE]", "Get a member's rank"),
    cmd("ZREM", "key member [member …]", "Remove sorted-set members"),
    cmd("ZREMRANGEBYLEX", "key min max", "Remove a lex range"),
    cmd("ZREMRANGEBYRANK", "key start stop", "Remove a rank range"),
    cmd("ZREMRANGEBYSCORE", "key min max", "Remove a score range"),
    cmd(
        "ZREVRANGE",
        "key start stop [WITHSCORES]",
        "Get a range, reversed",
    ),
    cmd(
        "ZREVRANGEBYLEX",
        "key max min [LIMIT o c]",
        "Get a lex range, reversed",
    ),
    cmd(
        "ZREVRANGEBYSCORE",
        "key max min [WITHSCORES] [LIMIT o c]",
        "Get a score range, reversed",
    ),
    cmd(
        "ZREVRANK",
        "key member [WITHSCORE]",
        "Get a member's rank, reversed",
    ),
    cmd(
        "ZSCAN",
        "key cursor [MATCH pattern] [COUNT n]",
        "Scan sorted-set members",
    ),
    cmd("ZSCORE", "key member", "Get a member's score"),
    cmd(
        "ZUNION",
        "numkeys key … [WEIGHTS …] [AGGREGATE …]",
        "Sorted-set union",
    ),
    cmd(
        "ZUNIONSTORE",
        "destination numkeys key …",
        "Store a sorted-set union",
    ),
];

const ACL_SUBCOMMANDS: &[Cmd] = &[
    cmd("CAT", "[category]", "List command categories"),
    cmd("DELUSER", "username [username …]", "Delete users"),
    cmd(
        "DRYRUN",
        "username command [arg …]",
        "Check a user's permissions",
    ),
    cmd("GENPASS", "[bits]", "Generate a random password"),
    cmd("GETUSER", "username", "Show a user"),
    cmd("LIST", "", "List users"),
    cmd("LOAD", "", "Reload users from the ACL file"),
    cmd("LOG", "[count|RESET]", "Show the ACL log"),
    cmd("SAVE", "", "Save users to the ACL file"),
    cmd("SETUSER", "username [rule …]", "Create or change a user"),
    cmd("USERS", "", "List user names"),
    cmd("WHOAMI", "", "Show the current user"),
];

const CLIENT_SUBCOMMANDS: &[Cmd] = &[
    cmd("CACHING", "YES|NO", "Control client-side caching"),
    cmd("GETNAME", "", "Get the connection name"),
    cmd("GETREDIR", "", "Get the tracking redirect id"),
    cmd("ID", "", "Get the connection id"),
    cmd("INFO", "", "Connection information"),
    cmd(
        "KILL",
        "[ID id] [TYPE type] [USER user] [ADDR addr]",
        "Close connections",
    ),
    cmd("LIST", "[TYPE type] [ID id …]", "List connections"),
    cmd("NO-EVICT", "ON|OFF", "Control client eviction"),
    cmd("NO-TOUCH", "ON|OFF", "Control LRU/LFU updates"),
    cmd("PAUSE", "timeout [WRITE|ALL]", "Pause processing"),
    cmd("REPLY", "ON|OFF|SKIP", "Control command replies"),
    cmd("SETINFO", "attr value", "Set client information"),
    cmd("SETNAME", "connection-name", "Name the connection"),
    cmd(
        "TRACKING",
        "ON|OFF [options]",
        "Control server-assisted caching",
    ),
    cmd("TRACKINGINFO", "", "Tracking information"),
    cmd("UNPAUSE", "", "Resume processing"),
    cmd("UNBLOCK", "client-id [TIMEOUT|ERROR]", "Unblock a client"),
];

const CLUSTER_SUBCOMMANDS: &[Cmd] = &[
    cmd("ADDSLOTS", "slot [slot …]", "Assign hash slots"),
    cmd(
        "ADDSLOTSRANGE",
        "start slot [start slot …]",
        "Assign hash slot ranges",
    ),
    cmd("BUMPEPOCH", "", "Advance the config epoch"),
    cmd("COUNTKEYSINSLOT", "slot", "Count keys in a slot"),
    cmd("DELSLOTS", "slot [slot …]", "Remove hash slot assignments"),
    cmd(
        "DELSLOTSRANGE",
        "start slot [start slot …]",
        "Remove hash slot ranges",
    ),
    cmd(
        "FAILOVER",
        "[TO host port] [FORCE] [TAKEOVER]",
        "Fail over the node",
    ),
    cmd("FORGET", "node-id", "Remove a node"),
    cmd("GETKEYSINSLOT", "slot count", "List keys in a slot"),
    cmd("INFO", "", "Cluster state"),
    cmd("KEYSLOT", "key", "Get a key's hash slot"),
    cmd("LINKS", "", "Node links"),
    cmd("MEET", "ip port [cluster-bus-port]", "Add a node"),
    cmd("MYID", "", "This node's id"),
    cmd("MYSHARDID", "", "This node's shard id"),
    cmd("NODES", "", "Known nodes"),
    cmd("REPLICATE", "node-id", "Make this node a replica"),
    cmd("RESET", "[HARD|SOFT]", "Reset the node"),
    cmd("SAVECONFIG", "", "Save the cluster configuration"),
    cmd("SET-CONFIG-EPOCH", "config-epoch", "Set the config epoch"),
    cmd(
        "SETSLOT",
        "slot IMPORTING|MIGRATING|STABLE|NODE node-id",
        "Change a slot's state",
    ),
    cmd("SHARDS", "", "Cluster shards"),
    cmd("SLOTS", "", "Slot assignments"),
];

const COMMAND_SUBCOMMANDS: &[Cmd] = &[
    cmd("COUNT", "", "Count commands"),
    cmd("DOCS", "[command-name …]", "Command documentation"),
    cmd("GETKEYS", "command [arg …]", "Extract keys from a command"),
    cmd(
        "GETKEYSANDFLAGS",
        "command [arg …]",
        "Extract keys and flags",
    ),
    cmd("INFO", "[command-name …]", "Command metadata"),
    cmd(
        "LIST",
        "[FILTERBY MODULE|ACLCAT|PATTERN value]",
        "List commands",
    ),
];

const CONFIG_SUBCOMMANDS: &[Cmd] = &[
    cmd("GET", "parameter [parameter …]", "Read configuration"),
    cmd("RESETSTAT", "", "Reset statistics"),
    cmd("REWRITE", "", "Rewrite redis.conf"),
    cmd(
        "SET",
        "parameter value [parameter value …]",
        "Change configuration",
    ),
];

const FUNCTION_SUBCOMMANDS: &[Cmd] = &[
    cmd("DELETE", "library-name", "Delete a library"),
    cmd("DUMP", "", "Dump all libraries"),
    cmd("FLUSH", "[ASYNC|SYNC]", "Delete all libraries"),
    cmd("KILL", "", "Stop a running function"),
    cmd("LIST", "[LIBRARYNAME name] [WITHCODE]", "List libraries"),
    cmd("LOAD", "[REPLACE] function-code", "Load a library"),
    cmd(
        "RESTORE",
        "serialized-value [FLUSH|APPEND|REPLACE]",
        "Restore libraries",
    ),
    cmd("STATS", "", "Function statistics"),
];

const LATENCY_SUBCOMMANDS: &[Cmd] = &[
    cmd("DOCTOR", "", "Latency advice"),
    cmd("GRAPH", "event", "Latency graph"),
    cmd("HISTORY", "event", "Latency samples"),
    cmd("LATEST", "", "Latest latency events"),
    cmd("RESET", "[event …]", "Reset latency samples"),
];

const MEMORY_SUBCOMMANDS: &[Cmd] = &[
    cmd("DOCTOR", "", "Memory advice"),
    cmd("MALLOC-STATS", "", "Allocator statistics"),
    cmd("PURGE", "", "Ask the allocator to release memory"),
    cmd("STATS", "", "Memory statistics"),
    cmd("USAGE", "key [SAMPLES count]", "Memory used by a key"),
];

const MODULE_SUBCOMMANDS: &[Cmd] = &[
    cmd("LIST", "", "List modules"),
    cmd("LOAD", "path [arg …]", "Load a module"),
    cmd(
        "LOADEX",
        "path [CONFIG name value …] [ARGS args …]",
        "Load a module with options",
    ),
    cmd("UNLOAD", "name", "Unload a module"),
];

const OBJECT_SUBCOMMANDS: &[Cmd] = &[
    cmd("ENCODING", "key", "Internal encoding"),
    cmd("FREQ", "key", "Access frequency"),
    cmd("IDLETIME", "key", "Idle time"),
    cmd("REFCOUNT", "key", "Reference count"),
];

const PUBSUB_SUBCOMMANDS: &[Cmd] = &[
    cmd("CHANNELS", "[pattern]", "List channels"),
    cmd("NUMPAT", "", "Count pattern subscriptions"),
    cmd("NUMSUB", "[channel …]", "Count channel subscribers"),
    cmd("SHARDCHANNELS", "[pattern]", "List shard channels"),
    cmd("SHARDNUMSUB", "[shardchannel …]", "Count shard subscribers"),
];

const SCRIPT_SUBCOMMANDS: &[Cmd] = &[
    cmd("EXISTS", "sha1 [sha1 …]", "Check cached scripts"),
    cmd("FLUSH", "[ASYNC|SYNC]", "Clear the script cache"),
    cmd("KILL", "", "Stop a running script"),
    cmd("LOAD", "script", "Cache a script"),
];

const SLOWLOG_SUBCOMMANDS: &[Cmd] = &[
    cmd("GET", "[count]", "Read the slow log"),
    cmd("LEN", "", "Slow log length"),
    cmd("RESET", "", "Clear the slow log"),
];

const XGROUP_SUBCOMMANDS: &[Cmd] = &[
    cmd(
        "CREATE",
        "key group id|$ [MKSTREAM]",
        "Create a consumer group",
    ),
    cmd("CREATECONSUMER", "key group consumer", "Create a consumer"),
    cmd("DELCONSUMER", "key group consumer", "Delete a consumer"),
    cmd("DESTROY", "key group", "Delete a consumer group"),
    cmd("SETID", "key group id|$", "Set the last delivered id"),
];

const XINFO_SUBCOMMANDS: &[Cmd] = &[
    cmd("CONSUMERS", "key group", "Consumers of a group"),
    cmd("GROUPS", "key", "Consumer groups"),
    cmd("STREAM", "key [FULL [COUNT n]]", "Stream information"),
];

fn subcommands(command: &str) -> &'static [Cmd] {
    match command {
        "ACL" => ACL_SUBCOMMANDS,
        "CLIENT" => CLIENT_SUBCOMMANDS,
        "CLUSTER" => CLUSTER_SUBCOMMANDS,
        "COMMAND" => COMMAND_SUBCOMMANDS,
        "CONFIG" => CONFIG_SUBCOMMANDS,
        "FUNCTION" => FUNCTION_SUBCOMMANDS,
        "LATENCY" => LATENCY_SUBCOMMANDS,
        "MEMORY" => MEMORY_SUBCOMMANDS,
        "MODULE" => MODULE_SUBCOMMANDS,
        "OBJECT" => OBJECT_SUBCOMMANDS,
        "PUBSUB" => PUBSUB_SUBCOMMANDS,
        "SCRIPT" => SCRIPT_SUBCOMMANDS,
        "SLOWLOG" => SLOWLOG_SUBCOMMANDS,
        "XGROUP" => XGROUP_SUBCOMMANDS,
        "XINFO" => XINFO_SUBCOMMANDS,
        _ => &[],
    }
}

/// The suggestions of `list` whose name starts with `prefix` (ASCII case-insensitive).
fn matching(list: &'static [Cmd], prefix: &str) -> Vec<&'static Cmd> {
    if prefix.is_empty() {
        return list.iter().collect();
    }
    let lower = prefix.to_ascii_lowercase();
    list.iter()
        .filter(|c| c.name.to_ascii_lowercase().starts_with(&lower))
        .collect()
}

/// The word being completed and the command whose subcommands are completed, if any.
struct Context {
    word: Range<usize>,
    /// `None` for the command name, `Some(command)` for its subcommands.
    command: Option<&'static str>,
    prefix: String,
}

/// The word (in char indices) around `caret`; empty at either end of the line.
fn word_at(input: &str, caret: usize) -> (usize, usize) {
    let chars: Vec<char> = input.chars().collect();
    let caret = caret.min(chars.len());
    let start = chars[..caret]
        .iter()
        .rposition(|c| c.is_whitespace())
        .map_or(0, |i| i + 1);
    let end = chars[caret..]
        .iter()
        .position(|c| c.is_whitespace())
        .map_or(chars.len(), |i| caret + i);
    (start, end)
}

/// Byte offset of char `index` in `text`.
fn char_byte(text: &str, index: usize) -> usize {
    text.char_indices()
        .nth(index)
        .map_or(text.len(), |(byte, _)| byte)
}

/// What the caret is completing; `None` in an argument or after an unknown command.
fn completion_context(input: &str, caret: usize) -> Option<Context> {
    let (start, end) = word_at(input, caret);
    let chars: Vec<char> = input.chars().collect();
    let before: String = chars[..start].iter().collect();
    let mut words = before.split_whitespace();
    let command = match (words.next(), words.next()) {
        (None, _) => None,
        (Some(first), None) => {
            let name = COMMANDS
                .iter()
                .find(|c| c.name.eq_ignore_ascii_case(first))?
                .name;
            (!subcommands(name).is_empty()).then_some(name)
        }
        // A third word is an argument, not a subcommand.
        _ => return None,
    };
    let prefix = chars[start..caret.min(chars.len())].iter().collect();
    Some(Context {
        word: start..end,
        command,
        prefix,
    })
}

/// The command completion popup: its items, selection and what to redraw.
#[derive(Default)]
struct Completion {
    open: bool,
    /// The user opened it with Ctrl+Space: it stays while the prefix is empty.
    explicit: bool,
    selected: usize,
    scroll_to_selected: bool,
    items: Vec<&'static Cmd>,
    word: Range<usize>,
    command: Option<&'static str>,
    prefix: String,
    /// Where it was drawn in the last frame, to keep the input focused while it is clicked.
    rect: Option<Rect>,
}

impl Completion {
    fn close(&mut self) {
        self.open = false;
        self.explicit = false;
        self.selected = 0;
        self.scroll_to_selected = false;
        self.items.clear();
        self.rect = None;
    }

    /// Recomputes the items for the word at `caret`; opens on a prefix, or on `explicit`.
    fn refresh(&mut self, input: &str, caret: usize, explicit: bool) {
        let Some(ctx) = completion_context(input, caret) else {
            self.close();
            return;
        };
        let items = match ctx.command {
            Some(command) => matching(subcommands(command), &ctx.prefix),
            None => matching(COMMANDS, &ctx.prefix),
        };
        if items.is_empty() || (!explicit && ctx.prefix.is_empty()) {
            self.close();
            return;
        }
        let same =
            self.prefix == ctx.prefix && self.command == ctx.command && self.word == ctx.word;
        if !same {
            self.selected = 0;
            self.scroll_to_selected = true;
        }
        self.open = true;
        self.explicit = explicit;
        self.items = items;
        self.word = ctx.word;
        self.command = ctx.command;
        self.prefix = ctx.prefix;
    }

    fn move_selection(&mut self, delta: isize) {
        if self.items.is_empty() {
            return;
        }
        let last = self.items.len() - 1;
        self.selected = self.selected.saturating_add_signed(delta).min(last);
        self.scroll_to_selected = true;
    }

    /// Whether Enter should run the line instead of completing it: the selected name is
    /// already exactly what is typed.
    fn exact(&self) -> bool {
        self.items
            .get(self.selected)
            .is_some_and(|item| item.name.eq_ignore_ascii_case(&self.prefix))
    }

    /// Replaces the word being completed with the selected name and a separating space;
    /// returns the new caret (in chars).
    fn accept(&mut self, input: &mut String) -> Option<usize> {
        let item = *self.items.get(self.selected)?;
        let start = char_byte(input, self.word.start);
        let end = char_byte(input, self.word.end);
        let after = &input[end..];
        let space = !after.chars().next().is_some_and(char::is_whitespace);
        let mut text = String::with_capacity(input.len() + item.name.len() + 1);
        text.push_str(&input[..start]);
        text.push_str(item.name);
        if space {
            text.push(' ');
        }
        text.push_str(after);
        *input = text;
        let caret = self.word.start + item.name.chars().count() + usize::from(space);
        self.close();
        Some(caret)
    }

    fn contains_pointer(&self, ctx: &egui::Context) -> bool {
        self.rect.is_some_and(|rect| {
            ctx.input(|i| {
                i.pointer
                    .interact_pos()
                    .is_some_and(|pos| rect.contains(pos))
            })
        })
    }
}

pub struct ConsoleState {
    /// The command line being typed.
    pub input: String,
    /// Lines entered before, oldest first (the app sets it from the engine each frame).
    pub history: Arc<Vec<String>>,
    /// The history entry shown in the input, while the user navigates.
    pos: Option<usize>,
    /// What was typed before navigating.
    draft: String,
    /// The text the navigation put in the input: typing something else ends it.
    shown: Option<String>,
    focus: bool,
    cache: Option<Cache>,
    completion: Completion,
}

impl Default for ConsoleState {
    fn default() -> Self {
        Self {
            input: String::new(),
            history: Arc::default(),
            pos: None,
            draft: String::new(),
            shown: None,
            focus: true,
            cache: None,
            completion: Completion::default(),
        }
    }
}

impl ConsoleState {
    /// ↑: the previous history entry (the oldest stays).
    pub fn history_up(&mut self) {
        let len = self.history.len();
        if len == 0 {
            return;
        }
        let pos = match self.pos {
            None => {
                self.draft = self.input.clone();
                len - 1
            }
            Some(i) => i.saturating_sub(1).min(len - 1),
        };
        self.show(pos);
    }

    /// ↓: the next history entry, then what was typed before.
    pub fn history_down(&mut self) {
        let Some(i) = self.pos else {
            return;
        };
        if i + 1 < self.history.len() {
            self.show(i + 1);
        } else {
            self.pos = None;
            self.input = std::mem::take(&mut self.draft);
            self.shown = None;
        }
    }

    fn show(&mut self, pos: usize) {
        self.pos = Some(pos);
        self.input = self.history[pos].clone();
        self.shown = Some(self.input.clone());
    }

    /// Asks the input for the caret on the next frame.
    pub fn request_focus(&mut self) {
        self.focus = true;
    }
}

fn kind_color(kind: Option<RespLineKind>, p: &Palette) -> egui::Color32 {
    match kind {
        None => p.text_muted,
        Some(RespLineKind::Plain) => p.text,
        Some(RespLineKind::Number) => p.icon_table,
        Some(RespLineKind::Nil) => p.null_text,
        Some(RespLineKind::Error) => p.error,
        Some(RespLineKind::Status) => p.success,
    }
}

fn rows_of(output: &[Arc<(String, RespValue)>]) -> Vec<Row> {
    let mut rows = Vec::new();
    for entry in output {
        // A blank line between commands keeps long replies readable.
        if !rows.is_empty() {
            rows.push(Row {
                text: String::new(),
                kind: None,
            });
        }
        let (command, reply) = &**entry;
        rows.push(Row {
            text: format!("> {command}"),
            kind: None,
        });
        rows.extend(resp_lines(reply).into_iter().map(|l| Row {
            text: l.text,
            kind: Some(l.kind),
        }));
    }
    rows
}

/// The caret position of the console input, in chars (the end when it never had focus).
fn caret_chars(ctx: &egui::Context, id: Id, input: &str) -> usize {
    let max = input.chars().count();
    egui::text_edit::TextEditState::load(ctx, id)
        .and_then(|state| state.cursor.char_range())
        .map_or(max, |range| range.primary.index.0.min(max))
}

fn set_caret(ctx: &egui::Context, id: Id, caret: usize) {
    if let Some(mut state) = egui::text_edit::TextEditState::load(ctx, id) {
        state
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::one(
                egui::text::CCursor::new(caret),
            )));
        state.store(ctx, id);
    }
}

/// Pads or cuts `text` to `width` chars for a popup column.
fn fit(text: &str, width: usize) -> String {
    let count = text.chars().count();
    if count <= width {
        return format!("{text:<width$}");
    }
    let mut cut: String = text.chars().take(width.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

/// Draws the completion popup above `input_rect`; returns the clicked row.
fn completion_popup(
    ui: &mut egui::Ui,
    state: &mut Completion,
    id: Id,
    input_rect: Rect,
) -> Option<usize> {
    if !state.open || state.items.is_empty() {
        return None;
    }
    let p = theme::palette(ui.ctx());
    let width = input_rect.width().clamp(320.0, 680.0);
    let row_height = ui.spacing().interact_size.y;
    let items = state.items.clone();
    let selected = state.selected;
    let mut clicked = None;
    let shadow = ui.style().visuals.popup_shadow;
    let area = Area::new(id.with("completion"))
        .order(Order::Foreground)
        .fixed_pos(input_rect.left_top() - egui::vec2(0.0, Spacing::S))
        .pivot(Align2::LEFT_BOTTOM)
        .show(ui.ctx(), |ui| {
            Frame::new()
                .fill(p.bg_raised)
                .stroke(Stroke::new(1.0, p.border_input))
                .corner_radius(CornerRadius::same(Spacing::RADIUS_M))
                .shadow(shadow)
                .inner_margin(Margin::same(4))
                .show(ui, |ui| {
                    ui.set_width((width - 8.0).max(280.0));
                    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
                    ScrollArea::vertical()
                        .id_salt(id.with("completion_rows"))
                        .max_height(row_height * 8.0)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            for (i, item) in items.iter().enumerate() {
                                let text = (
                                    RichText::new(fit(item.name, 20))
                                        .monospace()
                                        .strong()
                                        .color(p.text),
                                    RichText::new(fit(item.args, 30))
                                        .monospace()
                                        .color(p.text_muted),
                                    RichText::new(item.summary).small().color(p.text_muted),
                                );
                                let response = ui.add(
                                    Button::selectable(i == selected, text)
                                        .min_size(egui::vec2(ui.available_width(), row_height)),
                                );
                                if i == selected && state.scroll_to_selected {
                                    ui.scroll_to_rect(response.rect, Some(Align::Center));
                                }
                                if response.clicked() {
                                    clicked = Some(i);
                                }
                            }
                        });
                    ui.separator();
                    theme::hint(ui, "↑ ↓ select · Tab complete · Esc close");
                });
        });
    state.rect = Some(area.response.rect);
    state.scroll_to_selected = false;
    clicked
}

/// Draws the output above the command line. Returns the line when the user presses Enter on
/// a non-empty one (the input is cleared).
pub fn console(
    ui: &mut egui::Ui,
    state: &mut ConsoleState,
    output: &[Arc<(String, RespValue)>],
) -> Option<String> {
    let p = theme::palette(ui.ctx());
    // Typing something other than the navigated entry ends the navigation.
    if state.pos.is_some() && state.shown.as_deref() != Some(state.input.as_str()) {
        state.pos = None;
        state.shown = None;
    }

    let print = (output.as_ptr() as usize, output.len());
    if state.cache.as_ref().is_none_or(|c| c.print != print) {
        state.cache = Some(Cache {
            print,
            rows: rows_of(output),
        });
    }
    let input_height = ui.text_style_height(&egui::TextStyle::Monospace) + 18.0;
    let height = (ui.available_height() - input_height - Spacing::S).max(40.0);
    let rows = state.cache.as_ref().map_or(&[][..], |c| &c.rows[..]);
    if rows.is_empty() {
        ui.allocate_ui(egui::vec2(ui.available_width(), height), |ui| {
            let top = (height / 3.0).max(0.0);
            ui.vertical_centered(|ui| {
                ui.add_space(top);
                ui.label(
                    RichText::new(">_")
                        .monospace()
                        .size(22.0)
                        .color(p.text_disabled),
                );
                ui.add_space(Spacing::S);
                ui.label(RichText::new("Redis console").strong());
                ui.add_space(Spacing::XS);
                theme::hint(ui, "Type a command, for example PING or GET key.");
                theme::hint(ui, "↑ ↓ history · Tab complete · Enter run");
            });
        });
    } else {
        let row_height = ui.text_style_height(&egui::TextStyle::Monospace);
        ScrollArea::vertical()
            .id_salt("kv_console_output")
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .max_height(height)
            .show_rows(ui, row_height, rows.len(), |ui, range| {
                for i in range {
                    let row = &rows[i];
                    ui.label(
                        RichText::new(&row.text)
                            .monospace()
                            .color(kind_color(row.kind, &p)),
                    );
                }
            });
    }
    ui.add_space(Spacing::S);
    let mut submitted = None;
    ui.horizontal(|ui| {
        let id = ui.make_persistent_id("kv_console_input");
        let focused = ui.memory(|m| m.has_focus(id));
        let caret = caret_chars(ui.ctx(), id, &state.input);
        // egui drops keyboard focus on Escape before any widget runs, so close the popup and
        // ask for the input back after it is drawn.
        let escape = (state.completion.open || focused)
            && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape));
        let refocus = escape && state.completion.open;
        if refocus {
            state.completion.close();
        }
        if focused {
            if state.completion.open {
                if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowUp)) {
                    state.completion.move_selection(-1);
                }
                if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowDown)) {
                    state.completion.move_selection(1);
                }
                if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Tab))
                    && let Some(caret) = state.completion.accept(&mut state.input)
                {
                    set_caret(ui.ctx(), id, caret);
                }
            } else {
                if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowUp)) {
                    state.history_up();
                }
                if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowDown)) {
                    state.history_down();
                }
            }
            let keymap = crate::keymap::ui_keymap(ui.ctx());
            if ui.input_mut(|i| keymap.consume(i, sqlr_core::ActionId::Complete)) {
                if state.completion.open {
                    state.completion.close();
                } else {
                    state.completion.refresh(&state.input, caret, true);
                }
            }
        }
        ui.label(RichText::new(">").monospace().strong().color(p.accent));
        let stroke = if focused {
            Stroke::new(1.0, p.accent)
        } else {
            Stroke::new(1.0, p.border_input)
        };
        let frame = Frame::new()
            .fill(p.bg_input)
            .stroke(stroke)
            .corner_radius(CornerRadius::same(Spacing::RADIUS_M))
            .inner_margin(Margin::symmetric(10, 8));
        let response = crate::text_menu::text_edit(
            ui,
            TextEdit::singleline(&mut state.input)
                .id(id)
                .font(egui::TextStyle::Monospace)
                .hint_text("Type a command…")
                .lock_focus(true)
                .frame(frame)
                .margin(Margin::ZERO)
                .desired_width(f32::INFINITY),
        );
        name_edit(&response, "Console input");
        if state.focus || refocus {
            response.request_focus();
            state.focus = false;
        }
        let caret = caret_chars(ui.ctx(), id, &state.input);
        if response.changed() || (response.has_focus() && state.completion.open) {
            let explicit = state.completion.explicit;
            state.completion.refresh(&state.input, caret, explicit);
        }
        let enter = ui.input(|i| i.key_pressed(Key::Enter));
        if response.lost_focus() && enter {
            if state.completion.open && !state.completion.exact() {
                if let Some(caret) = state.completion.accept(&mut state.input) {
                    set_caret(ui.ctx(), id, caret);
                }
                response.request_focus();
            } else {
                let text = state.input.trim().to_string();
                if !text.is_empty() {
                    submitted = Some(text);
                    state.input.clear();
                    state.pos = None;
                    state.shown = None;
                }
                state.completion.close();
                response.request_focus();
            }
        } else if !response.has_focus() && !state.completion.contains_pointer(ui.ctx()) {
            state.completion.close();
        }
        if let Some(index) = completion_popup(ui, &mut state.completion, id, response.rect) {
            state.completion.selected = index;
            if let Some(caret) = state.completion.accept(&mut state.input) {
                set_caret(ui.ctx(), id, caret);
            }
            response.request_focus();
        }
    });
    submitted
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(value: &RespValue) -> Vec<String> {
        resp_lines(value).into_iter().map(|l| l.text).collect()
    }

    #[test]
    fn scalars_render_like_redis_cli() {
        assert_eq!(texts(&RespValue::Int(5)), ["(integer) 5"]);
        assert_eq!(texts(&RespValue::Nil), ["(nil)"]);
        assert_eq!(texts(&RespValue::Simple("OK".into())), ["OK"]);
        assert_eq!(
            texts(&RespValue::Bulk(b"a\"b\n".to_vec())),
            ["\"a\\\"b\\n\""]
        );
        assert_eq!(texts(&RespValue::Bulk(vec![0xff])), ["\"\\xff\""]);
        assert_eq!(texts(&RespValue::Double(1.5)), ["(double) 1.5"]);
        assert_eq!(texts(&RespValue::Bool(true)), ["(true)"]);
        assert_eq!(
            texts(&RespValue::Error("ERR bad".into())),
            ["(error) ERR bad"]
        );
        assert_eq!(texts(&RespValue::Verbatim("a\r\nb".into())), ["a", "b"]);
    }

    #[test]
    fn arrays_number_and_indent_their_elements() {
        let value = RespValue::Array(vec![
            RespValue::Array(vec![
                RespValue::Bulk(b"a".to_vec()),
                RespValue::Bulk(b"b".to_vec()),
            ]),
            RespValue::Int(3),
            RespValue::Array(Vec::new()),
        ]);
        assert_eq!(
            texts(&value),
            [
                "1) 1) \"a\"",
                "   2) \"b\"",
                "2) (integer) 3",
                "3) (empty array)"
            ]
        );
    }

    #[test]
    fn maps_show_key_arrow_value() {
        let value = RespValue::Map(vec![
            (RespValue::Bulk(b"k".to_vec()), RespValue::Int(1)),
            (
                RespValue::Bulk(b"l".to_vec()),
                RespValue::Array(vec![RespValue::Int(1), RespValue::Int(2)]),
            ),
        ]);
        assert_eq!(
            texts(&value),
            [
                "1# \"k\"",
                "   => (integer) 1",
                "2# \"l\"",
                "   => 1) (integer) 1",
                "      2) (integer) 2"
            ]
        );
    }

    #[test]
    fn history_stops_at_both_ends() {
        let mut state = ConsoleState::default();
        state.history_up();
        assert_eq!(state.input, "", "no history: nothing happens");
        state.history = Arc::new(vec!["a".into(), "b".into()]);
        state.input = "draft".into();
        state.history_down();
        assert_eq!(state.input, "draft", "down without navigating does nothing");
        state.history_up();
        state.history_up();
        state.history_up();
        assert_eq!(state.input, "a");
        state.history_down();
        state.history_down();
        assert_eq!(state.input, "draft");
    }

    #[test]
    fn completion_matches_commands_and_subcommands() {
        let ctx = completion_context("get", 3).expect("a command is being typed");
        assert_eq!(ctx.command, None);
        assert_eq!(ctx.prefix, "get");
        let names: Vec<&str> = matching(COMMANDS, &ctx.prefix)
            .iter()
            .map(|c| c.name)
            .collect();
        assert_eq!(
            &names[..6],
            ["GET", "GETBIT", "GETDEL", "GETEX", "GETRANGE", "GETSET"]
        );

        let ctx = completion_context("CONFIG g", 8).expect("a subcommand is being typed");
        assert_eq!(ctx.command, Some("CONFIG"));
        assert_eq!(ctx.prefix, "g");
        let names: Vec<&str> = matching(subcommands("CONFIG"), &ctx.prefix)
            .iter()
            .map(|c| c.name)
            .collect();
        assert_eq!(names, ["GET"]);

        // A third word is an argument; an unknown command has no subcommands.
        assert!(completion_context("CONFIG GET max", 14).is_none());
        assert!(completion_context("MYMODULE sub", 12).is_none());
    }

    #[test]
    fn completion_inserts_the_name_and_a_space() {
        let mut completion = Completion::default();
        completion.refresh("config g", 8, false);
        assert!(completion.open);
        completion.selected = 0;
        let mut input = "config g".to_string();
        assert_eq!(completion.accept(&mut input), Some(11));
        assert_eq!(input, "config GET ");
        assert!(!completion.open);

        // A word in the middle of the line is replaced whole, and the caret follows it.
        let mut completion = Completion::default();
        completion.refresh("CONFIG SE rest", 9, false);
        assert!(completion.open);
        completion.selected = 0;
        let mut input = "CONFIG SE rest".to_string();
        assert_eq!(completion.accept(&mut input), Some(10));
        assert_eq!(input, "CONFIG SET rest");
    }

    #[test]
    fn enter_runs_only_an_exact_command_name() {
        let mut completion = Completion::default();
        completion.refresh("pi", 2, false);
        assert!(!completion.exact(), "PIN is completed to PING first");
        completion.selected = completion
            .items
            .iter()
            .position(|c| c.name == "PING")
            .expect("PING is offered");
        assert!(!completion.exact());
        let input = "ping".to_string();
        completion.refresh(&input, 4, false);
        assert!(completion.exact());
    }
}
