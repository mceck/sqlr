//! The console: raw commands and `MULTI`/`EXEC` blocks, replies converted by RESP type.

use redis::{Cmd, Value};
use sqlr_core::{DriverError, RespValue};

use crate::classify::refused_in_console;
use crate::connect::{Inner, Role};
use crate::errors::{error_reply, to_resp};

fn command_of(args: &[Vec<u8>]) -> Cmd {
    let mut c = Cmd::new();
    for arg in args {
        c.arg(arg.as_slice());
    }
    c
}

fn refused(why: &str) -> RespValue {
    RespValue::Error(format!("ERR {why}"))
}

/// Runs one command on the console's connection for `db`. A server error is a reply
/// (`RespValue::Error`); a lost connection is an error.
pub(crate) async fn command(
    inner: &Inner,
    db: u32,
    args: Vec<Vec<u8>>,
) -> Result<RespValue, DriverError> {
    if args.is_empty() {
        return Ok(refused("empty command"));
    }
    if let Some(why) = refused_in_console(&args) {
        return Ok(refused(why));
    }
    inner.check_command(&args)?;
    let command = &command_of(&args);
    // A command may have run before the connection dropped: never repeat it.
    let reply = inner
        .with(db, Role::Console, false, |mut con| async move {
            let con = &mut con;
            match command.query_async::<Value>(con).await {
                Ok(value) => Ok(to_resp(value)),
                Err(e) => match error_reply(&e) {
                    Some(text) => Ok(RespValue::Error(text)),
                    None => Err(e),
                },
            }
        })
        .await?;
    Ok(reply)
}

/// Runs `commands` as one `MULTI`/`EXEC` block and returns the `EXEC` reply: an array with
/// each command's reply (a command that fails at execution shows as an error entry).
pub(crate) async fn exec_block(
    inner: &Inner,
    db: u32,
    commands: Vec<Vec<Vec<u8>>>,
) -> Result<RespValue, DriverError> {
    if commands.is_empty() {
        return Ok(RespValue::Array(Vec::new()));
    }
    // Redis silently ignores an empty command (`*0`), so the transaction's reply
    // never arrives and the console's connection would wait forever.
    if commands.iter().any(|args| args.is_empty()) {
        return Ok(refused("empty command"));
    }
    for args in &commands {
        if let Some(why) = refused_in_console(args) {
            return Ok(refused(why));
        }
        inner.check_command(args)?;
    }
    let mut pipe = redis::pipe();
    for args in &commands {
        pipe.add_command(command_of(args));
    }
    pipe.atomic().ignore_errors();
    let pipe = &pipe;
    inner
        .with(db, Role::Console, false, |mut con| async move {
            let con = &mut con;
            match pipe.query_async::<Value>(con).await {
                Ok(value) => Ok(to_resp(value)),
                Err(e) => match error_reply(&e) {
                    Some(text) => Ok(RespValue::Error(text)),
                    None => Err(e),
                },
            }
        })
        .await
}
