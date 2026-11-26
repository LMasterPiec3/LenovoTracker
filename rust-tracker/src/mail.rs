//! Mail-Bodies (Text/HTML), Subject, SMTP-Versand (plain/STARTTLS).

use crate::api::*;
use crate::config::*;
use crate::diff::*;
use crate::util::*;
use anyhow::{anyhow, bail, Context, Result};
use lettre::message::{Mailbox, MultiPart};
use std::collections::BTreeMap;
use std::io::Write;
use std::net::{IpAddr, ToSocketAddrs};
use std::process::{Command, Stdio};

pub fn build_subject(prefix: &str, diff: &Diff, kw_moved: &[(String, KwStat, KwStat)]) -> String {
    if diff.delta() > 0 {
        format!(
            "{} Mainboard-Bestand gestiegen: {} → {} (+{})",
            prefix,
            fmt_de(diff.old_total),
            fmt_de(diff.new_total),
            fmt_de(diff.delta())
        )
    } else {
        let details = kw_moved
            .iter()
            .take(3)
            .map(|(k, o, n)| {
                if o.stock == 0 {
                    format!("{} NEU ({} Stück)", k, fmt_de(n.stock))
                } else {
                    format!(
                        "{} {}→{} (+{})",
                        k,
                        fmt_de(o.stock),
                        fmt_de(n.stock),
                        fmt_de(n.stock - o.stock)
                    )
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        format!("{} Keyword-Bewegung: {}", prefix, details)
    }
}

/// Kosten eines Teils: Stückpreis × Bestand in Cent – `None`, wenn kein Preis
/// bekannt ist. (Der frühere Listenpreis wird nicht mehr angezeigt.)
fn part_cost(prices: &PriceMap, id: &str, stock: i64) -> Option<i64> {
    prices.get(id).map(|unit| unit * stock)
}

/// Kostenspalte der Textmail: "119,78 €", sonst "–".
fn cost_text(prices: &PriceMap, id: &str, stock: i64, currency: &str) -> String {
    part_cost(prices, id, stock).map_or_else(|| "–".to_string(), |c| fmt_money(c, currency))
}

/// Alles, was Text- und HTML-Mail gemeinsam brauchen – ein Struct statt acht
/// Parametern, das am Aufrufort einmal gefüllt wird.
pub struct MailView<'a> {
    pub diff: &'a Diff,
    pub page_url: &'a str,
    pub checked_at: &'a str,
    pub specs: &'a BTreeMap<String, String>,
    pub prices: &'a PriceMap,
    pub currency: &'a str,
    pub filter_label: &'a str,
    pub kw_moved: &'a [(String, KwStat, KwStat)],
}

/// Text- und HTML-Body in einem Aufruf – beide brauchen dieselben acht Werte.
pub fn build_mail(view: &MailView<'_>) -> (String, String) {
    (mail_text(view), mail_html(view))
}

fn mail_text(view: &MailView<'_>) -> String {
    let MailView {
        diff,
        page_url,
        checked_at,
        specs,
        prices,
        currency,
        filter_label,
        kw_moved,
    } = *view;
    let mut lines = vec![
        "Mainboard-Bestand gestiegen!".to_string(),
        String::new(),
        format!("Vorher: {} Stück", fmt_de(diff.old_total)),
        format!("Jetzt:  {} Stück", fmt_de(diff.new_total)),
        format!("Änderung: +{}", fmt_de(diff.delta())),
        String::new(),
    ];
    if !diff.increased.is_empty() {
        lines.push(format!(
            "Bestandsanstiege ({} Teile):",
            diff.increased.len()
        ));
        for item in diff.increased.iter() {
            lines.push(format!(
                "  +{}  {}  {}  ({} → {})  · {}",
                item.delta,
                item.id,
                item.name,
                item.old,
                item.new,
                cost_text(prices, &item.id, item.new, currency)
            ));
            if let Some(info) = specs.get(&item.id) {
                lines.push(format!("      {}", info));
            }
        }
        lines.push(String::new());
    }
    if !kw_moved.is_empty() {
        lines.push("Keyword-Bewegung:".to_string());
        for (kw, o, n) in kw_moved.iter() {
            if o.stock == 0 {
                lines.push(format!("  {}: NEU mit {} Stück", kw, fmt_de(n.stock)));
            } else {
                lines.push(format!(
                    "  {}: {} → {} (+{})",
                    kw,
                    fmt_de(o.stock),
                    fmt_de(n.stock),
                    fmt_de(n.stock - o.stock)
                ));
            }
        }
        lines.push(String::new());
    }
    if !diff.new_parts.is_empty() {
        lines.push(format!("Neu im Sale: {} Teile", diff.new_parts.len()));
        for (id, name, stock) in diff.new_parts.iter() {
            lines.push(format!(
                "  {}  {}  ({} Stück)  · {}",
                id,
                name,
                stock,
                cost_text(prices, id, *stock, currency)
            ));
            if let Some(info) = specs.get(id) {
                lines.push(format!("      {}", info));
            }
        }
        lines.push(String::new());
    }
    lines.push(format!("Seite: {}", page_url));
    lines.push(format!("Geprüft: {}", checked_at));
    lines.push(format!("Filter: {}", filter_label));
    lines.join("\n")
}

/// "2026-11-19T19:22:48Z" → "2026-11-19 19:22:48 UTC"
fn pretty_ts(ts: &str) -> String {
    format!("{} UTC", ts.replace('T', " ").trim_end_matches('Z'))
}

/// Tech-Daten als Chip-Badges (Inline-Styles, mail-client-sicher).
fn html_spec_chips(line: &str) -> String {
    line.split(" | ")
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| {
            format!(
                "<span style='display:inline-block;background:#f1f3f5;border:1px solid #e0e4e8;border-radius:4px;padding:2px 8px;margin:3px 4px 3px 0;font-size:11px;color:#3d434b'>{}</span>&nbsp;",
                esc(s)
            )
        })
        .collect()
}

/// Kostenzelle der HTML-Tabelle: Stückpreis × Bestand, sonst "–".
fn html_cost_cell(prices: &PriceMap, id: &str, stock: i64, currency: &str) -> String {
    part_cost(prices, id, stock).map_or_else(
        || "<span style='color:#8a8f98'>–</span>".to_string(),
        |c| format!("<b>{}</b>", esc(&fmt_money(c, currency))),
    )
}

fn html_delta_badge(delta: i64) -> String {
    format!(
        "<span style='display:inline-block;background:#e6f4ea;color:#137333;border-radius:10px;padding:2px 10px;font-size:12px;font-weight:bold'>+{}</span>",
        fmt_de(delta)
    )
}

pub const MAIL_CELL: &str = "padding:10px;border-bottom:1px solid #eceff2;font-family:Arial,Helvetica,sans-serif;font-size:13px;color:#222222;vertical-align:top";

fn mail_html(view: &MailView<'_>) -> String {
    let MailView {
        diff,
        page_url,
        checked_at,
        specs,
        prices,
        currency,
        filter_label,
        kw_moved,
    } = *view;
    let mut h: Vec<String> = vec![
        "<html><body style='margin:0;padding:0;background:#eef1f4'>".to_string(),
        "<table role='presentation' width='100%' cellpadding='0' cellspacing='0' bgcolor='#eef1f4' style='background:#eef1f4'><tr><td align='center' style='padding:24px 8px'>".to_string(),
        "<table role='presentation' width='600' cellpadding='0' cellspacing='0' style='width:600px;max-width:600px;background:#ffffff;border:1px solid #e3e6ea;border-radius:8px;overflow:hidden'>".to_string(),
        // Marken-Kopf
        "<tr><td bgcolor='#e2231a' style='background:#e2231a;padding:10px 24px;font-family:Arial,Helvetica,sans-serif;font-size:11px;letter-spacing:2px;color:#ffffff;font-weight:bold'>LENOVO CLEARANCE-TRACKER</td></tr>".to_string(),
        // Titel + Delta-Badge
        format!(
            "<tr><td style='padding:24px 24px 10px 24px;font-family:Arial,Helvetica,sans-serif'><div style='font-size:22px;line-height:1.3;font-weight:bold;color:#111111'>Mainboard-Bestand gestiegen!</div><div style='font-size:14px;color:#555555;margin-top:8px'>{} → {} Stück&nbsp;&nbsp;{}</div></td></tr>",
            fmt_de(diff.old_total), fmt_de(diff.new_total), html_delta_badge(diff.delta())
        ),
    ];
    if !kw_moved.is_empty() {
        let mut kw_rows: Vec<String> = Vec::new();
        for (kw, o, n) in kw_moved.iter() {
            kw_rows.push(format!(
                "<tr><td style='{}'>{}</td><td style='{};text-align:right'>{} → {} <b style='color:#137333'>+{}</b></td></tr>",
                MAIL_CELL,
                esc(kw),
                MAIL_CELL,
                fmt_de(o.stock),
                fmt_de(n.stock),
                fmt_de(n.stock - o.stock)
            ));
        }
        h.push(format!(
            "<tr><td style='padding:14px 24px 4px 24px'><div style='font-size:13px;font-weight:bold;color:#111111;text-transform:uppercase;letter-spacing:1px;border-left:4px solid #e2231a;padding-left:10px'>Keyword-Bewegung</div></td></tr><tr><td style='padding:8px 24px 8px 24px'><table role='presentation' width='100%' cellpadding='0' cellspacing='0' style='border-collapse:collapse'>{}</table></td></tr>",
            kw_rows.join("")
        ));
    }

    let section = |title: &str| {
        format!(
            "<tr><td style='padding:14px 24px 4px 24px;font-family:Arial,Helvetica,sans-serif'><div style='font-size:13px;font-weight:bold;color:#111111;text-transform:uppercase;letter-spacing:1px;border-left:4px solid #e2231a;padding-left:10px'>{}</div></td></tr>",
            esc(title)
        )
    };
    let table_open = "<tr><td style='padding:8px 24px 8px 24px'><table role='presentation' width='100%' cellpadding='0' cellspacing='0' style='border-collapse:collapse'>";
    let table_close = "</table></td></tr>";
    let head_cell = "background:#111111;color:#ffffff;font-family:Arial,Helvetica,sans-serif;font-size:11px;letter-spacing:1px;font-weight:bold;padding:8px 10px;text-align:left";

    if !diff.increased.is_empty() {
        h.push(section(&format!(
            "Bestandsanstiege ({})",
            diff.increased.len()
        )));
        h.push(table_open.to_string());
        h.push(format!(
            "<tr><th style='{}'>Δ</th><th style='{}'>Teilenummer</th><th style='{}'>Bezeichnung</th><th style='{};text-align:center'>Bestand</th><th style='{};text-align:right'>Kosten</th></tr>",
            head_cell, head_cell, head_cell, head_cell, head_cell
        ));
        for item in diff.increased.iter() {
            let chips = match specs.get(&item.id) {
                Some(info) => format!(
                    "<div style='margin-top:6px'>{}</div>",
                    html_spec_chips(info)
                ),
                None => String::new(),
            };
            h.push(format!(
                "<tr><td style='{};color:#137333;font-weight:bold'>+{}</td><td style='{};font-family:Consolas,Menlo,monospace;font-weight:bold'>{}</td><td style='{}'><b>{}</b>{}</td><td style='{};text-align:center'>{} → {}</td><td style='{};text-align:right;white-space:nowrap'>{}</td></tr>",
                MAIL_CELL, item.delta, MAIL_CELL, esc(&item.id), MAIL_CELL, esc(&item.name), chips, MAIL_CELL, item.old, item.new, MAIL_CELL, html_cost_cell(prices, &item.id, item.new, currency)
            ));
        }
        h.push(table_close.to_string());
    }
    if !diff.new_parts.is_empty() {
        h.push(section(&format!("Neu im Sale ({})", diff.new_parts.len())));
        h.push(table_open.to_string());
        h.push(format!(
            "<tr><th style='{}'>Teilenummer</th><th style='{}'>Bezeichnung</th><th style='{};text-align:center'>Bestand</th><th style='{};text-align:right'>Kosten</th></tr>",
            head_cell, head_cell, head_cell, head_cell
        ));
        for (id, name, stock) in diff.new_parts.iter() {
            let chips = match specs.get(id) {
                Some(info) => format!(
                    "<div style='margin-top:6px'>{}</div>",
                    html_spec_chips(info)
                ),
                None => String::new(),
            };
            h.push(format!(
                "<tr><td style='{};font-family:Consolas,Menlo,monospace;font-weight:bold'>{}</td><td style='{}'><b>{}</b>{}</td><td style='{};text-align:center'>{} Stück</td><td style='{};text-align:right;white-space:nowrap'>{}</td></tr>",
                MAIL_CELL, esc(id), MAIL_CELL, esc(name), chips, MAIL_CELL, stock, MAIL_CELL, html_cost_cell(prices, id, *stock, currency)
            ));
        }
        h.push(table_close.to_string());
    }

    h.push(format!(
        "<tr><td align='center' style='padding:16px 24px 22px 24px'><table role='presentation' cellpadding='0' cellspacing='0'><tr><td bgcolor='#e2231a' style='border-radius:6px'><a href='{}' style='display:inline-block;padding:12px 28px;font-family:Arial,Helvetica,sans-serif;font-size:14px;font-weight:bold;color:#ffffff;text-decoration:none'>Clearance-Sale ansehen →</a></td></tr></table></td></tr>",
        esc(page_url)
    ));
    h.push(format!(
        "<tr><td bgcolor='#f7f8fa' style='background:#f7f8fa;padding:14px 24px;border-top:1px solid #e3e6ea;font-family:Arial,Helvetica,sans-serif;font-size:11px;color:#8a8f98'>Geprüft: {} · Filter: {}<br><a href='{}' style='color:#e2231a'>{}</a></td></tr>",
        esc(&pretty_ts(checked_at)), esc(filter_label), esc(page_url), esc(page_url)
    ));
    h.push("</table></td></tr></table></body></html>".to_string());
    h.join("\n")
}

pub fn compose_mail(
    cfg: &Config,
    subject: String,
    text: String,
    html: String,
) -> Result<lettre::Message> {
    let from: Mailbox = cfg
        .mail
        .from
        .parse()
        .context("mail.from ist keine gültige Adresse")?;
    let mut builder = lettre::Message::builder().from(from).subject(subject);
    for recipient in cfg.mail.to.iter().chain(cfg.mail.extra_recipients.iter()) {
        let mb: Mailbox = recipient
            .parse()
            .with_context(|| format!("Ungültige Empfängeradresse: {}", recipient))?;
        builder = builder.to(mb);
    }
    builder
        .multipart(MultiPart::alternative_plain_html(text, html))
        .context("Mail konnte nicht zusammengesetzt werden")
}

/// Alle IPs zum Host (dedupliziert, max. 4) – Plain-SMTP-Fallback über mehrere
/// Mailin-Server: Provider tarpitten einzelne Nodes nach vielen anonymen
/// Verbindungen, ein anderer Node nimmt die Mail dann an.
pub fn smtp_target_ips(host: &str) -> Vec<IpAddr> {
    let mut out: Vec<IpAddr> = Vec::new();
    if let Ok(list) = (host, 0u16).to_socket_addrs() {
        for a in list {
            let ip = a.ip();
            if !out.contains(&ip) {
                out.push(ip);
                if out.len() >= 4 {
                    break;
                }
            }
        }
    }
    out
}

/// SMTP-URL für curl. IPv6-Literale brauchen eckige Klammern.
fn smtp_url(host: &str, port: u16) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("smtp://[{}]:{}", host, port)
    } else {
        format!("smtp://{}:{}", host, port)
    }
}

