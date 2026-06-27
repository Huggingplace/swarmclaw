use crate::core::state::{Message, Role, State};
use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};
use tracing::{info, warn};

#[derive(Debug, Clone)]
pub struct SessionSummary {
    pub session_id: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub message_count: usize,
    pub last_role: Option<Role>,
    pub last_timestamp: Option<u64>,
    pub last_content_preview: Option<String>,
}

#[derive(Debug, Clone)]
pub struct IndexedMessage {
    pub message_index: usize,
    pub message: Message,
}

pub fn derive_store_path(session_path: &Path) -> PathBuf {
    let sessions_dir = session_path.parent().unwrap_or_else(|| Path::new("."));
    let state_root = sessions_dir.parent().unwrap_or(sessions_dir);
    state_root.join("sessions.sqlite")
}

pub fn workspace_store_path(workspace_path: &Path) -> PathBuf {
    derive_store_path(
        &workspace_path
            .join(".swarmclaw")
            .join("sessions")
            .join("default.json"),
    )
}

pub fn migrate_legacy_sessions_in_workspace(workspace_path: &Path) -> Result<PathBuf> {
    let store_path = workspace_store_path(workspace_path);
    let sessions_dir = workspace_path.join(".swarmclaw").join("sessions");
    if !sessions_dir.exists() {
        return Ok(store_path);
    }

    for entry in std::fs::read_dir(&sessions_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }

        let Some(session_id) = path.file_stem().and_then(|value| value.to_str()) else {
            continue;
        };
        if session_id.is_empty() {
            continue;
        }

        let _ = load_session_state(&store_path, session_id, &path)?;
    }

    Ok(store_path)
}

pub fn load_session_state(
    store_path: &Path,
    session_id: &str,
    legacy_state_path: &Path,
) -> Result<Option<State>> {
    let mut conn = open_store(store_path)?;

    if let Some(state) = load_state_from_db(&conn, session_id)? {
        return Ok(Some(state));
    }

    let Some(legacy_state) = load_legacy_state(legacy_state_path)? else {
        return Ok(None);
    };

    persist_full_state(&mut conn, session_id, &legacy_state)?;
    info!(
        session_id = %session_id,
        store_path = %store_path.display(),
        legacy_state_path = %legacy_state_path.display(),
        migrated_messages = legacy_state.history.len(),
        "Migrated legacy JSON session history into SQLite"
    );

    Ok(Some(legacy_state))
}

pub fn persist_seed_state(store_path: &Path, session_id: &str, state: &State) -> Result<()> {
    if state.history.is_empty() {
        return Ok(());
    }

    let mut conn = open_store(store_path)?;
    persist_full_state(&mut conn, session_id, state)
}

pub fn persist_message(
    store_path: &Path,
    session_id: &str,
    message_index: usize,
    message: &Message,
) -> Result<()> {
    let mut conn = open_store(store_path)?;
    let tx = conn.transaction()?;
    upsert_session_row(&tx, session_id)?;
    insert_message_row(&tx, session_id, message_index, message)?;
    tx.commit()?;
    Ok(())
}

pub fn list_sessions(store_path: &Path, limit: usize) -> Result<Vec<SessionSummary>> {
    if !store_path.exists() {
        return Ok(Vec::new());
    }

    let conn = open_store(store_path)?;
    let mut stmt = conn.prepare(
        "SELECT session_id, created_at, updated_at
         FROM sessions
         ORDER BY updated_at DESC
         LIMIT ?1",
    )?;

    let session_rows = stmt.query_map(params![limit.max(1) as i64], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
        ))
    })?;

    let mut summaries = Vec::new();
    for row in session_rows {
        let (session_id, created_at, updated_at) = row?;
        let message_count = conn.query_row(
            "SELECT COUNT(*) FROM session_messages WHERE session_id = ?1",
            params![&session_id],
            |row| row.get::<_, i64>(0),
        )? as usize;

        let last_message = conn
            .query_row(
                "SELECT role, timestamp, content
                 FROM session_messages
                 WHERE session_id = ?1
                 ORDER BY message_index DESC
                 LIMIT 1",
                params![&session_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)? as u64,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?;

        let (last_role, last_timestamp, last_content_preview) = match last_message {
            Some((role, timestamp, content)) => {
                (Some(role_from_str(&role)?), Some(timestamp), Some(content))
            }
            None => (None, None, None),
        };

        summaries.push(SessionSummary {
            session_id,
            created_at,
            updated_at,
            message_count,
            last_role,
            last_timestamp,
            last_content_preview,
        });
    }

    Ok(summaries)
}

