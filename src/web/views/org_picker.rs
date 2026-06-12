//! Nav org picker — htmx partial. Renders only for multi-org users so the
//! single-org nav stays unchanged; mutations go through the JSON API.

use askama::Template;
use askama_web::WebTemplate;
use axum::extract::State;
use axum::response::{IntoResponse, Response};

use crate::app::AppState;
use crate::storage::orgs as orgs_store;
use crate::web::Session;
use crate::web::error::WebResult;

#[derive(Template, WebTemplate)]
#[template(path = "nav/org_picker.html")]
pub struct OrgPickerPartial {
    pub current_slug: String,
    pub orgs: Vec<PickerOrg>,
}

pub struct PickerOrg {
    pub id: String,
    pub slug: String,
    pub is_active: bool,
}

pub async fn partial(State(state): State<AppState>, session: Session) -> WebResult<Response> {
    // The placeholder also renders on session-less base.html pages (error
    // pages, expired sessions): quiet empty body, never an error payload.
    let Some(user) = session.user.as_ref() else {
        return Ok(().into_response());
    };
    let (Some(active), Some(pool)) = (session.active_org_id, state.db.as_ref()) else {
        return Ok(().into_response());
    };
    let rows = orgs_store::list_orgs_for_user(pool, user.id).await?;
    let current = rows.iter().find(|r| r.org.id == active);
    // Active org gone (removed/org deleted mid-session): every page 403s,
    // so the picker is the only escape hatch — render it for ANY remaining
    // org, not just multi-org users.
    let show = match current {
        Some(_) => rows.len() >= 2,
        None => !rows.is_empty(),
    };
    if !show {
        return Ok(().into_response());
    }
    Ok(OrgPickerPartial {
        current_slug: current.map_or_else(|| "—".to_string(), |r| r.org.slug.clone()),
        orgs: rows
            .iter()
            .map(|r| PickerOrg {
                id: r.org.id.0.to_string(),
                slug: r.org.slug.clone(),
                is_active: r.org.id == active,
            })
            .collect(),
    }
    .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picker_renders_all_orgs_and_marks_active() {
        let html = OrgPickerPartial {
            current_slug: "acme".into(),
            orgs: vec![
                PickerOrg {
                    id: "00000000-0000-0000-0000-000000000001".into(),
                    slug: "acme".into(),
                    is_active: true,
                },
                PickerOrg {
                    id: "00000000-0000-0000-0000-000000000002".into(),
                    slug: "client-co".into(),
                    is_active: false,
                },
            ],
        }
        .render()
        .unwrap();
        assert!(html.contains("data-org-picker"));
        assert!(html.contains("client-co"));
        assert!(html.contains(r#"data-org-switch="00000000-0000-0000-0000-000000000002""#));
        // Exactly one option carries the active marker.
        assert_eq!(html.matches(r#"aria-current="true""#).count(), 1);
        assert!(html.contains(r#"aria-expanded="false""#));
    }
}
