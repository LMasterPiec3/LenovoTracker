# Lenovo Clearance-Sale API – Reverse-Engineering-Notizen

**Stand: 2026-11-19** — alle Angaben sind empirisch verifiziert (≈ 45 Requests
an diesem Tag via `curl-impersonate` v2.3.2 aus CT 105/104, Auswertung mit
`python3` auf dem Host). Die API ist undokumentiert/privat —
alles hier kann sich jederzeit ändern.

---

## 1. Endpunkt

```
GET https://support.lenovo.com/{country}/{lang}/api/v4/lbom/clearanceParts/get
```

- Nur **GET** (`POST` → `405 Method Not Allowed`, plain-text Body;
  `HEAD` → `200` mit Headern)
- Die Pfad-Segmente `{country}/{lang}` bestimmen Region, Sprache und Währung
  der Daten (siehe §9) — sie sind kein Filter, sondern wählen den Datensatz
- Backend: Kubernetes-Pod-Familie `eservice-lenovo-bom-*` (aus
  `responseHost`), vorgeschaltet ist ein Envoy-Mesh (`x-envoy-upstream-service-time`,
  `x-mesh-version: 2026`) und Akamai (siehe §2)
- Entdeckt ursprünglich über Browser-DevTools (Netzwerkanalyse der
  Clearance-Page); die Page-HTML selbst referenziert den Endpunkt nicht
  (SPA lädt Chunks dynamisch, `chunk-common.*.js` enthält ihn nicht)

## 2. Bot-Schutz (Akamai Bot Manager)

| Client | Ergebnis |
|---|---|
| klassisches `curl` (gleiche Egress-IP!) | **HTTP 403** |
| busybox `wget` | **HTTP 403** |
| WebFetch/requests (laut Design-Doc 2026-11-18) | **HTTP 403** |
| `curl-impersonate --impersonate chrome99` | 200 |
| `curl-impersonate --impersonate chrome110` | 200 |
| `curl-impersonate --impersonate chrome124` (Produktivstand) | 200 |
| `curl-impersonate --impersonate chrome131` | 200 |
| `curl-impersonate --impersonate edge101` | 200 |
| `curl-impersonate --impersonate safari17_0` | 200 |
| `curl-impersonate --impersonate firefox133` | 200 |

- Der 403 ist **fingerprint-basiert**, nicht IP-basiert: klassisches curl von
  derselben IP → 403, Imitation von derselben IP → 200
- Bei 403: keine Retry-Werde (`--once` bricht ab); der Tracker loggt
  „HTTP 403 – Akamai blockiert den Abruf"
- Akamai setzt bei jedem Aufruf Bot-Manager-Cookies (`_abck`, `ak_bmsc`,
  `bm_sz`) plus Session-Cookies (`Lenovo_SessionID`, `LSESSION`-Reset,
  `esupport#lang1`) — **alle werden nicht gebraucht**: der erste nackte
  Imitations-Request funktioniert ohne Cookie-Jar
- **`Referer` und `Accept` sind für Akamai irrelevant** (beide weggelassen →
  weiterhin 200); `Accept` steuert nur das Format (§3)
- Rate-Limiting: bei ~45 Requests über ~30 min von einer IP keine Symptome;
  aggressives Testen bewusst unterlassen

## 3. Content-Negotiation (`Accept`)

| `Accept`-Header | HTTP | Body | Größe (DE) |
|---|---|---|---|
| `application/json` | 200 | JSON | **3.347.211 B** |
| `*, *` (`*/*`) | 200 | JSON | 3.347.211 B |
| `application/xml` | 200 | **XML** | 4.755.946 B |
| *(kein Accept-Header)* | 200 | **XML** | 4.755.946 B |
| `text/html` | **406** | leer | 0 |

