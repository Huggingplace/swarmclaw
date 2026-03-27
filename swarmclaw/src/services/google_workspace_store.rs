use aes_gcm::{
    aead::{rand_core::RngCore, Aead, AeadCore, KeyInit, OsRng},
    Aes256Gcm,
};
use anyhow::{bail, Context, Result};
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, Database, DatabaseConnection, DbBackend,
    EntityTrait, QueryFilter, QueryOrder, Set, Statement,
};
use std::{
    fs,
    path::{Path, PathBuf},
};
use uuid::Uuid;

#[derive(Clone)]
pub(crate) struct GoogleWorkspaceStore {
    db: DatabaseConnection,
    key_path: PathBuf,
}

#[derive(Debug, Clone)]
pub(crate) struct StoredGoogleAccount {
    pub refresh_token: String,
    pub scope: String,
    pub account_email: Option<String>,
    pub account_name: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct StoredGoogleSheetBinding {
    pub id: String,
    pub alias: String,
    pub spreadsheet_id: String,
    pub spreadsheet_title: String,
    pub sheet_titles: Vec<String>,
    pub allowed_tabs: Vec<String>,
    pub allowed_ranges: Vec<String>,
}

impl GoogleWorkspaceStore {
    pub(crate) async fn open(store_root: &Path) -> Result<Self> {
        fs::create_dir_all(store_root).with_context(|| {
            format!(
                "Failed to create SwarmClaw data directory at {:?}",
                store_root
            )
        })?;

        let key_path = store_root.join("google_workspace.key");
        let db_path = store_root.join("google_workspace.sqlite");

        let store = Self {
            db: Database::connect(sqlite_url(&db_path)?)
                .await
                .with_context(|| {
                    format!("Failed to open Google Workspace database at {:?}", db_path)
                })?,
            key_path,
        };
        store.init().await?;
        Ok(store)
    }

    async fn init(&self) -> Result<()> {
        self.ensure_key_file()?;
        self.db
            .execute(Statement::from_string(
                DbBackend::Sqlite,
                r#"
                CREATE TABLE IF NOT EXISTS google_workspace_account (
                    id INTEGER PRIMARY KEY NOT NULL,
                    encrypted_refresh_token BLOB NOT NULL,
                    scope TEXT NOT NULL,
                    account_email TEXT,
                    account_name TEXT,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                )
                "#
                .to_owned(),
            ))
            .await?;
        self.db
            .execute(Statement::from_string(
                DbBackend::Sqlite,
                r#"
                CREATE TABLE IF NOT EXISTS google_oauth_states (
                    state TEXT PRIMARY KEY NOT NULL,
                    created_at TEXT NOT NULL
                )
                "#
                .to_owned(),
            ))
            .await?;
        self.db
            .execute(Statement::from_string(
                DbBackend::Sqlite,
                r#"
                CREATE TABLE IF NOT EXISTS google_sheet_bindings (
                    id TEXT PRIMARY KEY NOT NULL,
                    alias TEXT NOT NULL UNIQUE,
                    spreadsheet_id TEXT NOT NULL UNIQUE,
                    spreadsheet_title TEXT NOT NULL,
                    sheet_titles_json TEXT NOT NULL,
                    allowed_tabs_json TEXT NOT NULL,
                    allowed_ranges_json TEXT NOT NULL,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                )
                "#
                .to_owned(),
            ))
            .await?;
        Ok(())
    }

    pub(crate) async fn create_oauth_state(&self) -> Result<String> {
        let state = Uuid::new_v4().to_string();
        google_oauth_states::ActiveModel {
            state: Set(state.clone()),
            created_at: Set(Utc::now().to_rfc3339()),
        }
        .insert(&self.db)
        .await?;
        Ok(state)
    }

    pub(crate) async fn consume_oauth_state(&self, state: &str) -> Result<bool> {
        let deleted = google_oauth_states::Entity::delete_by_id(state.to_owned())
            .exec(&self.db)
            .await?;
        Ok(deleted.rows_affected > 0)
    }

    pub(crate) async fn upsert_account(&self, account: &StoredGoogleAccount) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let encrypted_refresh_token = self.encrypt(&account.refresh_token)?;