/// Reine Adresse für den SMTP-Envelope: `Name <a@b>` → `a@b`.
fn envelope_addr(s: &str) -> &str {
    let s = s.trim();
    match (s.rfind('<'), s.rfind('>')) {
        (Some(a), Some(b)) if b > a + 1 => s[a + 1..b].trim(),
        _ => s,
    }
}

/// Eine Mail über curl verschicken: `--upload-file -` liest die fertige
/// MIME-Mail aus stdin, curl übernimmt SMTP-Dialog, STARTTLS und AUTH.
fn send_via_curl(
    cfg: &Config,
    host: &str,
    port: u16,
    tls: bool,
    timeout: u64,
    raw: &[u8],
) -> Result<()> {
    let mut cmd = Command::new(&cfg.api.curl_path);
    cmd.arg("--url")
        .arg(smtp_url(host, port))
        .arg("--mail-from")
        .arg(envelope_addr(&cfg.mail.from))
        .arg("--upload-file")
        .arg("-")
        .arg("--silent")
        .arg("--show-error")
        .arg("--max-time")
        .arg(timeout.to_string());
    for rcpt in cfg.mail.to.iter().chain(cfg.mail.extra_recipients.iter()) {
        cmd.arg("--mail-rcpt").arg(envelope_addr(rcpt));
    }
    if tls {
        cmd.arg("--ssl-reqd")
            .arg("--user")
            .arg(format!("{}:{}", cfg.mail.smtp_user, cfg.mail.smtp_pass));
    }
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .with_context(|| format!("curl konnte nicht gestartet werden: {}", cfg.api.curl_path))?;
    if let Some(mut stdin) = child.stdin.take() {
        // Bricht curl vorher ab (z. B. 5xx beim RCPT), ist EPIPE normal – der
        // Exit-Status unten trägt den eigentlichen Fehler.
        let _ = stdin.write_all(raw);
    }
    let out = child.wait_with_output()?;
    if out.status.success() {
        return Ok(());
    }
    let code = out
        .status
        .code()
        .map_or_else(|| "Signal".to_string(), |c| c.to_string());
    Err(anyhow!(
        "SMTP {}:{} – curl-Exit {}: {}",
        host,
        port,
        code,
        String::from_utf8_lossy(&out.stderr).trim()
    ))
}

