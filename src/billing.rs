//! `mn invoices`, `mn plan`, `mn limits` — billing, plan info, and quotas.
//!
//! Every number here is one the control plane actually sends: the credit
//! summary (`GET /api/v1/credits`) and the tenant's own org (`GET /api/v1/org`).
//! There is no invoice and no price-list route — the views used to be built from
//! `/billing` and `/pricing`, which this service has never served — so the
//! invoice view says where invoices live rather than rendering a shape nobody
//! sends, and neither view prints a rate limit the server did not report.

use anyhow::Result;
use serde_json::json;

use crate::AppContext;
use crate::output::Outcome;
use memnoth_cloud_client::{CloudClient, CreditsResponse, OrgInfo};

/// Execute billing-related commands.
pub fn run(ctx: &AppContext, args: &crate::cli::BillingCommands) -> Result<Outcome> {
    match &args.action {
        crate::cli::BillingCommand::Invoices(_) => invoices(ctx),
        crate::cli::BillingCommand::Plan(_) => plan(ctx),
        crate::cli::BillingCommand::Limits(_) => limits(ctx),
    }
}

/// Micros to whole currency units. The cloud bills in millionths, so every
/// human-facing number goes through here rather than being printed raw.
fn units(micros: i64) -> f64 {
    micros as f64 / 1_000_000.0
}

/// The only currency the control plane denominates in; it reports micros with
/// no currency field, so the unit is this constant rather than a parsed value.
const CURRENCY: &str = "USD";

/// `mn invoices` — credit balance and where the invoices themselves live.
fn invoices(ctx: &AppContext) -> Result<Outcome> {
    let client = get_cloud_client(ctx)?;

    // Billing is the credit summary: there is no invoice endpoint to call, and
    // a failure here is an authentication/deployment fact, not an empty bill.
    let credits = client.credits().ok();

    let text = match &credits {
        Some(credits) => render_invoices(credits),
        None => render_unavailable("billing"),
    };

    let data = json!({
        "billing_url": crate::context::console_url("usage"),
        "credits": credits.as_ref().map(|credits| json!({
            "balance_micros": credits.balance_micros,
            "burned_micros": credits.burned_micros,
            "window_days": credits.window_days,
            "expiring_micros": credits.expiring_micros,
            "next_expiry_at": credits.next_expiry_at,
            "demo": credits.demo,
        })),
    });

    Ok(Outcome::new("billing.invoices", text, data))
}

fn render_invoices(credits: &CreditsResponse) -> String {
    let mut out = String::new();
    out.push_str("Billing & Invoices\n");
    out.push_str("==================\n\n");

    out.push_str(&format!(
        "Balance: {CURRENCY} {:.2}\n",
        units(credits.balance_micros)
    ));

    if credits.window_days > 0 {
        out.push_str(&format!(
            "Spent (last {} days): {CURRENCY} {:.2}\n",
            credits.window_days,
            units(credits.burned_micros)
        ));
    }

    out.push_str(&expiring_line(credits));

    out.push_str(&format!(
        "\nInvoices live in the console: {}\n",
        crate::context::console_url("usage")
    ));
    out
}

/// The expiry warning, or nothing at all.
///
/// Expiry is the one fact about credits a balance cannot show, so it is worth a
/// line — but an account with nothing expiring must not read a warning-shaped
/// zero: a printed "0.00 expiring" is noise that trains people to skip the line.
fn expiring_line(credits: &CreditsResponse) -> String {
    if credits.expiring_micros <= 0 {
        return String::new();
    }
    let when = credits
        .next_expiry_at
        .as_deref()
        .map(|date| format!(", soonest {date}"))
        .unwrap_or_default();
    format!(
        "Expiring within {} days: {CURRENCY} {:.2} across {} grant(s){when}\n",
        credits.expiry_warning_days,
        units(credits.expiring_micros),
        credits.expiring_grants
    )
}

fn render_unavailable(what: &str) -> String {
    format!(
        "Unable to fetch {what}. Check authentication with `mn auth status`.\nFull billing history: {}\n",
        crate::context::console_url("usage")
    )
}