- **Default-Serialisierung ist XML** (`<Response><code>0</code>…<data><id>…`).
  Das erklärt die alte Tracker-Fehlermeldung „vermutlich XML ohne
  Accept-Header" — der Tracker sendet `Accept: application/json, text/plain,
  */*` und bekommt JSON
- XML-Variante enthält dieselben Daten (field-per-element), nur größer

## 4. Query-Parameter: serverseitige Filter gibt es NICHT

Getestet gegen den Live-Endpunkt (jede Zeile eigener Request, Vergleich der
Body-Größe gegen die Referenz 3.347.211 B = Byte-identisch):

| Parameter | Ergebnis |
|---|---|
| `keyword=system boards`, `keyword=mainboard` | ignoriert |
| `q=`, `searchText=`, `search=` | ignoriert |
| `commodityKey=system boards`, `category=`, `filter=`, `name=` | ignoriert |
| `commodity=`, `commodityGroup=`, `categoryKey=` | ignoriert |
| `page=1`, `pageSize=10`, `limit=10`, `offset=10`, `rows=10`, `start=10` | ignoriert (keine Pagination) |
| `sort=stock`, `order=desc` | ignoriert |
| `lang=en`, `locale=en-US` | ignoriert |
| `mtm=`, `partId=`, `ids=` | ignoriert |
| `country=us` (auf dem `/de/de`-Pfad) | **Sonderfall:** HTTP 200, aber leere Liste `data:[]` (154 B) — Parameter wird gelesen, liefert aber keinen nutzbaren Filter |

**Fazit:** Weder Keyword- noch Commodity- noch Mengen-/Paginierungs-Filter
existieren serverseitig. Auch Lenovos eigenes Commodity-Dropdown auf der
Clearance-Page filtert rein client-seitig. **Jeder Filter muss client-seitig
erfolgen** (so macht es der Tracker).

## 5. Response-Envelope

```json
{
  "code": 0,                      // 0 = OK (int)
  "costMillis": 197,              // Server-Verarbeitungszeit (beobachtet 6–224 ms)
  "requestId": "1AC6C206A23D",    // 12 Hex-Zeichen, pro Request
  "responseHost": "eservice-lenovo-bom-578675594c-rjbxt",  // Backend-Pod
  "msg": { "desc": "Success", "value": null },
  "data": [ … ]                   // Array der Teile
}
```

- `code != 0` wurde nie beobachtet; Fehlfälle (leere Liste) kommen als
  `code: 0` mit `data: []` daher (z. B. `jp/ja`, `country=us`)
- Antwort-Header: `HTTP/2`, `content-type: application/json`,
  `content-encoding: gzip` (Client sendet `--compressed`),
  `server-timing: traceparent`, HSTS-`preload`

## 6. Part-Schema (14 Felder, alle immer vorhanden)

Beispiel (DE, 2026-11-19):

```json
{
  "id": "01YT022",
  "country": "DE",
  "language": "DE",
  "commodityId": "PL",
  "commodityKey": "system boards",
  "commodityVal": "SYSTEM BOARDS",
  "name": "i3-7100U 3 MB NvPro YT WIN(HDCP)",
  "products": ["20J1", "20J2", "20J3"],
  "partSaleType": "Local_Promotion",
  "price": 855.63,
  "promotionPrice": 59.89,
  "stock": 2,
  "saleCount": 0,
  "updateTime": "2026-11-19T16:04:25.223Z"
}
```

| Feld | Typ | Bedeutung |
|---|---|---|
| `id` | string | Teilenummer (meist `^[A-Z0-9]{8,12}$`, 7.426/9.255; auch kürzer, z. B. `01YT022`) |
| `country` / `language` | string | Region/Sprache — spiegelt die URL-Pfad-Segmente (`DE`/`DE`, `US`/`EN`, `GB`/`EN`) |
| `commodityId` | string | Kurzcode der Warengruppe (`PL` = system boards, `CK` = cover with keyboard, …) |
| `commodityKey` | string | **Filterfeld des Trackers** — lowercase, übersetzungsstabil (`system boards`); `commodityVal` dagegen ist großgeschrieben/übersetzt |
| `name` | string | Bezeichnung; Sprache folgt dem Locale (GB: Präfix `Lenovo BDPLANAR …`) |
| `products` | string[] | kompatible Machine-Type-Models (MTM, z. B. `20J1`); 7.685 Teile haben ≥ 1 Eintrag, Ø 1,62, max 271; 1.570 leer |
| `partSaleType` | string | `Local_Promotion` (9.254/9.255) oder `Local` (1) |
| `price` | float | Listenpreis (33,32–6.871,05 DE) — **keine Währung im Schema**, Währung folgt dem Locale |
| `promotionPrice` | float | Sale-Preis (12,76–322,94 DE; Boards 15,07–144,85) |
| `stock` | int | verfügbare Menge (Summe Boards DE: 16.465) |
| `saleCount` | int | beobachtet: 0 — vermutlich verkaufte Stück während der Aktion |
| `updateTime` | string | RFC3339 mit Millisekunden — **Batch-Stempel**: alle Teile einer Region tragen denselben Zeitstempel ±13 ms (DE: 16:04:25.221–.234Z) |

## 7. Warengruppen (Commodity-Verteilung, DE 2026-11-19)

23 Commodities, 9.255 Teile gesamt:

| `commodityId` | `commodityKey` | Teile |
|---|---|---|
| CK | cover with keyboard | 4.968 |
| **PL** | **system boards** | **3.125** |
| KI | keyboard internal | 385 |
| KB | keyboards external | 248 |
| HS | heat sink | 187 |
| FA | fans | 113 |
| LF | lcd assembly | 69 |
| PU | processors | 42 |
| CL | wireless lan adapter | 37 |
| CW | wireless wan adapter | 15 |
| — | optical drives | 13 |
| — | ac adapter | 12 |
| — | hard drives | 8 |
| — | pointing device | 8 |
| — | docking stations | 8 |
| — | lcd panel | 5 |
| — | drive external hdd | 4 |
| — | cables internal | 2 |
| — | power supplies internal | 2 |
| — | whole unit exchange | 1 |
| — | misc internal | 1 |
| — | rechargeable battery internal | 1 |
| — | reusable item reusable tape | 1 |

## 8. Stock-Semantik

- `stock` ist **immer int ≥ 1** (beobachtet 2026-11-19: min 1, max 1.103,
  keine Nullen, Σ über alle Warengruppen 45.574)
- **Vermutung** (nicht beweisbar aus einem Snapshot): ausverkaufte Teile
  (stock = 0) verschwinden aus der Liste, statt als `stock: 0` zu erscheinen
  — die Listengröße ändert sich täglich (9.261 Teile am 2026-11-18 → 9.255
  am 2026-11-19)
- Historisch gab es garbage/string-`stock`-Werte (deshalb hat der Tracker
  den toleranten `StockField`-Deserializer); am 2026-11-19 war jeder Wert sauber int

## 9. Regionen / Locales

| Pfad | HTTP | Größe (JSON) | Teile | Boards | Σ Board-Stock | Bemerkung |
|---|---|---|---|---|---|---|
| `/de/de/` | 200 | 3,35 MB | 9.255 | 3.125 | 16.465 | Produktivstand des Trackers |
| `/gb/en/` | 200 | 4,70 MB* | 9.255 | 3.125 | 16.463 | fast identische Liste, Σ stock weicht leicht ab (−2) |
| `/us/en/` | 200 | 0,81 MB | 2.287 | 1.425 | 21.003 | komplett anderer, kleinerer Datensatz |
| `/fr/fr/`, `/es/es/`, `/it/it/`, `/nl/nl/` | 200 | ~4,7 MB* | — | — | — | EU-Listen, Größe wie ohne Accept-Header |
| `/jp/ja/` | 200 | 200 B | 0 | 0 | 0 | `data: []` (keine Clearance-Aktion) |
| `/cn/zh/` | — | — | — | — | — | Verbindung scheitert (000; eigene Infra?) |
| `/xx/xx/` | **302** | 105 B | — | — | — | Redirect |

\* *die ~4,7 MB-Werte stammen aus Läufen ohne `Accept`-Header → XML-Serialisierung
(§3); als JSON wären die Listen kleiner.*

- **Währung ohne Schema-Feld:** dasselbe Teil `01YT022` kostet DE 855,63 /
  GB 723,52 / US 256,46 — EUR/GBP/USD folgen implizit aus dem Locale
- **Name-Sprache folgt dem Locale:** DE „i3-7100U 3 MB NvPro YT WIN(HDCP)",
  GB „Lenovo BDPLANAR i3-7100U 3MB NvPro YT WIN(HDCP)"
- Jede Region hat ihren eigenen Batch-`updateTime` (DE 16:04, GB 16:24,
  US 16:30 UTC am 2026-11-19) — Regionen refreshen zu unterschiedlichen Zeiten
- DE und GB haben dieselbe Teileliste, aber **getrennte Lagerbestände**
  (16.465 vs. 16.463)

## 10. Detail-Endpunkt: Technische Daten (`lbom/parts/get`)

Die Clearance-Response enthält **keine** Tech-Daten — die Page enrichiert die
Teile über einen zweiten Endpunkt (aus dem JS-Chunk `lite-parts-lookup` reverse-engineered):

```
POST https://support.lenovo.com/de/de/api/v4/lbom/parts/get
Content-Type: application/json

{"country":"DE","language":"DE","ids":["02DC237","5B21J77190"]}
```

- **POST statt GET**; `ids` ist ein Batch-Array (5 IDs getestet → 16,6 KB
  Response ≈ 3,3 KB pro Teil; Obergrenze nicht erprobt)
- Ohne Locale-Pfad im URL: `302` Redirect zu `/de/en/…` (Server ergänzt Locale)
- Akamai davor wie in §2 (verifiziert nur mit curl-impersonate)

### Response (gleicher Envelope, viel reichere Part-Objekte)

```json
{
  "id": "02DC237",
  "name": "AMD R7 2700U mit Vega10 WIN YT",
  "basicName": "…", "bacCode": "1", "cruTier": "10", "notFru": false,
  "parentId": "…", "motionless": false, "source": "Windchill.TPG.MM",
  "imageNames": ["02DC237_A.jpg", "…"],
  "imageUrls": ["https://download.lenovo.com/Images/Parts/02DC237/02DC237_A.jpg", "…"],
  "imagesUpdated": "2022-02-19T05:09:47.725",
  "commodityKey": "system boards",
  "products": ["20KU"],
  "substitutes": [{"type": "p", "id": "SBB0Q57267", …}, {"type": "s", "id": "02DC235", …}],
  "bundles": [], "externalIds": …, "localizations": …, "supplierInfos": …,
  "screen": …, "redemption": …,
  "salability": {
    "partSaleType": "Local_Promotion", "promotionId": "202608269377",
    "clearance": true, "isCanSalability": true,
    "stock": 3, "realStock": 3, "oversellStock": 0,
    "value": 517.16, "formattedValue": "517,16€",
    "tax": 0.19, "discount": 0.92,
    "includingTaxAndDiscountValue": 46.77, "formattedIncludingTaxAndDiscountValue": "46,77€",
    "currency": "EUR", "saleCount": null, "taxRate": { "materialType": "ZERS", "unspsCode": "43201513", "netWeight": "0.110", … }
  },
  "specs": {
    "deleted": false, "type": null,
    "specsInfo": "DPK: WIN;;CPU: AMDR72700U;;GPU Type: UMA;;",
    "specsAttributes": [
      {"displayTitle": "DPK", "value": "WIN", "transKey": "dpk", "suffix": null},
      {"displayTitle": "CPU", "value": "AMDR72700U", "transKey": "cpu", "suffix": null},
      {"displayTitle": "GPU Type", "value": "UMA", "transKey": "gpu type", "suffix": null}
    ]
  }
}
```

- **`specs.specsInfo`** ist der fertige `;;`-getrennte Anzeige-String — genau das,
  was die Page unter „Technische Daten" zeigt (Werte dort teils ohne Leerzeichen,
  z. B. `AMDR72700U`)
- **`specsAttributes[]`** ist die strukturierte Variante (`displayTitle`/`value`/
  `transKey`); die deutschen Labels kommen aus dem Translations-Bundle
  (`"gpu type" → "GPU-Typ"`, `"specs" → "Technische Daten"`)
- Attribut-Vokabular (Stichprobe 60 Boards, 2026-11-19): `dpk` (49×), `cpu` (47×),
  `gpu type` (48×), `amt` (23×), **`dram` (18× — RAM, wenn vorhanden)**,
  `gpu name` (20×), `wlan` (2×). CPU-Werte sind komprimiert (`intelI79750H`,
  `AMDR76850H-PRO`) — der Tracker beautifiziert sie (→ „Intel Core i7-9750H“,
  „AMD Ryzen 7 6850H PRO“); GPU Type `DIS`/`UMA` wird zu „diskret“/„integriert“;
  RAM kommt aus dem `dram`-Attribut oder wird aus dem Namen abgeleitet
  („8 GB“/„16G“-Tokens, mit VRAM-Ausschluss bei diskreten GPUs)
- Bekannte Attribut-Schlüssel aus dem Frontend: `DPK`, `CPU`, `GPU_TYPE`,
  `GPU_NAME`, `DRAM`, `AMT`, `WLAN`, `HEATSINK`, `SIZE_INCH`, `RESOLUTION`,
  `SCREEN_TYPE`, `TOUCH`, `PANEL_TYPE`, `BRIGHTNESS`
- `salability.stock` spiegelt den Clearance-Bestand (hier 3 = wie in der
  Clearance-Response); der Listenpreis steht in `value`, der bezahlte
  Einzelpreis (inkl. MwSt. abzgl. Rabatt) in `includingTaxAndDiscountValue`
- `substitutes` enthält Ersatzteilverweise (`type: "p"`/`"s"` + Teilenummern)
- Batch-Größe: 60 IDs je POST verifiziert (190 KB Response, `costMillis` ~300);
  mehr nicht getestet — der Tracker nutzt max. 40

### Weitere im Frontend gefundene (hier ungenutzte) Endpoints

- `POST /{locale}/api/v4/lbom/parts/get3rdParty` — erwartet ebenfalls `ids`
  (bei `id`-Feld: `code 103 "ids: must not be null"`)
- `POST/GET /api/v4/upsell/parts/fuzzyGet`, `fuzzySortGet`,
  `fuzzySortGet3rdParty`, `getMtmFromProductService`,
  `getPartsLookupTitle`, `getPartsVideo`

## 11. Relevanz für den Tracker (`rust-tracker`)

- Abruf über `curl-impersonate`-Subprozess mit `--impersonate chrome124`
  (Akamai-Umgehung, §2), Body geht per `-o` in eine Temp-Datei und wird mit
  `serde_json::from_reader` gestreamt — der ~3,3-MB-Body liegt nie komplett im RAM
- `Accept: application/json, text/plain, */*` ist **Pflicht für JSON** (§3);
  ohne sie käme XML und der Parse würde fehlschlagen
- `Referer`/Cookies sind nicht nötig (§2), werden aber zur Authentizität
  mitgeschickt
- Serverseitige Filter existieren nicht (§4) → der `commodity_keys`-Client-
  Filter (default `["system boards"]`) ist der einzig mögliche Filter;
  Keyword-Filterung innerhalb der Boards wäre ebenfalls rein client-seitig
  umsetzbar (optional, Stand: nicht eingebaut)
- Abbruch-Kriterien des Trackers: `code != 0`, leere `data`-Liste, HTTP 403
  (kein Retry), HTTP ≥ 400 — siehe `fetch_once()` in `rust-tracker/src/main.rs`
- Diff-Basis ist ausschließlich `id` + `stock` der gefilterten Teile; alle
  anderen Felder (Preise, `products`, `updateTime`) werden derzeit ignoriert
- **Bei Mail:** die Top-20 je Diff-Liste (Anstiege + Neu im Sale) werden per
  Batch-POST gegen `lbom/parts/get` angereichert und `specsInfo` in der Mail
  unter dem Teil angezeigt (§10); fehlschlagender Enrichment ist nicht fatal

## 12. Offene Punkte / nicht untersucht

- Währungsfeld existiert nicht — Währungssicherheit nur über Locale-Implikation
- Verwandte Endpoints: der Detail-Endpunkt (`lbom/parts/get`) wurde aus dem
  `lite-parts-lookup`-App-Chunk reverse-engineert (§10); die upsell-Endpoints
  sind dort ebenfalls sichtbar, aber nicht näher untersucht. Der
  `supportapi.lenovo.com`-Host taucht in der Page auf, ohne dass ein
  konkreter Pfad greifbar war
- Rate-Limit-Grenzen: nicht ausgereizt (absichtlich)
- `saleCount`-Semantik: nur 0-Werte beobachtet
- Batch-Obergrenze von `lbom/parts/get`-`ids` (5 getestet; der Tracker nutzt
  max. 40 je POST)

## 13. Methodik / Reproduktion

```bash
# Beispiel-Request (im Container 105):
/opt/lenovo-tracker/curl-impersonate --impersonate chrome124 --compressed -s \
  --max-time 90 -o body.json \
  -H 'Accept: application/json' \
  -H 'Referer: https://support.lenovo.com/de/de/parts-lookup/clearance-sale' \
  'https://support.lenovo.com/de/de/api/v4/lbom/clearanceParts/get'
```

- Parameter-Matrix: `bash lenovo-tracker-setup.sh --probe 105` (probiert die
  Query-Varianten und zählt Teile/Boards im Body)
- Schema-/Verteilungs-Analysen: Body per `pct pull` auf den Host, dort mit
  `python3` (json-Modul) ausgewertet; alle Zahlen dieses Dokuments sind
  Snapshots vom 2026-11-19
