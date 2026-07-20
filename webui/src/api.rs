//! Роутинг HTTP-API поверх [`Modem`].

use crate::http::{Request, Response};
use crate::json::{form_get, parse_form, Json};
use crate::modem::{Caps, LockState, Modem, Neighbor, Sample, Signal};
use std::sync::Arc;

pub const INDEX_HTML: &str = include_str!("../web/index.html");
pub const APP_JS: &str = include_str!("../web/app.js");
pub const STYLE_CSS: &str = include_str!("../web/style.css");

pub fn route(modem: &Arc<Modem>, req: &Request) -> Response {
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/") => Response::html(INDEX_HTML),
        ("GET", "/app.js") => Response::asset("application/javascript; charset=utf-8", APP_JS),
        ("GET", "/style.css") => Response::asset("text/css; charset=utf-8", STYLE_CSS),

        ("GET", "/api/status") => ok(status_json(modem)),
        ("GET", "/api/history") => ok(history_json(&modem.history())),
        ("GET", "/api/bands") => match modem.read_bands() {
            Ok(raw) => ok(Json::obj(vec![("raw", Json::str(raw))])),
            Err(e) => err(400, &e),
        },

        ("POST", "/api/lock/earfcn") => lock_earfcn(modem, req),
        ("POST", "/api/lock/pci") => lock_pci(modem, req),
        ("POST", "/api/unlock") => match modem.unlock() {
            Ok(()) => ok(lock_result(modem)),
            Err(e) => err(400, &e),
        },
        ("POST", "/api/reset") => match modem.reset_modem() {
            Ok(()) => ok(Json::obj(vec![(
                "message",
                Json::str("Модем перезагружается, связь восстановится через 30-60 с"),
            )])),
            Err(e) => err(400, &e),
        },
        ("POST", "/api/scan") => match modem.scan_neighbors() {
            Ok(list) => ok(Json::obj(vec![("neighbors", neighbors_json(&list))])),
            Err(e) => err(400, &e),
        },
        ("POST", "/api/bands") => set_bands(modem, req),
        ("POST", "/api/at") => raw_at(modem, req),

        ("GET", _) | ("POST", _) => err(404, "не найдено"),
        _ => err(405, "метод не поддерживается"),
    }
}

// ---------------------------------------------------------------------------
// Обработчики
// ---------------------------------------------------------------------------

fn lock_earfcn(modem: &Arc<Modem>, req: &Request) -> Response {
    let form = parse_form(&req.body);
    let earfcn = match parse_u16(form_get(&form, "earfcn"), "EARFCN", 65535) {
        Ok(v) => v,
        Err(e) => return err(400, &e),
    };
    match modem.lock_earfcn(earfcn) {
        Ok(()) => ok(lock_result(modem)),
        Err(e) => err(400, &e),
    }
}

fn lock_pci(modem: &Arc<Modem>, req: &Request) -> Response {
    let form = parse_form(&req.body);
    let earfcn = match parse_u16(form_get(&form, "earfcn"), "EARFCN", 65535) {
        Ok(v) => v,
        Err(e) => return err(400, &e),
    };
    let pci = match parse_u16(form_get(&form, "pci"), "PCI", 503) {
        Ok(v) => v,
        Err(e) => return err(400, &e),
    };
    match modem.lock_pci(earfcn, pci) {
        Ok(()) => ok(lock_result(modem)),
        Err(e) => err(400, &e),
    }
}

fn set_bands(modem: &Arc<Modem>, req: &Request) -> Response {
    let form = parse_form(&req.body);
    let mask = match form_get(&form, "mask") {
        Some(m) if !m.is_empty() => m,
        _ => return err(400, "не указана маска бэндов"),
    };
    match modem.set_bands(mask) {
        Ok(()) => ok(Json::obj(vec![(
            "message",
            Json::str("Маска записана, требуется перезагрузка модема"),
        )])),
        Err(e) => err(400, &e),
    }
}

fn raw_at(modem: &Arc<Modem>, req: &Request) -> Response {
    let form = parse_form(&req.body);
    let cmd = match form_get(&form, "cmd") {
        Some(c) if !c.trim().is_empty() => c.trim(),
        _ => return err(400, "пустая команда"),
    };
    match modem.send_raw(cmd) {
        Ok(r) => ok(Json::obj(vec![
            ("raw", Json::str(r.raw)),
            ("body", Json::str(r.body)),
            ("ok", Json::Bool(r.ok)),
            ("error", Json::opt_str(r.error)),
        ])),
        Err(e) => err(400, &e),
    }
}

// ---------------------------------------------------------------------------
// Сериализация
// ---------------------------------------------------------------------------

fn status_json(modem: &Arc<Modem>) -> Json {
    let lock = modem.read_lock().unwrap_or_default();
    Json::obj(vec![
        ("transport", Json::str(modem.transport_name())),
        ("info", Json::str(modem.info())),
        ("caps", caps_json(&modem.caps())),
        ("lock", lock_json(&lock)),
        ("signal", signal_json(&modem.last_signal())),
    ])
}

fn lock_result(modem: &Arc<Modem>) -> Json {
    let lock = modem.read_lock().unwrap_or_default();
    Json::obj(vec![
        ("lock", lock_json(&lock)),
        (
            "message",
            Json::str("Записано. Изменения вступят в силу после перезагрузки модема."),
        ),
    ])
}