pub fn load_recent_history(
    store_path: &Path,
    session_id: &str,
    limit: usize,
) -> Result<Option<Vec<IndexedMessage>>> {
    if !store_path.exists() {
        return Ok(None);
    }

    let conn = open_store(store_path)?;
    if !session_exists(&conn, session_id)? {
        return Ok(None);
    }

    let mut stmt = conn.prepare(
        "SELECT message_index, role, content, timestamp, tool_calls_json, tool_call_id
         FROM session_messages
         WHERE session_id = ?1
         ORDER BY message_index DESC
         LIMIT ?2",
    )?;

    let rows = stmt.query_map(params![session_id, limit.max(1) as i64], |row| {
        let role_raw: String = row.get(1)?;
        let tool_calls_json: Option<String> = row.get(4)?;
        Ok(IndexedMessage {
            message_index: row.get::<_, i64>(0)? as usize,
            message: Message {
                role: role_from_str(&role_raw).map_err(to_sql_conversion_error)?,
                content: row.get(2)?,
                timestamp: row.get::<_, i64>(3)? as u64,
                tool_calls: tool_calls_json
                    .map(|json| serde_json::from_str(&json))
                    .transpose()
                    .map_err(to_sql_conversion_error)?,
                tool_call_id: row.get(5)?,
            },
        })
    })?;

    let mut history = Vec::new();
    for row in rows {
        history.push(row?);
    }
    history.reverse();

    Ok(Some(history))
}

fn load_legacy_state(state_path: &Path) -> Result<Option<State>> {
    if !state_path.exists() {
        return Ok(None);
    }

    let bytes = std::fs::read(state_path)
        .with_context(|| format!("unable to read {}", state_path.display()))?;
    if bytes.is_empty() {
        return Ok(None);
    }

    let state: State = serde_json::from_slice(&bytes)
        .with_context(|| format!("unable to parse {}", state_path.display()))?;
    if state.history.is_empty() {
        return Ok(None);
    }

    Ok(Some(state))
}

fn open_store(store_path: &Path) -> Result<Connection> {
    if let Some(parent) = store_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("unable to create {}", parent.display()))?;
    }

    let conn = Connection::open(store_path)
        .with_context(|| format!("unable to open {}", store_path.display()))?;
    init_store(&conn, store_path)?;
    Ok(conn)
}

fn session_exists(conn: &Connection, session_id: &str) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM sessions WHERE session_id = ?1 LIMIT 1",
            params![session_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .is_some())
}

fn init_store(conn: &Connection, store_path: &Path) -> Result<()> {
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA foreign_keys = ON;
         CREATE TABLE IF NOT EXISTS sessions (
             session_id TEXT PRIMARY KEY,
             created_at INTEGER NOT NULL,
             updated_at INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS session_messages (
             id INTEGER PRIMARY KEY AUTOINCREMENT,
             session_id TEXT NOT NULL,
             message_index INTEGER NOT NULL,
             role TEXT NOT NULL,
             content TEXT NOT NULL,
             timestamp INTEGER NOT NULL,
             tool_calls_json TEXT,
             tool_call_id TEXT,
             created_at INTEGER NOT NULL,
             FOREIGN KEY (session_id) REFERENCES sessions(session_id) ON DELETE CASCADE,
             UNIQUE (session_id, message_index)
         );
         CREATE INDEX IF NOT EXISTS idx_session_messages_session_idx
             ON session_messages(session_id, message_index);",
    )
    .with_context(|| format!("unable to initialize {}", store_path.display()))?;

    // Cross-session retrieval (PR-8): an FTS5 full-text index over message
    // content. This is BEST-EFFORT: if the bundled SQLite was compiled without
    // the FTS5 module the CREATE will fail, but the core store must keep
    // working. We only log a warning and continue. `search_messages` returns an
    // empty result when the table is absent, and `index_message_fts` is a
    // no-op on error, so the rest of the application is unaffected.
    if let Err(error) = conn.execute_batch(
        "CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
             session_id UNINDEXED,
             role UNINDEXED,
             content,
             ts UNINDEXED
         );",
    ) {
        warn!(
            store_path = %store_path.display(),
            "FTS5 unavailable; cross-session retrieval disabled: {error}"
        );
    }

    Ok(())
}

/// Whether the FTS5 `messages_fts` table exists in this connection's database.
/// Used so `search_messages` can short-circuit (and tests can detect whether
/// FTS5 was actually available in the bundled SQLite).
fn fts_table_exists(conn: &Connection) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'messages_fts' LIMIT 1",
            [],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .is_some())
}