/// `mn plan` — the plan the server says this org is on, and its seat usage.
fn plan(ctx: &AppContext) -> Result<Outcome> {
    let client = get_cloud_client(ctx)?;

    let org = client.org().ok();
    let credits = client.credits().ok();

    let text = match (&org, &credits) {
        (Some(org), credits) => render_plan(org, credits.as_ref()),
        (None, _) => render_unavailable("plan info"),
    };

    let data = json!({
        "org": org.as_ref().map(|org| json!({
            "slug": org.slug,
            "name": org.name,
            "plan": org.plan,
            "role": org.role,
            "seats": org.seats,
            "seats_used": org.seats_used,
            "member_count": org.member_count,
        })),
        "credits": credits.as_ref().map(|credits| json!({
            "balance_micros": credits.balance_micros,
            "burned_micros": credits.burned_micros,
            "window_days": credits.window_days,
        })),
    });

    Ok(Outcome::new("billing.plan", text, data))
}

fn render_plan(org: &OrgInfo, credits: Option<&CreditsResponse>) -> String {
    let mut out = String::new();
    out.push_str("Current Plan\n");
    out.push_str("============\n\n");

    // The plan is a fact the org reports. It used to be *derived* from the
    // credit balance, which labelled an unused paid plan "Free" and a topped-up
    // small one "Pro" — two wrong answers invented from the one number at hand.
    out.push_str(&format!("Organization: {} (org:{})\n", org.name, org.slug));
    out.push_str(&format!("Plan: {}\n", org.plan));
    out.push_str(&format!("Your role: {}\n", org.role));
    out.push_str(&format!("Seats: {}\n", seats_line(org)));
    out.push_str(&format!(
        "Members: {}   Teams: {}   Projects: {}\n",
        org.member_count, org.team_count, org.project_count
    ));

    if let Some(credits) = credits {
        out.push_str(&format!(
            "Credit balance: {CURRENCY} {:.2}\n",
            units(credits.balance_micros)
        ));
    }

    out
}

/// `3 of 10` when seats are sold, `3 used` when the deployment has no seat plan.
fn seats_line(org: &OrgInfo) -> String {
    if org.seats > 0 {
        format!("{} of {}", org.seats_used, org.seats)
    } else {
        format!("{} used (no seat count reported)", org.seats_used)
    }
}

/// `mn limits` — the quotas the control plane reports, and nothing more.
fn limits(ctx: &AppContext) -> Result<Outcome> {
    let client = get_cloud_client(ctx)?;

    let org = client.org().ok();
    let credits = client.credits().ok();

    let text = match &org {
        Some(org) => render_limits(org, credits.as_ref()),
        None => render_unavailable("limits"),
    };

    let data = json!({
        "quotas": org.as_ref().map(|org| json!({
            "seats": org.seats,
            "seats_used": org.seats_used,
            "member_count": org.member_count,
            "team_count": org.team_count,
            "project_count": org.project_count,
            "unassigned_sessions": org.unassigned_sessions,
        })),
        "credits": credits.as_ref().map(|credits| json!({
            "balance_micros": credits.balance_micros,
            "expiring_micros": credits.expiring_micros,
            "expiry_warning_days": credits.expiry_warning_days,
        })),
    });

    Ok(Outcome::new("billing.limits", text, data))
}

fn render_limits(org: &OrgInfo, credits: Option<&CreditsResponse>) -> String {
    let mut out = String::new();
    out.push_str("Quotas\n");
    out.push_str("======\n\n");

    out.push_str(&format!("Seats: {}\n", seats_line(org)));
    out.push_str(&format!("Members: {}\n", org.member_count));
    out.push_str(&format!("Teams: {}\n", org.team_count));
    out.push_str(&format!("Projects: {}\n", org.project_count));
    out.push_str(&format!(
        "Unassigned sessions: {}\n",
        org.unassigned_sessions
    ));

    if let Some(credits) = credits {
        out.push_str(&format!(
            "Credit balance: {CURRENCY} {:.2}\n",
            units(credits.balance_micros)
        ));
        out.push_str(&expiring_line(credits));
    }

    // The old view printed "Requests/minute: 60", "Tokens/minute: 100,000" and
    // similar: numbers this deployment has no source for. Saying nothing is the
    // only honest option until the control plane reports a limit.
    out.push_str("\nThis deployment reports no request or token rate limits.\n");
    out.push_str("Monitor usage: `mn usage`\n");

    out
}