        if let Some(existing) = google_workspace_account::Entity::find_by_id(1)
            .one(&self.db)
            .await?
        {
            let mut model: google_workspace_account::ActiveModel = existing.into();
            model.encrypted_refresh_token = Set(encrypted_refresh_token);
            model.scope = Set(account.scope.clone());
            model.account_email = Set(account.account_email.clone());
            model.account_name = Set(account.account_name.clone());
            model.updated_at = Set(now);
            model.update(&self.db).await?;
        } else {
            google_workspace_account::ActiveModel {
                id: Set(1),
                encrypted_refresh_token: Set(encrypted_refresh_token),
                scope: Set(account.scope.clone()),
                account_email: Set(account.account_email.clone()),
                account_name: Set(account.account_name.clone()),
                created_at: Set(now.clone()),
                updated_at: Set(now),
            }
            .insert(&self.db)
            .await?;
        }

        Ok(())
    }

    pub(crate) async fn account(&self) -> Result<Option<StoredGoogleAccount>> {
        let model = google_workspace_account::Entity::find_by_id(1)
            .one(&self.db)
            .await?;
        model
            .map(|value| {
                Ok(StoredGoogleAccount {
                    refresh_token: self.decrypt(&value.encrypted_refresh_token)?,
                    scope: value.scope,
                    account_email: value.account_email,
                    account_name: value.account_name,
                })
            })
            .transpose()
    }

    pub(crate) async fn save_sheet_binding(
        &self,
        binding: &StoredGoogleSheetBinding,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let alias_match = google_sheet_bindings::Entity::find()
            .filter(google_sheet_bindings::Column::Alias.eq(binding.alias.clone()))
            .one(&self.db)
            .await?;
        let spreadsheet_match = google_sheet_bindings::Entity::find()
            .filter(google_sheet_bindings::Column::SpreadsheetId.eq(binding.spreadsheet_id.clone()))
            .one(&self.db)
            .await?;

        let target = match (alias_match, spreadsheet_match) {
            (Some(alias_binding), Some(spreadsheet_binding))
                if alias_binding.id != spreadsheet_binding.id =>
            {
                bail!(
                    "Alias '{}' and spreadsheet '{}' are already bound to different entries. Remove one before rebinding.",
                    binding.alias,
                    binding.spreadsheet_id
                );
            }
            (Some(existing), _) => Some(existing),
            (None, Some(existing)) => Some(existing),
            (None, None) => None,
        };

        let sheet_titles_json = encode_string_list(&binding.sheet_titles)?;
        let allowed_tabs_json = encode_string_list(&binding.allowed_tabs)?;
        let allowed_ranges_json = encode_string_list(&binding.allowed_ranges)?;

        if let Some(existing) = target {
            let mut model: google_sheet_bindings::ActiveModel = existing.into();
            model.alias = Set(binding.alias.clone());
            model.spreadsheet_id = Set(binding.spreadsheet_id.clone());
            model.spreadsheet_title = Set(binding.spreadsheet_title.clone());
            model.sheet_titles_json = Set(sheet_titles_json);
            model.allowed_tabs_json = Set(allowed_tabs_json);
            model.allowed_ranges_json = Set(allowed_ranges_json);
            model.updated_at = Set(now);
            model.update(&self.db).await?;
        } else {
            google_sheet_bindings::ActiveModel {
                id: Set(binding.id.clone()),
                alias: Set(binding.alias.clone()),
                spreadsheet_id: Set(binding.spreadsheet_id.clone()),
                spreadsheet_title: Set(binding.spreadsheet_title.clone()),
                sheet_titles_json: Set(sheet_titles_json),
                allowed_tabs_json: Set(allowed_tabs_json),
                allowed_ranges_json: Set(allowed_ranges_json),
                created_at: Set(now.clone()),
                updated_at: Set(now),
            }
            .insert(&self.db)
            .await?;
        }

        Ok(())
    }

    pub(crate) async fn list_sheet_bindings(&self) -> Result<Vec<StoredGoogleSheetBinding>> {
        google_sheet_bindings::Entity::find()
            .order_by_asc(google_sheet_bindings::Column::Alias)
            .all(&self.db)
            .await?
            .into_iter()
            .map(stored_binding_from_model)
            .collect()
    }

    pub(crate) async fn find_sheet_binding_by_alias(
        &self,
        alias: &str,
    ) -> Result<Option<StoredGoogleSheetBinding>> {
        google_sheet_bindings::Entity::find()
            .filter(google_sheet_bindings::Column::Alias.eq(alias))
            .one(&self.db)
            .await?
            .map(stored_binding_from_model)
            .transpose()
    }

