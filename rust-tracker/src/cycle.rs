//! Ein Lauf: Abruf → Filter → Diff → Mail → State (Rückgabe: Exit-Code).
//!
//! Hier liegt `run_once` samt den Helfern, die nur dieser Ablauf braucht:
//! Specs-Nachladen, Mail-Keyword-Filter, `tracked.json` und Prüfsumme.

use crate::{api::*, config::*, diff::*, guard::*, mail::*, state::*, util::*};
use std::collections::BTreeMap;

/// Nach Erholung: Zaehler zuruecksetzen; war eine Stoerung gemeldet, folgt
/// eine kurze Recovery-Mail.
pub(crate) fn reset_fail_state(cfg: &Config) {
    let p = fail_state_path(cfg);
    let st = load_fail_state(&p);
    if st.failures == 0 {
        return;
    }
    let was_alerted = st.alerted;
    if let Err(e) = save_fail_state(&p, &FailState::default()) {
        log_warn!("FailState-Reset fehlgeschlagen: {}", e);
    }
    if was_alerted {
        let subject = format!("{} Tracker wieder online", cfg.mail.subject_prefix);
        let text = format!(
            "Der Lenovo-Mainboard-Tracker ist wieder erreichbar.\n\n\
             Aktueller Bestand: siehe nächste Änderungs-Mail oder\n\
             pct exec <VMID> -- cat {}/state.json\n",
            cfg.paths.state_dir
        );
        let html = "<html><body style='margin:0;padding:0;background:#eef1f4'>\
<table role='presentation' width='100%' cellpadding='0' cellspacing='0' bgcolor='#eef1f4'><tr><td align='center' style='padding:24px 8px'>\
<table role='presentation' width='600' cellpadding='0' cellspacing='0' style='width:600px;max-width:600px;background:#ffffff;border:1px solid #e3e6ea;border-radius:8px;overflow:hidden'>\
<tr><td bgcolor='#137333' style='background:#137333;padding:10px 24px;font-family:Arial,Helvetica,sans-serif;font-size:11px;letter-spacing:2px;color:#ffffff;font-weight:bold'>LENOVO CLEARANCE-TRACKER</td></tr>\
<tr><td style='padding:24px 24px 10px 24px;font-family:Arial,Helvetica,sans-serif'><div style='font-size:22px;line-height:1.3;font-weight:bold;color:#111111'>Erholung – Tracker wieder online ✓</div><div style='font-size:14px;color:#555555;margin-top:8px'>Die API ist wieder erreichbar; Änderungs-Mails laufen normal weiter.</div></td></tr>\
</table></td></tr></table></body></html>".to_string();
        if let Ok(m) = compose_mail(cfg, subject, text, html) {
            if let Err(err) = deliver_mail(cfg, &m) {
                log_warn!("Recovery-Mail fehlgeschlagen: {}", err);
            }
        }
    }
}

/// Ergänzt die technischen Daten (Specs) und wendet den Keyword-Filter an.
/// Der Specs-Cache (id → Specs-Zeile) liegt neben dem State; geladen werden
/// nur IDs, die dort noch fehlen – auch Teile ohne Specs werden als leerer
/// Eintrag vermerkt, damit sie nicht bei jedem Lauf erneut angefragt werden.
/// `preloaded` ist der bereits parallel zum Abruf geladene Cache (spart den
/// seriellen Disk-/Parse-Schritt); `None` lädt ihn hier.
/// Rückgabe: der Cache für die Keyword-Statistik.
/// Wie `apply_keyword_filter`, aber ohne Keyword-Filter: lädt die technischen
/// Daten für **alle** Teile des Abrufs (Cache, einmalig je Teil). Die Web-UI
/// zeigt den kompletten Bestand, deshalb braucht sie Specs für alles.
pub(crate) fn load_specs_for_all(
    cfg: &Config,
    map: &BTreeMap<String, PartState>,
    preloaded: Option<BTreeMap<String, String>>,
) -> anyhow::Result<BTreeMap<String, String>> {
    let path = specs_cache_path(cfg);
    let mut specs = match preloaded {
        Some(s) => s,
        None => load_specs_cache(&path),
    };
    let missing: Vec<&str> = map
        .keys()
        .filter(|id| !specs.contains_key(id.as_str()))
        .map(String::as_str)
        .collect();
    if !missing.is_empty() {
        log_info!(
            "Technische Daten für {} Teile nachladen (Keyword-Filter, einmalig je Teil)",
            missing.len()
        );
        let mut loaded = fetch_specs_batched(cfg, &missing)?;
        for id in missing {
            // Treffer verschieben statt klonen. Teile ohne Specs bleiben als
            // leerer Eintrag im Cache, sonst würden sie jeden Lauf erneut
            // angefragt – dafür genügt der Default-Wert.
            specs.insert(id.to_string(), loaded.remove(id).unwrap_or_default());
        }
        save_specs_cache(&path, &specs)?;
    }
    Ok(specs)
}

