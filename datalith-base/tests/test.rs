mod global;

use global::*;

#[tokio::test]
async fn test() {
    let datalith = datalith_init().await;

    datalith_close(datalith).await;
}
