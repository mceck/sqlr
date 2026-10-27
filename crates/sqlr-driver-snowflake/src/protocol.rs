use super::*;
use std::collections::VecDeque;

pub(super) fn invalid() -> DriverError {
    DriverError::new(ErrorKind::Protocol, "Invalid Snowflake SQL API response")
}
pub(super) fn validate_handle(handle: &str) -> Result<(), DriverError> {
    if handle.is_empty()
        || !handle
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        Err(invalid())
    } else {
        Ok(())
    }
}
impl Api {
    pub(super) async fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<&Json>,
    ) -> Result<(u16, Json), DriverError> {
        // Construct paths ourselves. Never follow statementStatusUrl/Link URLs supplied
        // by the server, since that could disclose the Authorization token.
        let mut url = self.endpoint.join(path).map_err(|_| invalid())?;
        if !query.is_empty() {
            url.query_pairs_mut()
                .extend_pairs(query.iter().map(|(key, value)| (*key, value)));
        }
        let mut request = self.client.request(method, url);
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request.send().await.map_err(|error| {
            DriverError::new(
                if error.is_timeout() {
                    ErrorKind::Timeout
                } else {
                    ErrorKind::Connection
                },
                "Snowflake HTTPS request failed",
            )
        })?;
        let status = response.status().as_u16();
        let body = response.json::<Json>().await;
        if status == 401 {
            return Err(DriverError::new(
                ErrorKind::Auth,
                "Snowflake authentication failed; check or renew the access token",
            ));
        }
        if status == 403 {
            return Err(DriverError::new(
                ErrorKind::Permission,
                "Snowflake access denied",
            ));
        }
        if status >= 500 {
            return Err(DriverError::new(
                ErrorKind::Connection,
                "Snowflake service unavailable",
            ));
        }
        let body = body.map_err(|_| invalid())?;
        if !body.is_object() {
            return Err(invalid());
        }
        let state = body.get("sqlState").and_then(Json::as_str).unwrap_or("");
        if !(200..300).contains(&status) && !matches!(status, 429)
            || (!state.is_empty() && state != "00000")
        {
            // Never attach a reqwest error URL. Authentication, permission and service
            // failures keep a static message.
            let kind = if state == "57014" {
                ErrorKind::Cancelled
            } else if state.starts_with("28") {
                ErrorKind::Auth
            } else if state == "42501" {
                ErrorKind::Permission
            } else if state.starts_with("42") {
                ErrorKind::Syntax
            } else if status >= 500 {
                ErrorKind::Connection
            } else {
                ErrorKind::Other
            };
            let summary = match kind {
                ErrorKind::Cancelled => "Snowflake query cancelled",
                ErrorKind::Syntax => "Snowflake SQL compilation failed",
                ErrorKind::Auth => "Snowflake authentication failed",
                ErrorKind::Permission => "Snowflake access denied",
                ErrorKind::Connection => "Snowflake service unavailable",
                _ => "Snowflake statement failed",
            };
            // A statement error is only useful with the server's explanation, like any
            // other driver's; the token is removed from it in case the server echoes it.
            let detail = matches!(kind, ErrorKind::Syntax | ErrorKind::Other)
                .then(|| body.get("message").and_then(Json::as_str))
                .flatten()
                .map(|message| message.replace(self.token.as_str(), "***"))
                .filter(|message| !message.trim().is_empty());
            return Err(DriverError::new(
                kind,
                match detail {
                    Some(detail) => format!("{summary}: {}", detail.trim()),
                    None => summary.into(),
                },
            ));
        }
        Ok((status, body))
    }
    pub(super) async fn cancel(&self, handle: &str) -> Result<(), DriverError> {
        match self
            .request(
                reqwest::Method::POST,
                &format!("api/v2/statements/{handle}/cancel"),
                &[],
                None,
            )
            .await
        {
            Ok(_) => Ok(()),
            Err(error) if error.kind == ErrorKind::Cancelled => Ok(()),
            Err(error) => Err(error),
        }
    }
}
pub(super) fn bindings(params: &[Value]) -> Result<Json, DriverError> {
    let mut bindings = serde_json::Map::new();
    for (i, value) in params.iter().enumerate() {
        let (kind, value) = match value {
            Value::Null => ("TEXT", Json::Null),
            Value::Bool(v) => ("BOOLEAN", json!(v.to_string())),
            Value::Int(v) => ("FIXED", json!(v.to_string())),
            Value::Float(v) => {
                if !v.is_finite() {
                    return Err(DriverError::new(
                        ErrorKind::Unsupported,
                        "Non-finite Snowflake bound floats are unsupported",
                    ));
                }
                ("REAL", json!(v.to_string()))
            }
            Value::Text(v) => ("TEXT", json!(v)),
            Value::Bytes(v) => ("BINARY", json!(&sqlr_core::hex_bytes(v)[2..])),
        };
        bindings.insert((i + 1).to_string(), json!({"type":kind,"value":value}));
    }
    Ok(Json::Object(bindings))
}
fn columns(response: &Json) -> Result<Arc<[ColumnMeta]>, DriverError> {
    let metadata = response.get("resultSetMetaData").ok_or_else(invalid)?;
    if metadata.get("format").and_then(Json::as_str) != Some("jsonv2") {
        return Err(invalid());
    }
    let row_type = metadata
        .get("rowType")
        .and_then(Json::as_array)
        .ok_or_else(invalid)?;
    row_type
        .iter()
        .map(|column| {
            let name = column
                .get("name")
                .and_then(Json::as_str)
                .ok_or_else(invalid)?;
            let ty = column
                .get("type")
                .and_then(Json::as_str)
                .ok_or_else(invalid)?;
            let (kind, hint) = match ty.to_ascii_lowercase().as_str() {
                "fixed" | "decfloat" => (ColumnKind::Text, DisplayHint::Decimal),
                "real" => (ColumnKind::Float, DisplayHint::Number),
                "boolean" => (ColumnKind::Bool, DisplayHint::Plain),
                "binary" => (ColumnKind::Bytes, DisplayHint::Binary),
                "date" | "time" | "timestamp_ltz" | "timestamp_ntz" | "timestamp_tz" => {
                    (ColumnKind::Text, DisplayHint::Temporal)
                }
                "variant" | "object" => (ColumnKind::Text, DisplayHint::Json),
                "array" => (ColumnKind::Text, DisplayHint::Array),
                _ => (ColumnKind::Text, DisplayHint::Plain),
            };
            Ok(ColumnMeta::new(name, ty, kind, hint))
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Arc::from)
}
fn data(
    response: &Json,
    columns: &[ColumnMeta],
    expected: u64,
) -> Result<VecDeque<Vec<Value>>, DriverError> {
    let data = response
        .get("data")
        .and_then(Json::as_array)
        .ok_or_else(invalid)?;
    if data.len() as u64 != expected {
        return Err(invalid());
    }
    data.iter()
        .map(|row| {
            let row = row.as_array().ok_or_else(invalid)?;
            if row.len() != columns.len() {
                return Err(invalid());
            }
            row.iter()
                .zip(columns)
                .map(|(cell, column)| {
                    if cell.is_null() {
                        return Ok(Value::Null);
                    }
                    let value = cell.as_str().ok_or_else(invalid)?;
                    match column.kind {
                        ColumnKind::Bool => match value {
                            "true" => Ok(Value::Bool(true)),
                            "false" => Ok(Value::Bool(false)),
                            _ => Err(invalid()),
                        },
                        ColumnKind::Float => value
                            .parse::<f64>()
                            .map(Value::Float)
                            .map_err(|_| invalid()),
                        ColumnKind::Bytes => {
                            if !value.len().is_multiple_of(2) || !value.is_ascii() {
                                return Err(invalid());
                            }
                            let bytes = (0..value.len())
                                .step_by(2)
                                .map(|i| {
                                    u8::from_str_radix(&value[i..i + 2], 16).map_err(|_| invalid())
                                })
                                .collect::<Result<Vec<_>, _>>()?;
                            Ok(Value::Bytes(bytes))
                        }
                        _ => Ok(Value::Text(value.into())),
                    }
                })
                .collect()
        })
        .collect()
}
pub(super) struct Stream {
    api: Arc<Api>,
    handle: String,
    columns: Arc<[ColumnMeta]>,
    rows: VecDeque<Vec<Value>>,
    partitions: Vec<u64>,
    next_partition: usize,
    guard: Option<QueryGuard>,
}
impl Stream {
    pub(super) fn new(
        api: Arc<Api>,
        handle: String,
        response: &Json,
        guard: QueryGuard,
    ) -> Result<Self, DriverError> {
        let columns = columns(response)?;
        let metadata = &response["resultSetMetaData"];
        let num_rows = metadata
            .get("numRows")
            .and_then(Json::as_u64)
            .ok_or_else(invalid)?;
        let partitions = metadata
            .get("partitionInfo")
            .and_then(Json::as_array)
            .ok_or_else(invalid)?
            .iter()
            .map(|p| p.get("rowCount").and_then(Json::as_u64).ok_or_else(invalid))
            .collect::<Result<Vec<_>, _>>()?;
        if partitions.is_empty()
            || partitions
                .iter()
                .try_fold(0u64, |sum, n| sum.checked_add(*n))
                != Some(num_rows)
        {
            return Err(invalid());
        }
        let refetch = response.get("code").and_then(Json::as_str) == Some("391908");
        let rows = if refetch {
            VecDeque::new()
        } else {
            data(response, &columns, partitions[0])?
        };
        Ok(Self {
            api,
            handle,
            columns,
            rows,
            partitions,
            next_partition: if refetch { 0 } else { 1 },
            guard: Some(guard),
        })
    }
}
#[async_trait::async_trait]
impl RowStream for Stream {
    fn columns(&self) -> Arc<[ColumnMeta]> {
        self.columns.clone()
    }
    async fn next_chunk(&mut self, max_rows: usize) -> Result<Option<ResultChunk>, DriverError> {
        let Some(guard) = &self.guard else {
            return Ok(None);
        };
        guard.query.check()?;
        if max_rows == 0 {
            return Err(DriverError::new(
                ErrorKind::Other,
                "Chunk size must be greater than zero",
            ));
        }
        let mut builder = ChunkBuilder::new(self.columns.clone());
        while builder.len() < max_rows {
            if let Some(row) = self.rows.pop_front() {
                let cells: Vec<_> = row.iter().map(Value::as_cell).collect();
                builder.push_row(&cells).map_err(|_| invalid())?;
            } else if self.next_partition < self.partitions.len() {
                let partition = self.next_partition;
                let mut retries = 0;
                let response = loop {
                    guard.query.check()?;
                    let (status, response) = self
                        .api
                        .request(
                            reqwest::Method::GET,
                            &format!("api/v2/statements/{}", self.handle),
                            &[("partition", partition.to_string())],
                            None,
                        )
                        .await?;
                    guard.query.check()?;
                    if status == 200 {
                        break response;
                    }
                    if !matches!(status, 202 | 429) {
                        return Err(invalid());
                    }
                    if retries == 5 {
                        return Err(DriverError::new(
                            ErrorKind::Timeout,
                            "Snowflake result partition is unavailable; retry the query",
                        ));
                    }
                    tokio::time::sleep(Duration::from_millis(100 << retries)).await;
                    retries += 1;
                };
                self.rows = data(&response, &self.columns, self.partitions[partition])?;
                self.next_partition += 1;
            } else {
                self.guard = None;
                break;
            }
        }
        if builder.is_empty() {
            Ok(None)
        } else {
            Ok(Some(builder.finish()))
        }
    }
    async fn close(&mut self) -> Result<(), DriverError> {
        self.rows.clear();
        self.guard = None;
        Ok(())
    }
}
