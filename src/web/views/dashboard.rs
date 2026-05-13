use askama::Template;
use askama_web::WebTemplate;

#[derive(Template, WebTemplate)]
#[template(path = "dashboard.html")]
pub struct DashboardPage {
    pub active_tab: &'static str,
}

pub async fn index() -> DashboardPage {
    DashboardPage {
        active_tab: "dashboard",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_valid_html() {
        let html = DashboardPage {
            active_tab: "dashboard",
        }
        .render()
        .expect("render");
        assert!(html.starts_with("<!doctype html>"));
        assert!(html.contains("Status Monitor"));
        assert!(html.contains("Dashboard"));
        assert!(html.contains(r#"href="/static/css/app.css""#));
        assert!(html.contains(r#"src="/static/js/htmx.min.js""#));
    }
}