pub fn deliver_mail(cfg: &Config, msg: &lettre::Message) -> Result<()> {
    // curl ist im Container ohnehin vorhanden (curl-impersonate) und bringt den
    // TLS-Stack mit, der auch für den API-Abruf genutzt wird – rustls, ring und
    // der eingebettete Root-Store (~900 KB Binary) entfallen dadurch.
    // Mit smtp_user/smtp_pass: authentifizierte Submission über STARTTLS – der
    // Provider signiert DKIM/SPF (kein "failed authentication"-Banner beim
    // Empfänger). Ohne Login: Plain-SMTP, tarpit-resilient – die Mail wird
    // nacheinander über alle aufgelösten Endpunkt-IPs versucht (je 25 s), bis
    // eine den Versand zulässt.
    let raw = msg.formatted();
    if cfg.mail.smtp_user.is_empty() {
        let ips = smtp_target_ips(&cfg.mail.smtp_host);
        if ips.is_empty() {
            bail!(
                "SMTP-Host konnte nicht aufgelöst werden: {}",
                cfg.mail.smtp_host
            );
        }
        let mut last: Option<anyhow::Error> = None;
        for ip in &ips {
            log_info!("SMTP-Versand (plain) über {}", ip);
            match send_via_curl(cfg, &ip.to_string(), cfg.mail.smtp_port, false, 25, &raw) {
                Ok(_) => return Ok(()),
                Err(e) => {
                    log_warn!("SMTP über {} fehlgeschlagen ({e}) – nächster Endpunkt", ip);
                    last = Some(e);
                }
            }
        }
        bail!(
            "SMTP-Verbindung auf allen {} Endpunkten fehlgeschlagen: {}",
            ips.len(),
            last.map(|e| e.to_string()).unwrap_or_default()
        );
    }
    send_via_curl(cfg, &cfg.mail.smtp_host, cfg.mail.smtp_port, true, 60, &raw)?;
    Ok(())
}