/// Insert one message into the FTS5 index. BEST-EFFORT: any error (e.g. FTS5
/// unavailable) is logged and swallowed so that message persistence is never
/// broken by retrieval indexing.
fn index_message_fts(conn: &Connection, session_id: &str, message: &Message) {
    // Only index human-readable conversational text. Skip empty bodies.
    if message.content.trim().is_empty() {
        return;
    }
    let result = conn.execute(
        "INSERT INTO messages_fts (session_id, role, content, ts)
         VALUES (?1, ?2, ?3, ?4)",
        params![
            session_id,
            role_as_str(&message.role),
            message.content.as_str(),
            message.timestamp as i64,
        ],
    );
    if let Err(error) = result {
        warn!(
            session_id = %session_id,
            "Failed to index message into FTS5 (continuing): {error}"
        );
    }
}

/// Build a SAFE FTS5 `MATCH` query string from arbitrary user input.
///
/// FTS5's MATCH syntax has its own operators (`AND`, `OR`, `NOT`, `*`, `:`,
/// `"..."`, `(`, `)`, etc.), so passing raw user text straight in can cause a
/// syntax error or behave surprisingly. The robust approach used here is to
/// tokenize the input into alphanumeric words, drop everything else, quote each
/// word, and join them with `OR`. This means any arbitrary string is reduced to
/// a harmless disjunction of literal terms — no injection, no syntax errors.
///
/// Returns an empty string when there are no usable terms (callers treat an
/// empty query as "no search").
pub fn build_fts_query(raw: &str) -> String {
    let mut terms: Vec<String> = Vec::new();
    let mut current = String::new();
    for ch in raw.chars() {
        if ch.is_alphanumeric() {
            current.push(ch);
        } else if !current.is_empty() {
            terms.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        terms.push(current);
    }

    terms
        .into_iter()
        // Drop single-character noise tokens to reduce useless matches.
        .filter(|term| term.chars().count() >= 2)
        // Quote each term so it is treated as a literal string token by FTS5.
        .map(|term| format!("\"{term}\""))
        .collect::<Vec<_>>()
        .join(" OR ")
}

/// Search past session messages by relevance using the FTS5 index.
///
/// - `query` is arbitrary user text; it is sanitized via [`build_fts_query`].
/// - `limit` caps the number of returned hits (most relevant first, by `rank`).
/// - `exclude_session`, when set, filters out matches from that session id
///   (so a session does not retrieve its own current messages).
///
/// Returns an empty vec for an empty/too-short query, when the store does not
/// exist, or when FTS5 is unavailable. Returned `IndexedMessage` entries carry
/// the matched message; `message_index` is set to 0 (the FTS row is content
/// only and not tied back to the ordered transcript index).
pub fn search_messages(
    store_path: &Path,
    query: &str,
    limit: usize,
    exclude_session: Option<&str>,
) -> Result<Vec<IndexedMessage>> {
    if !store_path.exists() || limit == 0 {
        return Ok(Vec::new());
    }

    let match_query = build_fts_query(query);
    if match_query.is_empty() {
        return Ok(Vec::new());
    }

    let conn = open_store(store_path)?;
    if !fts_table_exists(&conn)? {
        return Ok(Vec::new());
    }

    let mut stmt = conn.prepare(
        "SELECT session_id, role, content, ts
         FROM messages_fts
         WHERE messages_fts MATCH ?1
           AND (?3 IS NULL OR session_id <> ?3)
         ORDER BY rank
         LIMIT ?2",
    )?;

    let rows = stmt.query_map(
        params![match_query, limit.max(1) as i64, exclude_session],
        |row| {
            let role_raw: String = row.get(1)?;
            Ok(IndexedMessage {
                message_index: 0,
                message: Message {
                    role: role_from_str(&role_raw).map_err(to_sql_conversion_error)?,
                    content: row.get(2)?,
                    timestamp: row.get::<_, i64>(3)? as u64,
                    tool_calls: None,
                    tool_call_id: None,
                },
            })
        },
    )?;

    let mut hits = Vec::new();
    for row in rows {
        hits.push(row?);
    }
    Ok(hits)
}

fn load_state_from_db(conn: &Connection, session_id: &str) -> Result<Option<State>> {
    if !session_exists(conn, session_id)? {
        return Ok(None);
    }

    let mut stmt = conn.prepare(
        "SELECT role, content, timestamp, tool_calls_json, tool_call_id
         FROM session_messages
         WHERE session_id = ?1
         ORDER BY message_index ASC",
    )?;

    let rows = stmt.query_map(params![session_id], |row| {
        let role_raw: String = row.get(0)?;
        let tool_calls_json: Option<String> = row.get(3)?;

        Ok(Message {
            role: role_from_str(&role_raw).map_err(to_sql_conversion_error)?,
            content: row.get(1)?,
            timestamp: row.get::<_, i64>(2)? as u64,
            tool_calls: tool_calls_json
                .map(|json| serde_json::from_str(&json))
                .transpose()
                .map_err(to_sql_conversion_error)?,
            tool_call_id: row.get(4)?,
        })
    })?;

    let mut history = Vec::new();
    for row in rows {
        history.push(row?);
    }

    if history.is_empty() {
        return Ok(None);
    }

    Ok(Some(State { history }))
}

fn persist_full_state(conn: &mut Connection, session_id: &str, state: &State) -> Result<()> {
    let tx = conn.transaction()?;
    upsert_session_row(&tx, session_id)?;
    tx.execute(
        "DELETE FROM session_messages WHERE session_id = ?1",
        params![session_id],
    )?;
    // Clear any prior FTS rows for this session so a full re-persist does not
    // duplicate them. Best-effort: ignore failure / missing FTS table.
    let _ = tx.execute(
        "DELETE FROM messages_fts WHERE session_id = ?1",
        params![session_id],
    );

    for (index, message) in state.history.iter().enumerate() {
        insert_message_row(&tx, session_id, index, message)?;
    }

    tx.execute(
        "UPDATE sessions SET updated_at = ?2 WHERE session_id = ?1",
        params![session_id, now_millis()],
    )?;
    tx.commit()?;
    Ok(())
}

fn upsert_session_row(conn: &Connection, session_id: &str) -> Result<()> {
    let now = now_millis();
    conn.execute(
        "INSERT INTO sessions (session_id, created_at, updated_at)
         VALUES (?1, ?2, ?2)
         ON CONFLICT(session_id) DO UPDATE SET updated_at = excluded.updated_at",
        params![session_id, now],
    )?;
    Ok(())
}

fn insert_message_row(
    conn: &Connection,
    session_id: &str,
    message_index: usize,
    message: &Message,
) -> Result<()> {
    let tool_calls_json = message
        .tool_calls
        .as_ref()
        .map(serde_json::to_string)
        .transpose()?;

    conn.execute(
        "INSERT OR IGNORE INTO session_messages (
             session_id,
             message_index,
             role,
             content,
             timestamp,
             tool_calls_json,
             tool_call_id,
             created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            session_id,
            message_index as i64,
            role_as_str(&message.role),
            message.content.as_str(),
            message.timestamp as i64,
            tool_calls_json,
            message.tool_call_id.as_deref(),
            now_millis(),
        ],
    )?;

    conn.execute(
        "UPDATE sessions SET updated_at = ?2 WHERE session_id = ?1",
        params![session_id, now_millis()],
    )?;

    // Cross-session retrieval index (PR-8). Best-effort: never fail persistence.
    index_message_fts(conn, session_id, message);

    Ok(())
}

