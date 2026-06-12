//! Role-change endpoint + /settings/team page/partial flows.
//!
//! Run via:
//!     DATABASE_URL=postgres://monitor:monitor@localhost:5432/monitor \
//!         cargo test --test team_api_test -- --ignored

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use tower::ServiceExt;
use uptimepage::domain::{OrgId, Role, UserId, generate_signup_slug};
use uptimepage::storage::orgs::{self as orgs_store, create_signup_org_with_owner_in_tx};
use uuid::Uuid;

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations/postgres");

async fn fresh_pg() -> Option<(String, String)> {
    common::fresh_test_db("team_api").await
}

async fn open_pool(db_url: &str) -> sqlx::PgPool {
    common::open_test_pool(db_url).await
}

async fn seed_user(pool: &sqlx::PgPool, email: &str) -> UserId {
    let (id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO users (email, terms_version, privacy_version, email_verified_at) \
         VALUES ($1, 'v1', 'v1', now()) RETURNING id",
    )
    .bind(email)
    .fetch_one(pool)
    .await
    .unwrap();
    UserId(id)
}

async fn seed_org(pool: &sqlx::PgPool, owner: UserId) -> OrgId {
    let mut tx = pool.begin().await.unwrap();
    let org = loop {
        let slug = generate_signup_slug();
        if let Some(o) = create_signup_org_with_owner_in_tx(&mut tx, owner, &slug, "T")
            .await
            .unwrap()
        {
            break o;
        }
    };
    tx.commit().await.unwrap();
    org
}

async fn add_member(pool: &sqlx::PgPool, org: OrgId, user: UserId, role: &str) {
    sqlx::query("INSERT INTO memberships (org_id, user_id, role) VALUES ($1, $2, $3)")
        .bind(org.0)
        .bind(user.0)
        .bind(role)
        .execute(pool)
        .await
        .unwrap();
}

async fn member_role(pool: &sqlx::PgPool, org: OrgId, user: UserId) -> Option<String> {
    sqlx::query_scalar("SELECT role FROM memberships WHERE org_id = $1 AND user_id = $2")
        .bind(org.0)
        .bind(user.0)
        .fetch_optional(pool)
        .await
        .unwrap()
}

async fn audit_count(pool: &sqlx::PgPool, org: OrgId, action: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM org_audit_log WHERE org_id = $1 AND action = $2")
        .bind(org.0)
        .bind(action)
        .fetch_one(pool)
        .await
        .unwrap()
}

