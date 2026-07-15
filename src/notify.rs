use crate::config::{Notify, Ses};
use anyhow::Result;

pub enum RunEvent<'a> {
    Started,
    Finished {
        status: &'a str,
        snapshot_id: Option<&'a str>,
        detail: Option<&'a str>,
    },
}

/// Statuses that count as a healthy check-in for the dead-man switch.
/// Everything else, including statuses this version does not know,
/// pings /fail: fail closed.
pub fn is_success_ping(status: &str) -> bool {
    matches!(
        status,
        "success" | "success_retention_failed" | "success_prune_failed" | "verify_passed"
    )
}

/// Statuses that reach humans via webhook and email. verify_passed is
/// included deliberately: it is the spec's scheduled verify report.
pub fn alerts_humans(status: &str) -> bool {
    matches!(
        status,
        "failed"
            | "success_retention_failed"
            | "success_prune_failed"
            | "verify_failed"
            | "verify_passed"
    )
}

/// A successful backup with failed retention still pings healthchecks
/// success: the dead-man switch measures backup freshness and a snapshot
/// exists. The retention problem reaches the human via webhook/email, which
/// DO fire for this partial-success state.
/// Unknown statuses ping /fail: fail closed.
pub fn hc_url(base: &str, uuid: &str, event: &RunEvent) -> String {
    let base = base.trim_end_matches('/');
    match event {
        RunEvent::Started => format!("{base}/{uuid}/start"),
        RunEvent::Finished { status, .. } if is_success_ping(status) => format!("{base}/{uuid}"),
        RunEvent::Finished { .. } => format!("{base}/{uuid}/fail"),
    }
}

pub fn webhook_payload(
    source: &str,
    status: &str,
    snapshot_id: Option<&str>,
    detail: Option<&str>,
) -> serde_json::Value {
    serde_json::json!({
        "app": "vaultkeeper",
        "source": source,
        "status": status,
        "snapshot_id": snapshot_id,
        "detail": detail,
    })
}

pub fn email_subject_body(source: &str, status: &str, detail: Option<&str>) -> (String, String) {
    let subject = format!("[vaultkeeper] {source}: {status}");
    let body = format!(
        "Backup source: {source}\nStatus: {status}\n\n{}",
        crate::util::truncate_marked(detail.unwrap_or("no detail"), 2000)
    );
    (subject, body)
}

pub struct Notifier {
    healthchecks_base: Option<String>,
    webhook_url: Option<String>,
    ses: Option<Ses>,
    client: reqwest::blocking::Client,
}

impl Notifier {
    pub fn from_notify(cfg: &Notify) -> Result<Notifier> {
        Ok(Notifier {
            healthchecks_base: cfg.healthchecks_base.clone(),
            webhook_url: cfg.webhook_url.clone(),
            ses: cfg.ses.clone(),
            client: reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()?,
        })
    }

    pub fn notify(&self, source_name: &str, hc_uuid: Option<&str>, event: &RunEvent) {
        if let (Some(base), Some(uuid)) = (&self.healthchecks_base, hc_uuid) {
            let url = hc_url(base, uuid, event);
            let req = self.client.get(&url);
            let req = if let RunEvent::Finished {
                detail: Some(d), ..
            } = event
            {
                self.client
                    .post(&url)
                    .body(crate::util::truncate_marked(d, 2000))
            } else {
                req
            };
            if let Err(e) = req.send() {
                tracing::warn!("healthchecks ping failed for {source_name}: {e}");
            }
        }
        if let RunEvent::Finished {
            status,
            snapshot_id,
            detail,
        } = event
        {
            if alerts_humans(status) {
                if let Some(url) = &self.webhook_url {
                    let payload = webhook_payload(source_name, status, *snapshot_id, *detail);
                    if let Err(e) = self.client.post(url).json(&payload).send() {
                        tracing::warn!("webhook post failed for {source_name}: {e}");
                    }
                }
                if let Some(ses) = &self.ses {
                    let (subject, body) = email_subject_body(source_name, status, *detail);
                    if let Err(e) = send_ses(ses, &subject, &body) {
                        tracing::warn!("ses email failed for {source_name}: {e}");
                    }
                }
            }
        }
    }
}