/// Behält nur Teile, die den Mail-Keywords entsprechen (leere Liste = alle).
pub(crate) fn retain_keyword_matches(
    cfg: &Config,
    map: &mut BTreeMap<String, PartState>,
    specs: &BTreeMap<String, String>,
) {
    let keywords = &cfg.tracker.name_keywords;
    if keywords.is_empty() {
        return;
    }
    map.retain(|id, p| part_matches(id, p, specs, keywords));
}

/// Specs laden + Mail-Keyword-Filter (Report/Testmail).
pub(crate) fn apply_keyword_filter(
    cfg: &Config,
    map: &mut BTreeMap<String, PartState>,
    preloaded: Option<BTreeMap<String, String>>,
) -> anyhow::Result<BTreeMap<String, String>> {
    if cfg.tracker.name_keywords.is_empty() {
        return Ok(BTreeMap::new());
    }
    let specs = load_specs_for_all(cfg, map, preloaded)?;
    retain_keyword_matches(cfg, map, &specs);
    Ok(specs)
}

/// Schreibt den kompletten Bestand für die Web-UI (unabhängig von den
/// Mail-Keywords). Fehler sind nicht fatal – die Mail-Seite läuft weiter.
pub(crate) fn write_tracked(
    cfg: &Config,
    ts: &str,
    parts: &BTreeMap<String, PartState>,
    prices: &PriceMap,
) {
    let (path, history) = tracked_paths(cfg);
    let (total, count) = compute_totals(parts);
    let label = filter_label(&cfg.tracker.commodity_keys, &[]);
    let res = save_tracked(&path, ts, &label, total, count, parts, prices)
        .and_then(|_| append_history(&history, ts, total, count));
    if let Err(e) = res {
        log_warn!("Web-UI-Bestand (tracked.json) nicht schreibbar: {}", e);
    }
}

/// Verteilt frisch geladene Specs auf die angefragten IDs: Treffer wandern in
/// die Ausgabe (nur nicht-leere), jeder geprüfte Eintrag landet im Cache –
/// auch ein Negativ-Treffer (leer), damit das Teil nicht erneut angefragt wird.
pub(crate) fn merge_specs(
    ids: &[&str],
    fetched: BTreeMap<String, String>,
    out: &mut BTreeMap<String, String>,
    cache: &mut BTreeMap<String, String>,
) {
    let mut fetched = fetched;
    for &id in ids {
        let line = fetched.remove(id).unwrap_or_default();
        if !line.is_empty() {
            out.insert(id.to_string(), line.clone());
        }
        cache.insert(id.to_string(), line);
    }
}

/// Specs für die Mail-Tabelle: was im Cache (aus dem Filter-Lauf) liegt, kommt
/// ohne Netz; nur fehlende IDs gehen als ein Batch-POST raus. Neue Zeilen
/// werden in den Cache geschrieben, damit spätere Mails ohne Abruf auskommen.
/// Fehler sind nicht fatal – die Mail geht dann ohne Specs raus.
pub(crate) fn specs_for_mail(
    cfg: &Config,
    ids: &[&str],
    cache: &mut BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let path = specs_cache_path(cfg);
    // Ohne Keywords ist der Cache nicht geladen (apply_keyword_filter kehrt
    // dann früh zurück) – hier einmalig von Platte holen.
    if cache.is_empty() && path.exists() {
        *cache = load_specs_cache(&path);
    }
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    let mut missing: Vec<&str> = Vec::new();
    for &id in ids {
        match cache.get(id) {
            Some(line) if !line.is_empty() => {
                out.insert(id.to_string(), line.clone());
            }
            Some(_) => {} // bereits geprüft, aber keine Specs vorhanden
            None => missing.push(id),
        }
    }
    if missing.is_empty() {
        return out;
    }
    // Seit alle Teile in der Mail stehen, können das hunderte IDs sein:
    // `fetch_specs_batched` teilt sie in Batches (`api.specs_batch`) und
    // begrenzt die Anfragenrate über `api.specs_gap_ms`.
    match fetch_specs_batched(cfg, &missing) {
        Ok(fetched) => {
            merge_specs(&missing, fetched, &mut out, cache);
            if let Err(e) = save_specs_cache(&path, cache) {
                log_warn!("Specs-Cache nicht schreibbar: {}", e);
            }
        }
        Err(e) => log_warn!("Tech-Daten nicht abrufbar – Mail ohne Specs: {}", e),
    }
    out
}

