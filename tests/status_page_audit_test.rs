//! Status-page create/delete must write an atomic `org_audit_log` row
//! (actor + action + metadata), via record_audit_tx inside the mutation's tx.

mod common;

use uptimepage::domain::{NewStatusPage, StatusPageId, WriteSource};
use uptimepage::storage::{PgStatusPageStore, StatusPageStore, create_org_with_owner};

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations/postgres");

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn create_and_delete_page_write_audit_rows() {
    let Some((db, name)) = common::fresh_test_db("sp_audit").await else {
        return;
    };
    let pool = common::open_test_pool(&db).await;
    MIGRATOR.run(&pool).await.unwrap();

    let user = common::make_user(&pool, "a").await;
    let org = create_org_with_owner(&pool, user, &common::unique_slug("a"), "Co", 10)
        .await
        .unwrap()
        .unwrap();
    let store = PgStatusPageStore::new(pool.clone());

    let page = store
        .create(
            org.id,
            NewStatusPage {
                slug: common::unique_slug("aud"),
                name: "Aud".into(),
                enabled: true,
            },
            WriteSource::Ui,
            10,
            Some(user),
        )
        .await
        .unwrap()
        .unwrap();

    let created: (String, Option<uuid::Uuid>) = sqlx::query_as(
        "SELECT action, actor_id FROM org_audit_log \
         WHERE org_id = $1 AND action = 'status_page.created'",
    )
    .bind(org.id.0)
    .fetch_one(&pool)
    .await
    .expect("create audit row");
    assert_eq!(created.1, Some(user.0), "actor recorded");

    assert!(
        store
            .delete(org.id, StatusPageId(page.id.0), Some(user))
            .await
            .unwrap()
    );
    let (deleted_count,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM org_audit_log \
         WHERE org_id = $1 AND action = 'status_page.deleted'",
    )
    .bind(org.id.0)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(deleted_count, 1, "delete audit row written");

    common::drop_test_db(&name).await;
}