fn lock_json(l: &LockState) -> Json {
    Json::obj(vec![
        ("earfcn", Json::opt_int(l.earfcn.map(|v| v as i64))),
        ("pciEarfcn", Json::opt_int(l.pci_earfcn.map(|v| v as i64))),
        ("pci", Json::opt_int(l.pci.map(|v| v as i64))),
        ("rawEarfcn", Json::opt_str(l.raw_earfcn.clone())),
        ("rawPci", Json::opt_str(l.raw_pci.clone())),
        ("conflict", Json::Bool(l.has_conflict())),
    ])
}

fn signal_json(s: &Signal) -> Json {
    Json::obj(vec![
        ("rsrp", Json::opt_num(s.rsrp)),
        ("rsrq", Json::opt_num(s.rsrq)),
        ("sinr", Json::opt_num(s.sinr)),
        ("rssi", Json::opt_num(s.rssi)),
        ("earfcn", Json::opt_int(s.earfcn.map(|v| v as i64))),
        ("pci", Json::opt_int(s.pci.map(|v| v as i64))),
        ("band", Json::opt_str(s.band.clone())),
        ("operator", Json::opt_str(s.operator.clone())),
        ("registered", Json::Bool(s.registered)),
    ])
}

fn caps_json(c: &Caps) -> Json {
    Json::obj(vec![
        ("efs", Json::Bool(c.efs)),
        ("serving", Json::opt_str(c.serving.clone())),
        ("neighbors", Json::opt_str(c.neighbors.clone())),
        ("bands", Json::opt_str(c.bands_query.clone())),
        ("bandsWritable", Json::Bool(c.bands_set.is_some())),
    ])
}

fn neighbors_json(list: &[Neighbor]) -> Json {
    Json::Arr(
        list.iter()
            .map(|n| {
                Json::obj(vec![
                    ("earfcn", Json::Int(n.earfcn as i64)),
                    ("pci", Json::Int(n.pci as i64)),
                    ("rsrp", Json::opt_num(n.rsrp)),
                    ("rsrq", Json::opt_num(n.rsrq)),
                ])
            })
            .collect(),
    )
}

fn history_json(samples: &[Sample]) -> Json {
    Json::obj(vec![(
        "samples",
        Json::Arr(
            samples
                .iter()
                .map(|s| {
                    Json::obj(vec![
                        ("ts", Json::Int(s.ts as i64)),
                        ("rsrp", Json::opt_num(s.rsrp)),
                        ("rsrq", Json::opt_num(s.rsrq)),
                        ("sinr", Json::opt_num(s.sinr)),
                    ])
                })
                .collect(),
        ),
    )])
}

// ---------------------------------------------------------------------------
// Помощники
// ---------------------------------------------------------------------------

fn parse_u16(raw: Option<&str>, name: &str, max: u16) -> Result<u16, String> {
    let raw = raw.map(|s| s.trim()).unwrap_or("");
    if raw.is_empty() {
        return Err(format!("не указан {}", name));
    }
    let v: u32 = raw
        .parse()
        .map_err(|_| format!("{} должен быть целым числом, получено «{}»", name, raw))?;
    if v > max as u32 {
        return Err(format!(
            "{} {} вне допустимого диапазона 0..{}",
            name, v, max
        ));
    }
    Ok(v as u16)
}

fn ok(body: Json) -> Response {
    Response::json(200, body.to_string())
}

fn err(status: u16, message: &str) -> Response {
    Response::json(
        status,
        Json::obj(vec![("error", Json::str(message))]).to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_numbers() {
        assert_eq!(parse_u16(Some("2850"), "EARFCN", 65535), Ok(2850));
        assert_eq!(parse_u16(Some(" 57 "), "PCI", 503), Ok(57));
        assert_eq!(parse_u16(Some("0"), "PCI", 503), Ok(0));
    }

    #[test]
    fn rejects_bad_numbers() {
        assert!(parse_u16(None, "EARFCN", 65535).is_err());
        assert!(parse_u16(Some(""), "EARFCN", 65535).is_err());
        assert!(parse_u16(Some("abc"), "EARFCN", 65535).is_err());
        assert!(parse_u16(Some("-1"), "EARFCN", 65535).is_err());
        assert!(parse_u16(Some("504"), "PCI", 503).is_err());
        assert!(parse_u16(Some("65536"), "EARFCN", 65535).is_err());
    }

    #[test]
    fn error_bodies_are_json() {
        let r = err(400, "плохо");
        assert_eq!(r.status, 400);
        assert_eq!(String::from_utf8(r.body).unwrap(), r#"{"error":"плохо"}"#);
    }

    #[test]
    fn lock_state_serialises_nulls_when_unset() {
        let s = lock_json(&LockState::default()).to_string();
        assert!(s.contains(r#""earfcn":null"#));
        assert!(s.contains(r#""conflict":false"#));
    }

    #[test]
    fn neighbors_serialise_in_order() {
        let list = vec![
            Neighbor {
                earfcn: 2850,
                pci: 57,
                rsrp: Some(-80.0),
                rsrq: Some(-9.0),
            },
            Neighbor {
                earfcn: 1575,
                pci: 161,
                rsrp: None,
                rsrq: None,
            },
        ];
        let s = neighbors_json(&list).to_string();
        assert!(s.starts_with(r#"[{"earfcn":2850,"pci":57,"rsrp":-80,"rsrq":-9}"#));
        assert!(s.contains(r#""rsrp":null"#));
    }
}