pub fn build_test_mail_text(total: i64, count: usize, ship: &str, ts: &str) -> String {
    format!(
        "Installation fertig – der Lenovo-Mainboard-Tracker läuft.\n\n\
         Aktueller Mainboard-Bestand: {} Stück über {} Teile.\n\
         Versandweg: {}\nGeprüft: {}\nSeite: {}\n",
        fmt_de(total),
        count,
        ship,
        ts,
        PAGE_URL
    )
}

pub fn build_test_mail_html(total: i64, count: usize, label: &str, ship: &str, ts: &str) -> String {
    // Gleiche Design-Sprache wie die Änderungs-Mails: Marken-Kopf, Info-Tabelle,
    // CTA-Button, Fußzeile.
    format!(
        "<html><body style='margin:0;padding:0;background:#eef1f4'>\
<table role='presentation' width='100%' cellpadding='0' cellspacing='0' bgcolor='#eef1f4' style='background:#eef1f4'><tr><td align='center' style='padding:24px 8px'>\
<table role='presentation' width='600' cellpadding='0' cellspacing='0' style='width:600px;max-width:600px;background:#ffffff;border:1px solid #e3e6ea;border-radius:8px;overflow:hidden'>\
<tr><td bgcolor='#e2231a' style='background:#e2231a;padding:10px 24px;font-family:Arial,Helvetica,sans-serif;font-size:11px;letter-spacing:2px;color:#ffffff;font-weight:bold'>LENOVO CLEARANCE-TRACKER</td></tr>\
<tr><td style='padding:24px 24px 10px 24px;font-family:Arial,Helvetica,sans-serif'>\
<div style='font-size:22px;line-height:1.3;font-weight:bold;color:#111111'>Installation fertig – der Tracker läuft ✓</div>\
<div style='font-size:14px;color:#555555;margin-top:8px'>Mail-Zustellung funktioniert. Bei Änderungen des Mainboard-Bestands bekommst du automatisch eine Benachrichtigung.</div>\
</td></tr>\
<tr><td style='padding:8px 24px 8px 24px'><table role='presentation' width='100%' cellpadding='0' cellspacing='0' style='border-collapse:collapse'>\
<tr><td style='{cell}'>Aktueller Bestand</td><td style='{cell};text-align:right'><b style='font-size:16px'>{tot} Stück</b> über {cnt} Teile</td></tr>\
<tr><td style='{cell}'>Filter</td><td style='{cell};text-align:right'>{label}</td></tr>\
<tr><td style='{cell}'>Versandweg</td><td style='{cell};text-align:right'>{ship}</td></tr>\
<tr><td style='{cell};border-bottom:none'>Geprüft</td><td style='{cell};text-align:right;border-bottom:none'>{pretty}</td></tr>\
</table></td></tr>\
<tr><td align='center' style='padding:16px 24px 22px 24px'><table role='presentation' cellpadding='0' cellspacing='0'><tr><td bgcolor='#e2231a' style='border-radius:6px'><a href='{url}' style='display:inline-block;padding:12px 28px;font-family:Arial,Helvetica,sans-serif;font-size:14px;font-weight:bold;color:#ffffff;text-decoration:none'>Clearance-Sale ansehen →</a></td></tr></table></td></tr>\
<tr><td bgcolor='#f7f8fa' style='background:#f7f8fa;padding:14px 24px;border-top:1px solid #e3e6ea;font-family:Arial,Helvetica,sans-serif;font-size:11px;color:#8a8f98'>Installations-Testmail – kein Bestandswechsel · <a href='{url}' style='color:#e2231a'>{url}</a></td></tr>\
</table></td></tr></table></body></html>",
        cell = MAIL_CELL,
        tot = fmt_de(total),
        cnt = count,
        label = esc(label),
        ship = esc(ship),
        pretty = esc(&pretty_ts(ts)),
        url = esc(PAGE_URL)
    )
}

