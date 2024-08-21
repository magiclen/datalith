mod global;

use global::*;

#[tokio::test]
async fn initialize() {
    let datalith = datalith_init().await;

    datalith_close(datalith).await;
}
