async fn deadline(resp: hyper::Response<hyper::body::Incoming>, at: tokio::time::Instant) {
    let _ = tokio::time::timeout_at(at, Limited::new(resp.into_body(), 1024).collect()).await;
}

async fn remaining(resp: hyper::Response<hyper::body::Incoming>, left: std::time::Duration) {
    let _ = tokio::time::timeout(left, Limited::new(resp.into_body(), 1024).collect()).await;
}