pub fn send_test_mail(cfg: &Config) -> Result<()> {
    let outcome = fetch_clearance_parts(cfg, &cfg.tracker.commodity_keys)
        .map_err(|e| anyhow!("Abruf fehlgeschlagen: {}", e))?;
    let mut part_map = outcome.boards;
    crate::apply_keyword_filter(cfg, &mut part_map, None)
        .map_err(|e| anyhow!("Keyword-/Specs-Filter fehlgeschlagen: {}", e))?;
    let (total, count) = compute_totals(&part_map);
    let ts = now_iso();
    let subject = format!(
        "{} Installation fertig – Tracker aktiv",
        cfg.mail.subject_prefix
    );
    let label = filter_label(&cfg.tracker.commodity_keys, &cfg.tracker.name_keywords);
    let ship = format!(
        "SMTP {} ({})",
        cfg.mail.smtp_host,
        if cfg.mail.smtp_user.is_empty() {
            "plain"
        } else {
            "STARTTLS+AUTH"
        }
    );
    let text = build_test_mail_text(total, count, &ship, &ts);
    let html = build_test_mail_html(total, count, &label, &ship, &ts);
    let msg = compose_mail(cfg, subject, text, html)?;
    deliver_mail(cfg, &msg)?;
    log_info!(
        "Mail via SMTP {} ({}) gesendet: {}",
        cfg.mail.smtp_host,
        if cfg.mail.smtp_user.is_empty() {
            "plain"
        } else {
            "STARTTLS+AUTH"
        },
        cfg.mail.from
    );
    Ok(())
}