fn role_as_str(role: &Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

fn role_from_str(raw: &str) -> Result<Role> {
    match raw {
        "system" => Ok(Role::System),
        "user" => Ok(Role::User),
        "assistant" => Ok(Role::Assistant),
        "tool" => Ok(Role::Tool),
        _ => anyhow::bail!("unknown persisted role '{}'", raw),
    }
}

fn to_sql_conversion_error<E>(error: E) -> rusqlite::Error
where
    E: std::fmt::Display,
{
    rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Text,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            error.to_string(),
        )),
    )
}

fn now_millis() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrates_legacy_json_and_appends_new_messages() -> Result<()> {
        let root =
            std::env::temp_dir().join(format!("swarmclaw-session-store-{}", uuid::Uuid::new_v4()));
        let sessions_dir = root.join(".swarmclaw").join("sessions");
        std::fs::create_dir_all(&sessions_dir)?;

        let legacy_state_path = sessions_dir.join("default.json");
        let legacy_state = State {
            history: vec![Message {
                role: Role::System,
                content: "legacy system prompt".to_string(),
                timestamp: 1,
                tool_calls: None,
                tool_call_id: None,
            }],
        };
        std::fs::write(&legacy_state_path, serde_json::to_vec(&legacy_state)?)?;

        let store_path = derive_store_path(&legacy_state_path);
        let migrated =
            load_session_state(&store_path, "default", &legacy_state_path)?.expect("state");
        assert_eq!(migrated.history.len(), 1);
        assert_eq!(migrated.history[0].content, "legacy system prompt");

        persist_message(
            &store_path,
            "default",
            1,
            &Message {
                role: Role::User,
                content: "hello".to_string(),
                timestamp: 2,
                tool_calls: None,
                tool_call_id: None,
            },
        )?;

        let reloaded =
            load_session_state(&store_path, "default", &legacy_state_path)?.expect("state");
        assert_eq!(reloaded.history.len(), 2);
        assert_eq!(reloaded.history[1].content, "hello");

        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn build_fts_query_tokenizes_and_quotes_words() {
        assert_eq!(build_fts_query("hello world"), "\"hello\" OR \"world\"");
        // Punctuation / FTS operators are stripped, never passed through.
        assert_eq!(
            build_fts_query("  rust's  (AND) foo-bar! "),
            "\"rust\" OR \"AND\" OR \"foo\" OR \"bar\""
        );
        // Single-character noise tokens are dropped.
        assert_eq!(build_fts_query("a bб x deployment"), "\"bб\" OR \"deployment\"");
    }

    #[test]
    fn build_fts_query_empty_for_garbage() {
        assert_eq!(build_fts_query(""), "");
        assert_eq!(build_fts_query("   "), "");
        assert_eq!(build_fts_query("!!! @#$ ()"), "");
        // All tokens single-char -> dropped -> empty.
        assert_eq!(build_fts_query("a b c"), "");
    }

    #[test]
    fn fts5_is_available_in_bundled_sqlite() {
        // Documents which path PR-8 took: FTS5 vs LIKE fallback.
        let conn = Connection::open_in_memory().expect("in-memory conn");
        let created = conn
            .execute_batch("CREATE VIRTUAL TABLE t USING fts5(x);")
            .is_ok();
        assert!(
            created,
            "bundled SQLite lacks FTS5; PR-8 expects FTS5 to be available"
        );
    }

    fn msg(role: Role, content: &str, ts: u64) -> Message {
        Message {
            role,
            content: content.to_string(),
            timestamp: ts,
            tool_calls: None,
            tool_call_id: None,
        }
    }

    #[test]
    fn search_messages_round_trips_across_sessions() -> Result<()> {
        let root = std::env::temp_dir()
            .join(format!("swarmclaw-fts-search-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root)?;
        let store_path = root.join("sessions.sqlite");

        // Session A: talks about kubernetes deployment.
        persist_message(&store_path, "sess-a", 0, &msg(Role::User, "How do I configure a kubernetes deployment manifest?", 1))?;
        persist_message(&store_path, "sess-a", 1, &msg(Role::Assistant, "Use a Deployment YAML with replicas.", 2))?;
        // Session B: talks about something unrelated (pancakes).
        persist_message(&store_path, "sess-b", 0, &msg(Role::User, "What is the best pancake recipe?", 3))?;

        // Search for a term only present in session A.
        let hits = search_messages(&store_path, "kubernetes deployment", 5, None)?;
        assert!(!hits.is_empty(), "expected at least one kubernetes hit");
        assert!(
            hits.iter().all(|h| h.message.content.to_lowercase().contains("deployment")
                || h.message.content.to_lowercase().contains("kubernetes")),
            "all hits should be relevant: {:?}",
            hits.iter().map(|h| &h.message.content).collect::<Vec<_>>()
        );

        // limit is respected.
        let limited = search_messages(&store_path, "kubernetes deployment", 1, None)?;
        assert_eq!(limited.len(), 1);

        // exclude_session filters out the excluded session entirely.
        let excluded = search_messages(&store_path, "kubernetes deployment", 5, Some("sess-a"))?;
        assert!(
            excluded.is_empty(),
            "excluding sess-a should drop all kubernetes hits: {:?}",
            excluded.iter().map(|h| &h.message.content).collect::<Vec<_>>()
        );

        // A term that exists only in session B.
        let pancakes = search_messages(&store_path, "pancake", 5, None)?;
        assert!(pancakes.iter().any(|h| h.message.content.contains("pancake")));

        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn search_messages_no_match_is_empty() -> Result<()> {
        let root = std::env::temp_dir()
            .join(format!("swarmclaw-fts-nomatch-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root)?;
        let store_path = root.join("sessions.sqlite");

        persist_message(&store_path, "sess-a", 0, &msg(Role::User, "hello there general", 1))?;

        let hits = search_messages(&store_path, "zznonexistentterm", 5, None)?;
        assert!(hits.is_empty(), "no documents should match a nonsense term");

        // Empty / garbage query -> empty.
        assert!(search_messages(&store_path, "", 5, None)?.is_empty());
        assert!(search_messages(&store_path, "!!!", 5, None)?.is_empty());

        // Nonexistent store -> empty, no error.
        let missing = root.join("does-not-exist.sqlite");
        assert!(search_messages(&missing, "hello", 5, None)?.is_empty());

        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}
