//! The dashboard's alert subsystem: a generic model and renderer, always
//! present regardless of which producers are compiled in. Producers (e.g. the
//! feature-gated Nebula certificate-expiry watcher in [`crate::nebula`]) fill an
//! [`AlertsSnapshot`] in the background; the request path only ever renders it.

use crate::config::Config;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AlertLevel {
    Info,
    Warning,
    Critical,
}

impl AlertLevel {
    fn label(self) -> &'static str {
        match self {
            AlertLevel::Info => "Info",
            AlertLevel::Warning => "Warning",
            AlertLevel::Critical => "Critical",
        }
    }

    fn css_class(self) -> &'static str {
        match self {
            AlertLevel::Info => "alert-info",
            AlertLevel::Warning => "alert-warning",
            AlertLevel::Critical => "alert-critical",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Alert {
    pub level: AlertLevel,
    pub title: String,
    pub detail: String,
    pub hosts: Vec<String>,
}

#[derive(Debug, Default)]
pub struct AlertsSnapshot {
    pub alerts: Vec<Alert>,
}

/// Render the alerts section, or the empty string when there is nothing to show.
pub fn render_alerts_section(alerts: &[Alert], config: &Config) -> String {
    if alerts.is_empty() {
        return String::new();
    }

    let mut html = String::from(
        r#"<div class="alerts-section"><div class="section-header"><h2 class="section-title">Alerts</h2></div>"#,
    );

    for alert in alerts {
        let badges: String = alert
            .hosts
            .iter()
            .map(|host| {
                let clean = match &config.hostname_strip_suffix {
                    Some(suffix) => host.replace(suffix.as_str(), ""),
                    None => host.clone(),
                };
                format!(r#"<span class="host-badge">{}</span>"#, esc(&clean))
            })
            .collect();

        html.push_str(&format!(
            r#"<div class="alert {css}">
                <span class="alert-level">{level}</span>
                <div class="alert-body">
                    <div class="alert-title">{title}</div>
                    <div class="alert-detail">{detail}</div>
                    <div class="host-badges">{badges}</div>
                </div>
            </div>"#,
            css = alert.level.css_class(),
            level = alert.level.label(),
            title = esc(&alert.title),
            detail = esc(&alert.detail),
        ));
    }

    html.push_str("</div>");
    html
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