fn patch_role(org: OrgId, target: UserId, role: &str) -> Request<Body> {
    Request::builder()
        .method("PATCH")
        .uri(format!("/api/v1/orgs/{}/members/{}", org.0, target.0))
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-requested-with", "uptimepage")
        .body(Body::from(format!(r#"{{"role":"{role}"}}"#)))
        .unwrap()
}

async fn send(app: &axum::Router, req: Request<Body>) -> StatusCode {
    app.clone().oneshot(req).await.unwrap().status()
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn role_change_round_trip_with_audit() {
    let Some((db, name)) = fresh_pg().await else {
        return;
    };
    let pool = open_pool(&db).await;
    MIGRATOR.run(&pool).await.unwrap();
    let owner = seed_user(&pool, "own1@team.test").await;
    let org = seed_org(&pool, owner).await;
    let member = seed_user(&pool, "mem1@team.test").await;
    add_member(&pool, org, member, "member").await;

    let (app, _d) = common::build_test_app_with_pg(pool.clone(), |_| {}).await;
    let app = common::with_session(app, owner, Some(org), None);

    // Promote.
    assert_eq!(
        send(&app, patch_role(org, member, "owner")).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        member_role(&pool, org, member).await.as_deref(),
        Some("owner")
    );
    assert_eq!(audit_count(&pool, org, "member.role_changed").await, 1);

    // Demote back (two owners exist, allowed).
    assert_eq!(
        send(&app, patch_role(org, member, "member")).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        member_role(&pool, org, member).await.as_deref(),
        Some("member")
    );
    assert_eq!(audit_count(&pool, org, "member.role_changed").await, 2);

    // No-op: same role → 204, no extra audit row.
    assert_eq!(
        send(&app, patch_role(org, member, "member")).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(audit_count(&pool, org, "member.role_changed").await, 2);

    common::drop_test_db(&name).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn sole_owner_demote_is_conflict() {
    let Some((db, name)) = fresh_pg().await else {
        return;
    };
    let pool = open_pool(&db).await;
    MIGRATOR.run(&pool).await.unwrap();
    let owner = seed_user(&pool, "own2@team.test").await;
    let org = seed_org(&pool, owner).await;

    let (app, _d) = common::build_test_app_with_pg(pool.clone(), |_| {}).await;
    let app = common::with_session(app, owner, Some(org), None);
    assert_eq!(
        send(&app, patch_role(org, owner, "member")).await,
        StatusCode::CONFLICT
    );
    assert_eq!(
        member_role(&pool, org, owner).await.as_deref(),
        Some("owner")
    );

    common::drop_test_db(&name).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn self_demote_with_second_owner_is_allowed() {
    let Some((db, name)) = fresh_pg().await else {
        return;
    };
    let pool = open_pool(&db).await;
    MIGRATOR.run(&pool).await.unwrap();
    let owner = seed_user(&pool, "own3@team.test").await;
    let org = seed_org(&pool, owner).await;
    let second = seed_user(&pool, "own3b@team.test").await;
    add_member(&pool, org, second, "owner").await;

    let (app, _d) = common::build_test_app_with_pg(pool.clone(), |_| {}).await;
    let app = common::with_session(app, owner, Some(org), None);
    assert_eq!(
        send(&app, patch_role(org, owner, "member")).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        member_role(&pool, org, owner).await.as_deref(),
        Some("member")
    );

    common::drop_test_db(&name).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn member_caller_forbidden_and_unknown_target_404() {
    let Some((db, name)) = fresh_pg().await else {
        return;
    };
    let pool = open_pool(&db).await;
    MIGRATOR.run(&pool).await.unwrap();
    let owner = seed_user(&pool, "own4@team.test").await;
    let org = seed_org(&pool, owner).await;
    let member = seed_user(&pool, "mem4@team.test").await;
    add_member(&pool, org, member, "member").await;
    let stranger = seed_user(&pool, "ghost4@team.test").await;

    let (app, _d) = common::build_test_app_with_pg(pool.clone(), |_| {}).await;

    // Member caller → 403 (require_owner).
    let as_member = common::with_session(app.clone(), member, Some(org), None);
    assert_eq!(
        send(&as_member, patch_role(org, owner, "member")).await,
        StatusCode::FORBIDDEN
    );

    // Owner targeting a non-member → 404.
    let as_owner = common::with_session(app, owner, Some(org), None);
    assert_eq!(
        send(&as_owner, patch_role(org, stranger, "owner")).await,
        StatusCode::NOT_FOUND
    );

    common::drop_test_db(&name).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn bad_role_value_is_client_error() {
    let Some((db, name)) = fresh_pg().await else {
        return;
    };
    let pool = open_pool(&db).await;
    MIGRATOR.run(&pool).await.unwrap();
    let owner = seed_user(&pool, "own5@team.test").await;
    let org = seed_org(&pool, owner).await;

    let (app, _d) = common::build_test_app_with_pg(pool.clone(), |_| {}).await;
    let app = common::with_session(app, owner, Some(org), None);
    let status = send(&app, patch_role(org, owner, "superadmin")).await;
    assert!(status.is_client_error(), "got {status}");

    common::drop_test_db(&name).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn storage_set_member_role_outcomes() {
    let Some((db, name)) = fresh_pg().await else {
        return;
    };
    let pool = open_pool(&db).await;
    MIGRATOR.run(&pool).await.unwrap();
    let owner = seed_user(&pool, "own6@team.test").await;
    let org = seed_org(&pool, owner).await;
    let member = seed_user(&pool, "mem6@team.test").await;
    add_member(&pool, org, member, "member").await;
    let stranger = seed_user(&pool, "ghost6@team.test").await;

    use orgs_store::SetRoleOutcome::*;
    assert_eq!(
        orgs_store::set_member_role(&pool, org, owner, member, Role::Owner)
            .await
            .unwrap(),
        Updated
    );
    assert_eq!(
        orgs_store::set_member_role(&pool, org, owner, member, Role::Owner)
            .await
            .unwrap(),
        Unchanged
    );
    assert_eq!(
        orgs_store::set_member_role(&pool, org, owner, stranger, Role::Owner)
            .await
            .unwrap(),
        NotFound
    );
    // Demote both owners one by one — second demotion must hit LastOwner.
    assert_eq!(
        orgs_store::set_member_role(&pool, org, owner, member, Role::Member)
            .await
            .unwrap(),
        Updated
    );
    assert_eq!(
        orgs_store::set_member_role(&pool, org, owner, owner, Role::Member)
            .await
            .unwrap(),
        LastOwner
    );

    common::drop_test_db(&name).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn team_page_and_partial_render_by_role() {
    let Some((db, name)) = fresh_pg().await else {
        return;
    };
    let pool = open_pool(&db).await;
    MIGRATOR.run(&pool).await.unwrap();
    let owner = seed_user(&pool, "own7@team.test").await;
    let org = seed_org(&pool, owner).await;
    let member = seed_user(&pool, "mem7@team.test").await;
    add_member(&pool, org, member, "member").await;

    let (app, _d) = common::build_test_app_with_pg(pool.clone(), |_| {}).await;

    let get = |app: &axum::Router, uri: &str| {
        let app = app.clone();
        let uri = uri.to_string();
        async move {
            let resp = app
                .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            let status = resp.status();
            let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
                .await
                .unwrap();
            (status, String::from_utf8_lossy(&bytes).to_string())
        }
    };

    let as_owner = common::with_session(app.clone(), owner, Some(org), None);
    let (status, body) = get(&as_owner, "/settings/team").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains(r#"id="invite-form""#));
    let (status, body) = get(&as_owner, "/web/partials/settings/team").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("mem7@team.test"));
    assert!(body.contains("you"));
    assert!(body.contains("# no pending invitations"));

    let as_member = common::with_session(app, member, Some(org), None);
    let (status, body) = get(&as_member, "/settings/team").await;
    assert_eq!(status, StatusCode::OK);
    assert!(!body.contains(r#"id="invite-form""#));
    assert!(body.contains("owners manage the team"));
    let (status, _) = get(&as_member, "/web/partials/settings/team").await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    common::drop_test_db(&name).await;
}
