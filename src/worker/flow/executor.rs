//! Runs a flow's step list against a live page. Every action is driven through
//! `Runtime.evaluate` JS rather than CDP input primitives: on the Lightpanda
//! engine, CDP key-typing does not populate `input.value` and a CDP click on a
//! submit button does not submit — but `el.value = ...` + dispatched events and
//! `el.click()` in page JS do both reliably.

use std::time::{Duration, Instant};

use chromiumoxide::Page;

use crate::domain::FlowStep;

/// Terminal result of running the step list.
pub enum RunResult {
    Passed,
    /// A step legitimately failed (selector missing, assertion false, wait
    /// timed out): the monitored flow is down. `step` is 0-based.
    Failed {
        step: usize,
        op: &'static str,
        reason: String,
    },
    /// The engine/CDP transport broke: not a verdict on the target, an error.
    Engine(String),
}

/// A step that did not pass: a flow failure, or a transport break.
enum StepError {
    Failed(String),
    Engine(String),
}

pub async fn run_steps(page: &Page, steps: &[FlowStep], step_timeout: Duration) -> RunResult {
    for (i, step) in steps.iter().enumerate() {
        match drive(page, step, step_timeout).await {
            Ok(()) => {}
            Err(StepError::Failed(reason)) => {
                return RunResult::Failed {
                    step: i,
                    op: step_op(step),
                    reason,
                };
            }
            Err(StepError::Engine(e)) => return RunResult::Engine(e),
        }
    }
    RunResult::Passed
}

fn step_op(step: &FlowStep) -> &'static str {
    match step {
        FlowStep::Goto { .. } => "goto",
        FlowStep::Click { .. } => "click",
        FlowStep::Fill { .. } => "fill",
        FlowStep::WaitFor { .. } => "wait_for",
        FlowStep::AssertText { .. } => "assert_text",
        FlowStep::AssertUrl { .. } => "assert_url",
    }
}

async fn drive(page: &Page, step: &FlowStep, step_timeout: Duration) -> Result<(), StepError> {
    match step {
        FlowStep::Goto { url } => {
            page.goto(url.as_str())
                .await
                .map_err(|e| StepError::Engine(format!("goto: {e}")))?;
            page.wait_for_navigation()
                .await
                .map_err(|e| StepError::Engine(format!("goto nav: {e}")))?;
            Ok(())
        }
        FlowStep::Fill { selector, value } => {
            match eval_string(page, &js_fill(selector, value)).await? {
                s if s == "OK" => Ok(()),
                _ => Err(StepError::Failed(format!("selector not found: {selector}"))),
            }
        }
        FlowStep::Click { selector } => match eval_string(page, &js_click(selector)).await? {
            s if s == "OK" => Ok(()),
            _ => Err(StepError::Failed(format!("selector not found: {selector}"))),
        },
        FlowStep::WaitFor { selector } => {
            let js = js_present(selector);
            if poll(step_timeout, || eval_bool(page, &js)).await? {
                Ok(())
            } else {
                Err(StepError::Failed(format!(
                    "timed out waiting for {selector}"
                )))
            }
        }
        FlowStep::AssertText { selector, contains } => {
            let js = js_text(selector.as_deref());
            let want = contains.clone();
            let found = poll(step_timeout, || async {
                Ok(eval_string(page, &js).await?.contains(&want))
            })
            .await?;
            if found {
                Ok(())
            } else {
                Err(StepError::Failed(format!("text not found: {contains:?}")))
            }
        }
        FlowStep::AssertUrl { contains } => {
            let want = contains.clone();
            let found = poll(step_timeout, || async {
                let url = page
                    .url()
                    .await
                    .map_err(|e| StepError::Engine(format!("url: {e}")))?
                    .unwrap_or_default();
                Ok(url.contains(&want))
            })
            .await?;
            if found {
                Ok(())
            } else {
                Err(StepError::Failed(format!(
                    "url does not contain {contains:?}"
                )))
            }
        }
    }
}

/// Poll `check` every 100ms until it is true or `deadline` elapses.
async fn poll<F, Fut>(deadline: Duration, mut check: F) -> Result<bool, StepError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<bool, StepError>>,
{
    let start = Instant::now();
    loop {
        if check().await? {
            return Ok(true);
        }
        if start.elapsed() >= deadline {
            return Ok(false);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn eval_string(page: &Page, js: &str) -> Result<String, StepError> {
    let v = page
        .evaluate(js)
        .await
        .map_err(|e| StepError::Engine(format!("evaluate: {e}")))?;
    Ok(v.into_value::<Option<String>>()
        .map_err(|e| StepError::Engine(format!("evaluate decode: {e}")))?
        .unwrap_or_default())
}

async fn eval_bool(page: &Page, js: &str) -> Result<bool, StepError> {
    let v = page
        .evaluate(js)
        .await
        .map_err(|e| StepError::Engine(format!("evaluate: {e}")))?;
    Ok(v.into_value::<Option<bool>>()
        .map_err(|e| StepError::Engine(format!("evaluate decode: {e}")))?
        .unwrap_or(false))
}

// JS builders. Selector and value are JSON-encoded into the source so a hostile
// selector or credential can never break out of its string literal.
fn js_fill(selector: &str, value: &str) -> String {
    format!(
        "(function(){{const e=document.querySelector({s});if(!e)return 'NOT_FOUND';\
         e.value={v};e.dispatchEvent(new Event('input',{{bubbles:true}}));\
         e.dispatchEvent(new Event('change',{{bubbles:true}}));return 'OK';}})()",
        s = enc(selector),
        v = enc(value),
    )
}

fn js_click(selector: &str) -> String {
    format!(
        "(function(){{const e=document.querySelector({s});if(!e)return 'NOT_FOUND';\
         e.click();return 'OK';}})()",
        s = enc(selector),
    )
}

fn js_present(selector: &str) -> String {
    format!("!!document.querySelector({s})", s = enc(selector))
}

fn js_text(selector: Option<&str>) -> String {
    match selector {
        Some(s) => format!(
            "(function(){{const e=document.querySelector({s});return e?e.textContent:null;}})()",
            s = enc(s)
        ),
        None => "document.body?document.body.textContent:null".to_string(),
    }
}

fn enc(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_encodes_selector_and_value_safely() {
        // A value containing quotes/backslashes must stay inside its literal.
        let js = js_fill("#pw", "a\"b\\c</script>");
        assert!(js.contains(r##""#pw""##));
        assert!(js.contains(r#""a\"b\\c</script>""#));
        assert!(js.contains("dispatchEvent(new Event('input'"));
    }

    #[test]
    fn click_and_present_and_text_target_the_selector() {
        assert!(js_click("#go").contains(r##"querySelector("#go")"##));
        assert!(js_present("#x").starts_with("!!document.querySelector("));
        assert!(js_text(Some(".flash")).contains(r#"querySelector(".flash")"#));
        assert_eq!(
            js_text(None),
            "document.body?document.body.textContent:null"
        );
    }

    #[test]
    fn selector_quote_is_escaped_exactly() {
        // Exact output proves a quote in the selector is escaped, so it stays
        // one string literal and cannot break out into executable code.
        assert_eq!(js_present("#a"), "!!document.querySelector(\"#a\")");
        assert_eq!(js_present("a\"b"), "!!document.querySelector(\"a\\\"b\")");
    }
}
