use super::*;
use clickhouse::query::BytesCursor;
use tokio::io::AsyncBufReadExt;

pub(super) struct ClickHouseStream {
    reader: BytesCursor,
    columns: Arc<[ColumnMeta]>,
    guard: Option<QueryGuard>,
}
impl ClickHouseStream {
    pub(super) async fn open(
        cursor: BytesCursor,
        guard: QueryGuard,
    ) -> Result<Execution, DriverError> {
        let mut stream = Self {
            reader: cursor,
            columns: Arc::from([]),
            guard: Some(guard),
        };
        let Some(names) = stream.line().await? else {
            let affected = stream
                .reader
                .summary()
                .and_then(|s| s.written_rows())
                .unwrap_or(0);
            stream.finish();
            return Ok(Execution::Affected(affected));
        };
        let names: Vec<String> = serde_json::from_str(&names).map_err(|_| invalid())?;
        let types: Vec<String> = serde_json::from_str(&stream.line().await?.ok_or_else(invalid)?)
            .map_err(|_| invalid())?;
        if names.len() != types.len() {
            return Err(invalid());
        }
        stream.columns = names
            .into_iter()
            .zip(types)
            .map(|(name, ty)| {
                let (kind, hint) = column_type(&ty);
                ColumnMeta::new(name, ty, kind, hint)
            })
            .collect::<Vec<_>>()
            .into();
        Ok(Execution::Rows(Box::new(stream)))
    }
    async fn line(&mut self) -> Result<Option<String>, DriverError> {
        let Some(guard) = &self.guard else {
            return Ok(None);
        };
        if guard.query.cancelled.load(Ordering::SeqCst) {
            return Err(DriverError::new(
                ErrorKind::Cancelled,
                "ClickHouse query cancelled",
            ));
        }
        let _active = guard.activate()?;
        let mut line = String::new();
        tokio::select! {
            biased;
            _ = guard.query.notify.notified() => Err(DriverError::new(ErrorKind::Cancelled, "ClickHouse query cancelled")),
            read = self.reader.read_line(&mut line).with_subscriber(tracing::subscriber::NoSubscriber::default()) => {
                let read = read.map_err(|e| map_error(&e.into(), &guard.query.password))?;
                if read == 0 { Ok(None) } else { Ok(Some(line)) }
            }
        }
    }
    fn finish(&mut self) {
        if let Some(mut guard) = self.guard.take() {
            guard.finished = true;
        }
    }
}
#[async_trait::async_trait]
impl RowStream for ClickHouseStream {
    fn columns(&self) -> Arc<[ColumnMeta]> {
        self.columns.clone()
    }
    async fn next_chunk(&mut self, max_rows: usize) -> Result<Option<ResultChunk>, DriverError> {
        let mut builder = ChunkBuilder::new(self.columns.clone());
        let mut count = 0;
        for _ in 0..max_rows.max(1) {
            let Some(line) = self.line().await? else {
                self.finish();
                break;
            };
            let row: Vec<serde_json::Value> = serde_json::from_str(&line).map_err(|_| invalid())?;
            if row.len() != self.columns.len() {
                return Err(invalid());
            }
            let values = row
                .iter()
                .zip(self.columns.iter())
                .map(|(value, col)| decode(value, col.kind))
                .collect::<Result<Vec<_>, _>>()?;
            builder
                .push_row(&values.iter().map(Value::as_cell).collect::<Vec<_>>())
                .map_err(|_| invalid())?;
            count += 1;
        }
        Ok((count > 0).then(|| builder.finish()))
    }
    async fn close(&mut self) -> Result<(), DriverError> {
        if let Some(mut guard) = self.guard.take() {
            let result = guard.query.kill().await;
            guard.finished = result.is_ok();
            drop(guard);
            result?;
        }
        Ok(())
    }
}
fn base_type(mut ty: &str) -> &str {
    loop {
        if let Some(inner) = ty
            .strip_prefix("Nullable(")
            .or_else(|| ty.strip_prefix("LowCardinality("))
            .and_then(|s| s.strip_suffix(')'))
        {
            ty = inner;
        } else {
            return ty;
        }
    }
}
fn column_type(ty: &str) -> (ColumnKind, DisplayHint) {
    let ty = base_type(ty);
    match ty {
        "Bool" => (ColumnKind::Bool, DisplayHint::Plain),
        "Int8" | "Int16" | "Int32" | "Int64" | "UInt8" | "UInt16" | "UInt32" => {
            (ColumnKind::Int, DisplayHint::Number)
        }
        "Float32" | "Float64" | "BFloat16" => (ColumnKind::Float, DisplayHint::Number),
        "UInt64" | "UInt128" | "UInt256" | "Int128" | "Int256" => {
            (ColumnKind::Text, DisplayHint::Decimal)
        }
        "UUID" => (ColumnKind::Text, DisplayHint::Uuid),
        _ if ty.starts_with("Decimal") => (ColumnKind::Text, DisplayHint::Decimal),
        _ if ty.starts_with("Date") || ty.starts_with("Time") => {
            (ColumnKind::Text, DisplayHint::Temporal)
        }
        _ if ty.starts_with("Array(") => (ColumnKind::Text, DisplayHint::Array),
        _ if ty.starts_with("JSON") => (ColumnKind::Text, DisplayHint::Json),
        _ => (ColumnKind::Text, DisplayHint::Plain),
    }
}
fn decode(value: &serde_json::Value, kind: ColumnKind) -> Result<Value, DriverError> {
    if value.is_null() {
        return Ok(Value::Null);
    }
    let text = value
        .as_str()
        .map_or_else(|| value.to_string(), str::to_owned);
    match kind {
        ColumnKind::Bool => match text.as_str() {
            "true" | "1" => Ok(Value::Bool(true)),
            "false" | "0" => Ok(Value::Bool(false)),
            _ => Err(invalid()),
        },
        ColumnKind::Int => text.parse().map(Value::Int).map_err(|_| invalid()),
        ColumnKind::Float => match text.as_str() {
            "inf" | "+inf" => Ok(Value::Float(f64::INFINITY)),
            "-inf" => Ok(Value::Float(f64::NEG_INFINITY)),
            "nan" => Ok(Value::Float(f64::NAN)),
            _ => text.parse().map(Value::Float).map_err(|_| invalid()),
        },
        _ => Ok(Value::Text(text)),
    }
}