fn get_cloud_client(ctx: &AppContext) -> Result<CloudClient> {
    let mut client = CloudClient::new()?;

    if let Some(token) = ctx.auth_token() {
        client.set_auth_token(token);
    }

    Ok(client)
}

#[cfg(test)]
mod tests {
    use super::*;
    use memnoth_cloud_client::{CreditGrant, CreditLedgerEntry};

    fn credits(balance_micros: i64, burned_micros: i64) -> CreditsResponse {
        CreditsResponse {
            balance_micros,
            burned_micros,
            window_days: 30,
            grants: vec![CreditGrant {
                id: "g1".into(),
                amount_micros: 5_000_000,
                remaining_micros: 5_000_000,
                source: "grant".into(),
                note: "trial".into(),
                granted_by: "operator".into(),
                expires_at: Some("2026-11-13T00:00:00Z".into()),
                created_at: "2026-10-14T00:00:00Z".into(),
            }],
            ledger: vec![CreditLedgerEntry {
                id: "l1".into(),
                kind: "debit".into(),
                amount_micros: -1_200_000,
                balance_after_micros: Some(3_800_000),
                reason: "managed turn".into(),
                ref_id: "turn:abc".into(),
                actor: "system".into(),
                created_at: "2026-11-01T00:00:00Z".into(),
            }],
            unpriced_sessions: 0,
            expiring_micros: 0,
            expiring_grants: 0,
            next_expiry_at: None,
            expiry_warning_days: 7,
            grants_total: 1,
            ledger_total: 1,
            demo: false,
        }
    }

    fn org() -> OrgInfo {
        OrgInfo {
            slug: "acme".into(),
            name: "Acme".into(),
            plan: "team".into(),
            role: "admin".into(),
            member_count: 4,
            seats_used: 3,
            seats: 10,
            team_count: 2,
            project_count: 5,
            unassigned_sessions: 1,
            created_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    #[test]
    fn the_invoice_view_renders_the_reported_balance_and_window() {
        let text = render_invoices(&credits(7_250_000, 1_200_000));
        assert!(text.contains("Balance: USD 7.25"), "{text}");
        assert!(text.contains("Spent (last 30 days): USD 1.20"), "{text}");
        assert!(text.contains("/app/usage"), "{text}");
    }

    #[test]
    fn nothing_expiring_prints_no_expiry_line() {
        let quiet = render_invoices(&credits(7_250_000, 0));
        assert!(!quiet.contains("Expiring within"), "{quiet}");

        let mut soon = credits(7_250_000, 0);
        soon.expiring_micros = 5_000_000;
        soon.expiring_grants = 1;
        soon.next_expiry_at = Some("2026-11-13T00:00:00Z".into());
        let loud = render_invoices(&soon);
        assert!(loud.contains("Expiring within 7 days: USD 5.00"), "{loud}");
        assert!(loud.contains("2026-11-13"), "{loud}");
    }

    #[test]
    fn the_plan_is_the_reported_plan_not_a_balance_guess() {
        // A $0.50 balance used to be labelled "Starter" and $100 "Pro"; neither
        // is what the org says it is on.
        let text = render_plan(&org(), Some(&credits(500_000, 0)));
        assert!(text.contains("Plan: team"), "{text}");
        assert!(text.contains("Seats: 3 of 10"), "{text}");
        assert!(
            !text.contains("Starter") && !text.contains("Pro\n"),
            "{text}"
        );
    }

    #[test]
    fn limits_print_only_numbers_the_server_sent() {
        let text = render_limits(&org(), Some(&credits(1_000_000, 0)));
        assert!(text.contains("Projects: 5"), "{text}");
        assert!(text.contains("Unassigned sessions: 1"), "{text}");
        assert!(!text.contains("Requests/minute"), "{text}");
        assert!(!text.contains("100,000"), "{text}");
    }
}
