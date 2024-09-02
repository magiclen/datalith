mod global;

#[cfg(feature = "manager")]
use datalith_core::DatalithManager;
use global::*;

#[tokio::test]
async fn initialize() {
    let datalith = datalith_init().await;

    #[cfg(feature = "manager")]
    DatalithManager::new(datalith.clone()).await.unwrap();

    datalith_close(datalith).await;
}