/// Nimmt MX-/Quadro-T-Karten aus den Mail-Listen und sagt, ob danach noch
/// etwas zu senden ist. Der State bleibt unberührt – der Aufrufer schreibt ihn
/// auch dann, wenn nur gefilterte Einsteigerkarten gestiegen sind.
pub(crate) fn filter_mail_parts(
    diff: &mut Diff,
    mail_specs: &BTreeMap<String, String>,
    kw_moved: &[(String, KwStat, KwStat)],
) -> bool {
    let excluded = drop_low_end_parts(diff, mail_specs);
    if excluded > 0 {
        log_info!("{} MX-/Quadro-T-Karten aus der Mail gefiltert", excluded);
    }
    if diff.increased.is_empty() && diff.new_parts.is_empty() && kw_moved.is_empty() {
        log_info!("Keine Mail: nur gefilterte Einsteigerkarten");
        return false;
    }
    true
}

pub(crate) fn run_once(cfg: &Config, dry_run: bool) -> i32 {
    let ts = now_iso();
    let (state_path, history_path) = state_paths(cfg);
    let fp = filter_fingerprint(&cfg.tracker.commodity_keys, &cfg.tracker.name_keywords);
    let cache_path = specs_cache_path(cfg);

    // State und Specs-Cache laufen parallel zum netzlastigen Abruf: die paar
    // Millisekunden Disk+Parse verschwinden hinter den ~250–400 ms Netz.
    // Trockenlauf braucht keinen State, ein Lauf ohne Keywords keinen Cache.
    let (outcome, loaded, preloaded) = std::thread::scope(|scope| {
        let state_job = (!dry_run).then(|| scope.spawn(|| load_state(&state_path, &fp)));
        // Specs werden immer gebraucht: die Web-UI filtert nach CPU/RAM/GPU.
        let specs_job = scope.spawn(|| load_specs_cache(&cache_path));
        let outcome = fetch_clearance_parts(cfg, &cfg.tracker.commodity_keys);
        (
            outcome,
            state_job.and_then(|h| h.join().ok()).flatten(),
            specs_job.join().ok(),
        )
    });

    let outcome = match outcome {
        Ok(o) => {
            // Notebook-Typ (Maschinentyp) für die Web-UI mitschreiben.
            if let Err(e) = save_products_cache(&products_cache_path(cfg), &o.products) {
                log_warn!("Notebook-Typ-Cache nicht schreibbar: {}", e);
            }
            o
        }
        Err(e) => {
            log_error!("Abruf fehlgeschlagen – kein State-Update: {}", e);
            // Stoerungszaehler (nur echte Laeufe; Trockenlauf kehrt vorher um)
            let fsp = fail_state_path(cfg);
            let mut fst = load_fail_state(&fsp);
            fst.failures = fst.failures.saturating_add(1);
            if fst.failures >= 3 && !fst.alerted {
                fst.alerted = true;
                let subject = format!(
                    "{} Tracker gestört – API seit {} Läufen nicht erreichbar",
                    cfg.mail.subject_prefix, fst.failures
                );
                let text = format!(
                    "Störungsmeldung des Lenovo-Mainboard-Trackers.\n\n\
                     Die Lenovo-API ist seit {} aufeinanderfolgenden Läufen\
                     (≈{} Minuten) nicht erreichbar.\nLetzter Fehler: {}\n\n\
                     Der Cron versucht es weiter; bei Erholung folgt eine Meldung.\n\
                     Log: pct exec <VMID> -- tail /var/log/lenovo-tracker.log\n",
                    fst.failures,
                    fst.failures * 10,
                    e
                );
                let html = format!(
                    "<html><body style='margin:0;padding:0;background:#eef1f4'>\
<table role='presentation' width='100%' cellpadding='0' cellspacing='0' bgcolor='#eef1f4'><tr><td align='center' style='padding:24px 8px'>\
<table role='presentation' width='600' cellpadding='0' cellspacing='0' style='width:600px;max-width:600px;background:#ffffff;border:1px solid #e3e6ea;border-radius:8px;overflow:hidden'>\
<tr><td bgcolor='#e2231a' style='background:#e2231a;padding:10px 24px;font-family:Arial,Helvetica,sans-serif;font-size:11px;letter-spacing:2px;color:#ffffff;font-weight:bold'>LENOVO CLEARANCE-TRACKER</td></tr>\
<tr><td style='padding:24px 24px 10px 24px;font-family:Arial,Helvetica,sans-serif'><div style='font-size:22px;line-height:1.3;font-weight:bold;color:#111111'>Störung – API nicht erreichbar</div><div style='font-size:14px;color:#555555;margin-top:8px'>Seit {} Läufen (≈{} Minuten) kein erfolgreicher Abruf. Der Cron versucht es weiter.</div></td></tr>\
<tr><td style='padding:8px 24px 8px 24px'><table role='presentation' width='100%' cellpadding='0' cellspacing='0' style='border-collapse:collapse'>\
<tr><td style='{}'>Fehler</td><td style='{};text-align:right'>{}</td></tr>\
<tr><td style='{};border-bottom:none'>Geprüft</td><td style='{};text-align:right;border-bottom:none'>{}</td></tr>\
</table></td></tr>\
<tr><td bgcolor='#f7f8fa' style='background:#f7f8fa;padding:14px 24px;border-top:1px solid #e3e6ea;font-family:Arial,Helvetica,sans-serif;font-size:11px;color:#8a8f98'>Störungsmeldung – kein Bestandswechsel</td></tr>\
</table></td></tr></table></body></html>",
                    fst.failures, fst.failures * 10,
                    MAIL_CELL, MAIL_CELL, esc(&e.to_string()),
                    MAIL_CELL, MAIL_CELL, esc(&now_iso())
                );
                if let Ok(m) = compose_mail(cfg, subject, text, html) {
                    match deliver_mail(cfg, &m) {
                        Ok(_) => log_info!("Störungsmail gesendet ({} Läufe)", fst.failures),
                        Err(err) => log_warn!("Störungsmail fehlgeschlagen: {}", err),
                    }
                }
            }
            if let Err(err) = save_fail_state(&fsp, &fst) {
                log_warn!("FailState konnte nicht geschrieben werden: {}", err);
            }
            return 2;
        }
    };

    // Preise kommen aus demselben Abruf (kein Diff-Kriterium, nur für Mail/UI).
    let mut price_map = outcome.prices;
    let all_map = outcome.boards;
    if all_map.is_empty() {
        log_warn!(
            "Keine Mainboards in der API-Response (Filter: {:?}, Keywords: {:?})",
            cfg.tracker.commodity_keys,
            cfg.tracker.name_keywords
        );
    }
    // Tech-Daten für alle Teile: die Web-UI filtert nach CPU/RAM/GPU, die
    // Mails nutzen denselben Cache (einmalig je Teil, danach ohne Netz).
    let mut specs = match load_specs_for_all(cfg, &all_map, preloaded) {
        Ok(s) => s,
        // Ohne Keywords hängen nur die UI-Facetten an den Specs – kein Grund,
        // den Lauf abzubrechen. Mit Keywords bleibt der Abruf Pflicht: sonst
        // würden Teile stillschweigend aus der Mail-Liste fallen.
        Err(e) if cfg.tracker.name_keywords.is_empty() => {
            log_warn!("Tech-Daten nicht abrufbar ({}) – neue Teile ohne Specs", e);
            load_specs_cache(&cache_path)
        }
        Err(e) => {
            log_error!(
                "Keyword-/Specs-Filter fehlgeschlagen – kein State-Update: {}",
                e
            );
            return 2;
        }
    };
    // Volle Sicht (Web-UI) und Mail-Sicht (Keywords) teilen sich den Abruf.
    let all_prices = price_map.clone();
    let mut new_map = all_map.clone();
    retain_keyword_matches(cfg, &mut new_map, &specs);
    let count = new_map.len();
    // Nur Preise der Teile behalten, die der Filter durchlässt – die Liste
    // enthält sonst hunderte IDs, die weder Mail noch State brauchen.
    price_map.retain(|id, _| new_map.contains_key(id));

    if dry_run {
        let (total, _) = compute_totals(&new_map);
        log_info!(
            "DRY-RUN: {} Mainboard-Teile, Summe {} Stück",
            count,
            fmt_de(total)
        );
        return 0;
    }

    let new_kw = keyword_stats(&new_map, &specs, &cfg.tracker.name_keywords);

    // Kein State = Baseline: Bestand festschreiben, keine Mail.
    let Some(loaded) = loaded else {
        let (total, _) = compute_totals(&new_map);
        // Specs werden für den Baseline-Lauf nicht mehr gebraucht.
        drop(specs);
        let empty = BTreeMap::new();
        let snap = Snapshot {
            ts: &ts,
            total,
            count,
            filter: &fp,
            kw: &new_kw,
            parts: &new_map,
            gone: &empty,
            prices: &price_map,
        };
        if let Err(e) = persist(&state_path, &history_path, &snap) {
            log_error!("State konnte nicht geschrieben werden: {}", e);
            return 2;
        }
        write_tracked(cfg, &ts, &all_map, &all_prices);
        log_info!(
            "Baseline gesetzt: {} Stück über {} Mainboard-Teile",
            fmt_de(total),
            count
        );
        return 0;
    };
    let LoadedState {
        parts: old_map,
        kw: old_kw,
        gone: old_gone,
        prices: old_prices,
    } = loaded;
    let old_count = old_map.len();
    // Preise sind kein Diff-/Mail-Kriterium, aber die UI zeigt sie aus dem
    // State – ändern sie sich, wird der State neu geschrieben.
    let price_changed = old_prices != price_map;

    let mut diff = build_diff(&old_map, &new_map, &old_gone);
    // new_total ist exakt die Summe aus compute_totals() – kein zweiter Durchlauf.
    let total = diff.new_total;
    // Für den Keyword-Vergleich zählen zurückgekehrte Teile nicht mit: sie
    // standen vorher schon im Bestand, ihre Rückkehr ist keine Bewegung. Sonst
    // meldet eine unvollständige Antwort später eine falsche Keyword-Mail.
    let mut kw_compare = new_kw.clone();
    if !old_gone.is_empty() {
        let returned: BTreeMap<String, PartState> = new_map
            .iter()
            .filter(|(id, _)| old_gone.contains_key(*id))
            .map(|(id, p)| (id.clone(), p.clone()))
            .collect();
        for (kw, st) in keyword_stats(&returned, &specs, &cfg.tracker.name_keywords) {
            if let Some(slot) = kw_compare.get_mut(&kw) {
                slot.parts = slot.parts.saturating_sub(st.parts);
                slot.stock = (slot.stock - st.stock).max(0);
            }
        }
    }
    let kw_moved = keyword_changes(&old_kw, &kw_compare);
    // `build_diff` hat den Vergleich im selben Pass erledigt.
    let unchanged = !diff.changed && !price_changed;
    // Ein großer Rückgang wird erst nach Bestätigung im nächsten Lauf übernommen:
    // eine einzelne unvollständige Antwort darf die Baseline nicht senken – sonst
    // meldet der Lauf danach lauter falsche Anstiege.
    let pending = pending_path(cfg);
    if suspicious_drop(old_count, count) {
        if load_pending(&pending).is_some() {
            log_info!(
                "Rückgang bestätigt ({} → {} Teile) – neuer Stand wird übernommen",
                old_count,
                count
            );
            clear_pending(&pending);
        } else {
            if let Err(e) = save_pending(
                &pending,
                &Pending {
                    parts: count,
                    total,
                    ts: ts.clone(),
                },
            ) {
                log_warn!("Pending-Marker nicht schreibbar: {}", e);
            }
            log_warn!(
                "Verdächtiger Rückgang: {} → {} Teile, {} → {} Stück – State bleibt, nächster Lauf bestätigt",
                old_count, count, fmt_de(diff.old_total), fmt_de(total)
            );
            reset_fail_state(cfg);
            return 0;
        }
    } else if pending.exists() {
        clear_pending(&pending);
    }
    // Verschwundene Teile mit ihrem letzten Bestand behalten (siehe guard.rs) –
    // erst nach der Sperre, vorher wird das Ergebnis verworfen.
    let gone_now = next_gone(&old_map, &new_map, &old_gone, &ts);
    // Die Web-UI führt den kompletten Bestand getrennt vom Mail-State – erst
    // nach der Guard-Prüfung, damit ein verdächtiger Rückgang sie nicht leert.
    write_tracked(cfg, &ts, &all_map, &all_prices);
    drop(all_prices);
    drop(all_map);
    // Beide alten Stände werden ab hier nicht mehr gebraucht: vor dem
    // Mail-/State-Aufbau freigeben, damit der Peak-RAM nicht zwei komplette
    // Bestände plus Mail-Strings gleichzeitig hält.
    drop(old_map);
    drop(old_kw);
    // Der neue Stand steht fest: einmal bündeln – der Mail-Pfad und der
    // Kein-Mail-Pfad schreiben denselben State.
    let snapshot = Snapshot {
        ts: &ts,
        total,
        count,
        filter: &fp,
        kw: &new_kw,
        parts: &new_map,
        gone: &gone_now,
        prices: &price_map,
    };
    // Mail nur bauen, wenn es etwas zu melden gibt – die Specs braucht auch
    // der MX-/Quadro-T-Filter. Bleibt danach nichts übrig, fällt der Lauf in
    // den gemeinsamen Pfad unten (State/Verlauf werden trotzdem geschrieben).
    let mut mail_specs = BTreeMap::new();
    let mut send_mail = should_mail(&diff, &kw_moved);
    if send_mail {
        // Tech-Daten für alle gemailten Teile: gecachte Zeilen kommen ohne
        // Netz, nur der Rest geht als Batch-POSTs raus (gechunkt).
        let mut spec_ids: Vec<&str> = diff.increased.iter().map(|i| i.id.as_str()).collect();
        spec_ids.extend(diff.new_parts.iter().map(|p| p.0.as_str()));
        mail_specs = specs_for_mail(cfg, &spec_ids, &mut specs);
        send_mail = filter_mail_parts(&mut diff, &mail_specs, &kw_moved);
    }
    if send_mail {
        // Stärkste Teile zuerst (GPU-/CPU-Klasse aus den Specs).
        sort_by_performance(&mut diff, &mail_specs);
        // Der komplette Cache wird für Mail/State nicht mehr gebraucht.
        drop(specs);
        let subject = build_subject(&cfg.mail.subject_prefix, &diff, &kw_moved);
        let label = filter_label(&cfg.tracker.commodity_keys, &cfg.tracker.name_keywords);
        let currency = currency_symbol(&cfg.api.url);
        let view = MailView {
            diff: &diff,
            page_url: PAGE_URL,
            checked_at: &ts,
            specs: &mail_specs,
            prices: &price_map,
            currency,
            filter_label: &label,
            kw_moved: &kw_moved,
        };
        let (text, html) = build_mail(&view);
        let msg = match compose_mail(cfg, subject.clone(), text, html) {
            Ok(m) => m,
            Err(e) => {
                log_error!(
                    "Mail konnte nicht erstellt werden – Anstieg wird erneut gemeldet: {}",
                    e
                );
                return 2;
            }
        };
        if let Err(e) = deliver_mail(cfg, &msg) {
            // State NICHT aktualisieren: der Anstieg wird beim nächsten Lauf
            // erneut gemeldet (Duplikat-Alarm schlägt verlorenen Alarm).
            log_error!(
                "Mailversand fehlgeschlagen – Anstieg wird erneut gemeldet: {}",
                e
            );
            return 2;
        }
        if let Err(e) = persist(&state_path, &history_path, &snapshot) {
            log_error!("State konnte nicht geschrieben werden: {}", e);
            return 2;
        }
        log_info!("Mail gesendet: {}", subject);
        reset_fail_state(cfg);
        return 0;
    }

    drop(specs);
    reset_fail_state(cfg);
    if unchanged {
        // Bestand exakt unverändert (häufigster Lauf): state.json nicht neu
        // serialisieren – der Inhalt wäre identisch, nur last_check würde sich
        // ändern. Nur der Verlauf wird fortgeschrieben.
        if let Err(e) = append_history(&history_path, &ts, total, count) {
            log_error!("Verlauf konnte nicht geschrieben werden: {}", e);
            return 2;
        }
    } else if let Err(e) = persist(&state_path, &history_path, &snapshot) {
        log_error!("State konnte nicht geschrieben werden: {}", e);
        return 2;
    }
    log_info!(
        "Keine Mail (vorher {} → jetzt {})",
        fmt_de(diff.old_total),
        fmt_de(diff.new_total)
    );
    0
}