fn send_ses(ses: &Ses, subject: &str, body: &str) -> Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        let cfg = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .region(aws_config::Region::new(ses.region.clone()))
            .load()
            .await;
        let client = aws_sdk_sesv2::Client::new(&cfg);
        let dest = aws_sdk_sesv2::types::Destination::builder()
            .set_to_addresses(Some(ses.to.clone()))
            .build();
        let content = aws_sdk_sesv2::types::EmailContent::builder()
            .simple(
                aws_sdk_sesv2::types::Message::builder()
                    .subject(
                        aws_sdk_sesv2::types::Content::builder()
                            .data(subject)
                            .build()?,
                    )
                    .body(
                        aws_sdk_sesv2::types::Body::builder()
                            .text(
                                aws_sdk_sesv2::types::Content::builder()
                                    .data(body)
                                    .build()?,
                            )
                            .build(),
                    )
                    .build(),
            )
            .build();
        client
            .send_email()
            .from_email_address(&ses.from)
            .destination(dest)
            .content(content)
            .send()
            .await?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const B: &str = "https://hc-ping.com";

    #[test]
    fn hc_urls_per_event() {
        assert_eq!(
            hc_url(B, "u1", &RunEvent::Started),
            "https://hc-ping.com/u1/start"
        );
        let ok = RunEvent::Finished {
            status: "success",
            snapshot_id: Some("s"),
            detail: None,
        };
        assert_eq!(hc_url(B, "u1", &ok), "https://hc-ping.com/u1");
        let warn = RunEvent::Finished {
            status: "success_retention_failed",
            snapshot_id: Some("s"),
            detail: Some("d"),
        };
        assert_eq!(hc_url(B, "u1", &warn), "https://hc-ping.com/u1");
        let bad = RunEvent::Finished {
            status: "failed",
            snapshot_id: None,
            detail: Some("boom"),
        };
        assert_eq!(hc_url(B, "u1", &bad), "https://hc-ping.com/u1/fail");
    }

    #[test]
    fn hc_url_trims_trailing_slash() {
        assert_eq!(
            hc_url("https://hc-ping.com/", "u", &RunEvent::Started),
            "https://hc-ping.com/u/start"
        );
    }

    #[test]
    fn webhook_payload_shape() {
        let p = webhook_payload("acme-db", "failed", None, Some("boom"));
        assert_eq!(p["source"], "acme-db");
        assert_eq!(p["status"], "failed");
        assert_eq!(p["snapshot_id"], serde_json::Value::Null);
        assert_eq!(p["detail"], "boom");
        assert_eq!(p["app"], "vaultkeeper");
    }

    #[test]
    fn email_subject_names_source_and_status_and_truncates() {
        let long = "x".repeat(3000);
        let (subject, body) = email_subject_body("acme-db", "failed", Some(&long));
        assert!(subject.contains("acme-db"));
        assert!(subject.contains("failed"));
        assert!(body.contains(" ...[truncated]"));
    }

    #[test]
    fn unknown_status_pings_fail_closed() {
        let ev = RunEvent::Finished {
            status: "exploded_weirdly",
            snapshot_id: None,
            detail: None,
        };
        assert_eq!(hc_url(B, "u1", &ev), "https://hc-ping.com/u1/fail");
    }

    #[test]
    fn verify_statuses_route_correctly() {
        let pass = RunEvent::Finished {
            status: "verify_passed",
            snapshot_id: None,
            detail: Some("tables=3"),
        };
        assert_eq!(hc_url(B, "u1", &pass), "https://hc-ping.com/u1");
        let fail = RunEvent::Finished {
            status: "verify_failed",
            snapshot_id: None,
            detail: Some("no tables"),
        };
        assert_eq!(hc_url(B, "u1", &fail), "https://hc-ping.com/u1/fail");
    }

    #[test]
    fn alert_gating_per_status() {
        assert!(alerts_humans("failed"));
        assert!(alerts_humans("success_retention_failed"));
        // Rows written by v0.1.0 retain their original status semantics.
        assert!(alerts_humans("success_prune_failed"));
        assert!(alerts_humans("verify_failed"));
        assert!(alerts_humans("verify_passed"));
        assert!(!alerts_humans("success"));
        assert!(!alerts_humans("stale"));
    }
}
