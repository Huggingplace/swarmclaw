use anyhow::{Context, Result};
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ConnectionTrait, Database, DatabaseConnection, EntityTrait, QueryOrder, Set,
    Statement,
};
use std::path::Path;

#[derive(Clone)]
pub struct ControlPlaneStore {
    db: DatabaseConnection,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ChannelRegistration {
    pub platform: String,
    pub transport: String,
    pub endpoint: String,
    pub enabled: bool,
    pub updated_at: String,
}

impl ControlPlaneStore {
    pub async fn open(store_root: &Path) -> Result<Self> {
        std::fs::create_dir_all(store_root).with_context(|| {
            format!(
                "Failed to create SwarmClaw control plane directory at {}",
                store_root.display()
            )
        })?;

        let db_url = if let Ok(value) = std::env::var("SWARMCLAW_CONTROL_PLANE_DATABASE_URL") {
            value
        } else {
            sqlite_url(&store_root.join("control_plane.sqlite"))?
        };

        let store = Self {
            db: Database::connect(db_url).await?,
        };
        store.init().await?;
        Ok(store)
    }

    async fn init(&self) -> Result<()> {
        self.db
            .execute(Statement::from_string(
                self.db.get_database_backend(),
                r#"
                CREATE TABLE IF NOT EXISTS channel_registrations (
                    platform TEXT PRIMARY KEY NOT NULL,
                    transport TEXT NOT NULL,
                    endpoint TEXT NOT NULL,
                    enabled BOOLEAN NOT NULL,
                    updated_at TEXT NOT NULL
                )
                "#
                .to_owned(),
            ))
            .await?;
        Ok(())
    }

    pub async fn upsert_channel_registration(
        &self,
        platform: &str,
        transport: &str,
        endpoint: &str,
        enabled: bool,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        if let Some(existing) = channel_registrations::Entity::find_by_id(platform.to_string())
            .one(&self.db)
            .await?
        {
            let mut model: channel_registrations::ActiveModel = existing.into();
            model.transport = Set(transport.to_string());
            model.endpoint = Set(endpoint.to_string());
            model.enabled = Set(enabled);
            model.updated_at = Set(now);
            model.update(&self.db).await?;
        } else {
            channel_registrations::ActiveModel {
                platform: Set(platform.to_string()),
                transport: Set(transport.to_string()),
                endpoint: Set(endpoint.to_string()),
                enabled: Set(enabled),
                updated_at: Set(now),
            }
            .insert(&self.db)
            .await?;
        }

        Ok(())
    }

    pub async fn list_channel_registrations(&self) -> Result<Vec<ChannelRegistration>> {
        Ok(channel_registrations::Entity::find()
            .order_by_asc(channel_registrations::Column::Platform)
            .all(&self.db)
            .await?
            .into_iter()
            .map(|model| ChannelRegistration {
                platform: model.platform,
                transport: model.transport,
                endpoint: model.endpoint,
                enabled: model.enabled,
                updated_at: model.updated_at,
            })
            .collect())
    }
}

fn sqlite_url(path: &Path) -> Result<String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let display = absolute.to_string_lossy().replace('\\', "/");
    Ok(format!("sqlite://{}?mode=rwc", display))
}

mod channel_registrations {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "channel_registrations")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub platform: String,
        pub transport: String,
        pub endpoint: String,
        pub enabled: bool,
        pub updated_at: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}