    pub(crate) async fn delete_sheet_binding(&self, binding_id: &str) -> Result<bool> {
        let deleted = google_sheet_bindings::Entity::delete_by_id(binding_id.to_owned())
            .exec(&self.db)
            .await?;
        Ok(deleted.rows_affected > 0)
    }

    fn ensure_key_file(&self) -> Result<()> {
        if self.key_path.exists() {
            return Ok(());
        }

        let mut key = [0u8; 32];
        OsRng.fill_bytes(&mut key);
        fs::write(&self.key_path, hex::encode(key)).with_context(|| {
            format!(
                "Failed to write Google Workspace key file to {:?}",
                self.key_path
            )
        })?;
        Ok(())
    }

    fn encryption_key(&self) -> Result<[u8; 32]> {
        let raw = fs::read_to_string(&self.key_path).with_context(|| {
            format!(
                "Failed to read Google Workspace key file at {:?}",
                self.key_path
            )
        })?;
        let decoded =
            hex::decode(raw.trim()).context("Failed to decode stored Google Workspace key file")?;
        if decoded.len() != 32 {
            bail!("Google Workspace key file must decode to 32 bytes");
        }

        let mut key = [0u8; 32];
        key.copy_from_slice(&decoded);
        Ok(key)
    }

    fn encrypt(&self, value: &str) -> Result<Vec<u8>> {
        let key = self.encryption_key()?;
        let cipher = Aes256Gcm::new_from_slice(&key)
            .context("Failed to initialize Google Workspace cipher")?;
        let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        let ciphertext = cipher
            .encrypt(&nonce, value.as_bytes())
            .context("Failed to encrypt Google Workspace secret")?;

        let mut payload = nonce.to_vec();
        payload.extend_from_slice(&ciphertext);
        Ok(payload)
    }

    fn decrypt(&self, value: &[u8]) -> Result<String> {
        let key = self.encryption_key()?;
        if value.len() < 12 {
            bail!("Encrypted Google Workspace secret is too short");
        }

        let cipher = Aes256Gcm::new_from_slice(&key)
            .context("Failed to initialize Google Workspace cipher")?;
        let (nonce, ciphertext) = value.split_at(12);
        let plaintext = cipher
            .decrypt(nonce.into(), ciphertext)
            .context("Failed to decrypt Google Workspace secret")?;
        String::from_utf8(plaintext).context("Google Workspace secret is not valid UTF-8")
    }
}

fn stored_binding_from_model(
    model: google_sheet_bindings::Model,
) -> Result<StoredGoogleSheetBinding> {
    Ok(StoredGoogleSheetBinding {
        id: model.id,
        alias: model.alias,
        spreadsheet_id: model.spreadsheet_id,
        spreadsheet_title: model.spreadsheet_title,
        sheet_titles: decode_string_list(&model.sheet_titles_json)?,
        allowed_tabs: decode_string_list(&model.allowed_tabs_json)?,
        allowed_ranges: decode_string_list(&model.allowed_ranges_json)?,
    })
}

fn encode_string_list(values: &[String]) -> Result<String> {
    serde_json::to_string(values).context("Failed to encode Google Workspace string list")
}

fn decode_string_list(raw: &str) -> Result<Vec<String>> {
    serde_json::from_str(raw)
        .with_context(|| format!("Failed to decode Google Workspace string list: {}", raw))
}

fn sqlite_url(path: &Path) -> Result<String> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .context("Failed to resolve current working directory for SQLite URL")?
            .join(path)
    };
    Ok(format!("sqlite://{}?mode=rwc", path.display()))
}

mod google_workspace_account {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "google_workspace_account")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: i32,
        pub encrypted_refresh_token: Vec<u8>,
        pub scope: String,
        pub account_email: Option<String>,
        pub account_name: Option<String>,
        pub created_at: String,
        pub updated_at: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

mod google_oauth_states {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "google_oauth_states")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub state: String,
        pub created_at: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

mod google_sheet_bindings {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "google_sheet_bindings")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: String,
        pub alias: String,
        pub spreadsheet_id: String,
        pub spreadsheet_title: String,
        pub sheet_titles_json: String,
        pub allowed_tabs_json: String,
        pub allowed_ranges_json: String,
        pub created_at: String,
        pub updated_at: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}
