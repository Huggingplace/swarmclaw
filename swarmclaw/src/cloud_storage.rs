use anyhow::Result;
use object_store::aws::AmazonS3Builder;
use object_store::azure::MicrosoftAzureBuilder;
use object_store::gcp::GoogleCloudStorageBuilder;
use object_store::path::Path;
use object_store::{ObjectStore, ObjectStoreExt};
use std::sync::Arc;

pub enum CloudProvider {
    AwsS3,
    GcpGcs,
    AzureBlob,
}

pub struct CloudStorageManager {
    store: Arc<dyn ObjectStore>,
}

impl CloudStorageManager {
    pub fn new(provider: CloudProvider, bucket_name: &str) -> Result<Self> {
        let store: Arc<dyn ObjectStore> = match provider {
            CloudProvider::AwsS3 => {
                let s3 = AmazonS3Builder::from_env()
                    .with_bucket_name(bucket_name)
                    .build()?;
                Arc::new(s3)
            }
            CloudProvider::GcpGcs => {
                let gcs = GoogleCloudStorageBuilder::from_env()
                    .with_bucket_name(bucket_name)
                    .build()?;
                Arc::new(gcs)
            }
            CloudProvider::AzureBlob => {
                let azure = MicrosoftAzureBuilder::from_env()
                    .with_container_name(bucket_name)
                    .build()?;
                Arc::new(azure)
            }
        };

        Ok(Self { store })
    }

    pub async fn upload_file(&self, remote_path: &str, data: Vec<u8>) -> Result<()> {
        let path = Path::from(remote_path);
        let bytes = bytes::Bytes::from(data);
        self.store.put(&path, bytes.into()).await?;
        Ok(())
    }

    pub async fn download_file(&self, remote_path: &str) -> Result<Vec<u8>> {
        let path = Path::from(remote_path);
        let result = self.store.get(&path).await?;
        let bytes = result.bytes().await?;
        Ok(bytes.to_vec())
    }
}
