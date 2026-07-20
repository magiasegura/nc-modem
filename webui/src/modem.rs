//! Модель модема: EFS-фиксация, метрики сигнала, соседние соты, бэнды.
//!
//! Набор поддерживаемых AT-команд у разных прошивок T77W968 отличается, поэтому
//! на старте выполняется проба: каждый кандидат отправляется модему, и запоминается
//! тот, что ответил осмысленно. UI показывает только подтверждённые возможности —
//! лучше честно скрыть кнопку, чем показать неработающую.

use crate::at::{AtPort, AtResponse, Transport};
use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

pub const EFS_EARFCN: &str = "/nv/item_files/modem/lte/rrc/csp/earfcn_lock";
pub const EFS_PCI: &str = "/nv/item_files/modem/lte/rrc/csp/pci_lock";

/// Сколько точек графика храним (при опросе раз в 5 с это ~1 час).
const HISTORY_CAP: usize = 720;

// ---------------------------------------------------------------------------
// Little-endian конверсия
// ---------------------------------------------------------------------------

/// 2850 -> "22,0b"
pub fn to_le16(v: u16) -> String {
    format!("{:02x},{:02x}", v & 0xff, v >> 8)
}

/// "22,0b" -> 2850. Допускает пробелы и верхний регистр.
pub fn from_le16(s: &str) -> Option<u16> {
    let parts: Vec<&str> = s.split(',').map(|p| p.trim()).collect();
    if parts.len() != 2 {
        return None;
    }
    let lo = u16::from_str_radix(parts[0], 16).ok()?;
    let hi = u16::from_str_radix(parts[1], 16).ok()?;
    if lo > 0xff || hi > 0xff {
        return None;
    }
    Some((hi << 8) | lo)
}

/// Вытащить байты из `^EFS: /nv/…/pci_lock, 22,0b,39,00`.
/// Возвращает `None`, если строки нет или в ней не hex-байты.
pub fn parse_efs_bytes(body: &str) -> Option<String> {
    let line = body
        .lines()
        .find(|l| l.to_ascii_uppercase().contains("EFS:"))?;

    // После "EFS:" идёт путь, затем первая запятая отделяет данные.
    let after_tag = line.split_once(':')?.1;
    let data = after_tag.split_once(',')?.1;

    let cleaned: String = data.chars().filter(|c| !c.is_whitespace()).collect();
    let cleaned = cleaned.to_ascii_lowercase();

    if cleaned.is_empty() {
        return None;
    }
    if !cleaned.chars().all(|c| c.is_ascii_hexdigit() || c == ',') {
        return None;
    }
    Some(cleaned)
}

// ---------------------------------------------------------------------------
// Состояние
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq)]
pub struct LockState {
    /// Значение из earfcn_lock.
    pub earfcn: Option<u16>,
    /// Значения из pci_lock.
    pub pci_earfcn: Option<u16>,
    pub pci: Option<u16>,
    /// Сырые байты — для отладки в UI.
    pub raw_earfcn: Option<String>,
    pub raw_pci: Option<String>,
}

impl LockState {
    /// Обе фиксации сразу конфликтуют по приоритетам в модеме.
    pub fn has_conflict(&self) -> bool {
        self.earfcn.is_some() && self.pci.is_some()
    }
}

#[derive(Debug, Clone, Default)]
pub struct Signal {
    pub rsrp: Option<f64>,
    pub rsrq: Option<f64>,
    pub sinr: Option<f64>,
    pub rssi: Option<f64>,
    pub earfcn: Option<u32>,
    pub pci: Option<u16>,
    pub band: Option<String>,
    pub operator: Option<String>,
    pub registered: bool,
}

#[derive(Debug, Clone)]
pub struct Sample {
    pub ts: u64,
    pub rsrp: Option<f64>,
    pub rsrq: Option<f64>,
    pub sinr: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct Neighbor {
    pub earfcn: u32,
    pub pci: u16,
    pub rsrp: Option<f64>,
    pub rsrq: Option<f64>,
}

/// Какие команды подтвердились на конкретной прошивке.
#[derive(Debug, Clone, Default)]
pub struct Caps {
    /// Команда, отдающая метрики служебной соты.
    pub serving: Option<String>,
    /// Команда, отдающая соседей.
    pub neighbors: Option<String>,
    /// Команда управления бэндами (чтение).
    pub bands_query: Option<String>,
    /// Шаблон записи бэндов; `{}` заменяется на маску.
    pub bands_set: Option<String>,
    /// Поддерживается ли at^efs (без него фиксация невозможна).
    pub efs: bool,
}

// ---------------------------------------------------------------------------
// Modem
// ---------------------------------------------------------------------------

pub struct Modem {
    port: Mutex<AtPort>,
    caps: Mutex<Caps>,
    history: Mutex<VecDeque<Sample>>,
    last_signal: Mutex<Signal>,
    info: Mutex<String>,
}

/// Кандидаты на метрики служебной соты, в порядке предпочтения.
const SERVING_CANDIDATES: &[&str] = &["AT^DEBUG?", "AT+GTCCINFO?", "AT+CESQ", "AT+CSQ"];
/// Кандидаты на список соседей.
const NEIGHBOR_CANDIDATES: &[&str] = &["AT$QCRSRP?", "AT+VZWRSRP?", "AT+GTCCINFO?"];
/// Кандидаты на чтение бэндов.
const BAND_QUERY_CANDIDATES: &[&str] = &["AT+GTACT?", "AT^BAND_PRI?", "AT^SYSCFGEX?"];

impl Modem {
    pub fn new(transport: Transport) -> Self {
        Modem {
            port: Mutex::new(AtPort::new(transport)),
            caps: Mutex::new(Caps::default()),
            history: Mutex::new(VecDeque::with_capacity(HISTORY_CAP)),
            last_signal: Mutex::new(Signal::default()),
            info: Mutex::new(String::new()),
        }
    }

    pub fn transport_name(&self) -> String {
        self.port
            .lock()
            .map(|p| p.transport().describe())
            .unwrap_or_else(|_| "unknown".to_string())
    }

    pub fn info(&self) -> String {
        self.info.lock().map(|i| i.clone()).unwrap_or_default()
    }

    pub fn caps(&self) -> Caps {
        self.caps.lock().map(|c| c.clone()).unwrap_or_default()
    }

    /// Сырая отправка — используется AT-консолью в UI.
    pub fn send_raw(&self, cmd: &str) -> Result<AtResponse, String> {
        let mut port = self
            .port
            .lock()
            .map_err(|_| "AT-порт отравлен".to_string())?;
        port.send_slow(cmd)
    }

    fn send(&self, cmd: &str) -> Result<AtResponse, String> {
        let mut port = self
            .port
            .lock()
            .map_err(|_| "AT-порт отравлен".to_string())?;
        port.send(cmd)
    }

    /// Проба возможностей. Вызывается один раз при старте.
    pub fn probe(&self) {
        if let Ok(r) = self.send("ATI") {
            if let Ok(mut i) = self.info.lock() {
                *i = r.body.clone();
            }
        }

        let mut caps = Caps::default();

        // at^efs — читаем заведомо существующий путь; ERROR значит команды нет.
        if let Ok(r) = self.send(&format!("at^efs=\"{}\"", EFS_EARFCN)) {
            caps.efs = r.ok || r.body.to_ascii_uppercase().contains("EFS:");
        }

        for cmd in SERVING_CANDIDATES {
            if let Ok(r) = self.send(cmd) {
                if r.ok && !r.body.is_empty() {
                    caps.serving = Some(cmd.to_string());
                    break;
                }
            }
        }

        for cmd in NEIGHBOR_CANDIDATES {
            if let Ok(r) = self.send(cmd) {
                if r.ok && !r.body.is_empty() && !parse_neighbors(&r.body).is_empty() {
                    caps.neighbors = Some(cmd.to_string());
                    break;
                }
            }
        }

        for cmd in BAND_QUERY_CANDIDATES {
            if let Ok(r) = self.send(cmd) {
                if r.ok && !r.body.is_empty() {
                    caps.bands_query = Some(cmd.to_string());
                    caps.bands_set = band_set_template(cmd);
                    break;
                }
            }
        }

        if let Ok(mut c) = self.caps.lock() {
            *c = caps;
        }
    }

    // --- фиксация ---

    pub fn read_lock(&self) -> Result<LockState, String> {
        let mut st = LockState::default();

        let e = self.send(&format!("at^efs=\"{}\"", EFS_EARFCN))?;
        if let Some(bytes) = parse_efs_bytes(&e.body) {
            st.earfcn = from_le16(&bytes);
            st.raw_earfcn = Some(bytes);
        }

        let p = self.send(&format!("at^efs=\"{}\"", EFS_PCI))?;
        if let Some(bytes) = parse_efs_bytes(&p.body) {
            let parts: Vec<&str> = bytes.split(',').collect();
            if parts.len() == 4 {
                st.pci_earfcn = from_le16(&format!("{},{}", parts[0], parts[1]));
                st.pci = from_le16(&format!("{},{}", parts[2], parts[3]));
            }
            st.raw_pci = Some(bytes);
        }

        Ok(st)
    }

    /// Записать EFS-файл и убедиться, что записалось именно то, что просили.
    /// При расхождении — один раз переключаемся на прямой порт (ndmc портит кавычки).
    fn write_efs(&self, path: &str, bytes: &str) -> Result<(), String> {
        let cmd = if bytes.is_empty() {
            format!("at^efs=\"{}\",0", path)
        } else {
            let len = bytes.split(',').count();
            format!("at^efs=\"{}\",{},\"{}\"", path, len, bytes)
        };

        self.send(&cmd)?;
        if self.verify_efs(path, bytes)? {
            return Ok(());
        }

        let switched = {
            let mut port = self
                .port
                .lock()
                .map_err(|_| "AT-порт отравлен".to_string())?;
            port.fallback_to_serial()
        };
        if switched {
            self.send(&cmd)?;
            if self.verify_efs(path, bytes)? {
                return Ok(());
            }
        }

        Err(format!(
            "не удалось записать {}: в модеме оказалось не то, что ожидалось",
            path
        ))
    }

    fn verify_efs(&self, path: &str, expected: &str) -> Result<bool, String> {
        let r = self.send(&format!("at^efs=\"{}\"", path))?;
        let got = parse_efs_bytes(&r.body).unwrap_or_default();
        Ok(got == expected)
    }

    /// Зафиксировать несущую. pci_lock при этом снимается — иначе конфликт.
    pub fn lock_earfcn(&self, earfcn: u16) -> Result<(), String> {
        self.write_efs(EFS_EARFCN, &to_le16(earfcn))?;
        self.write_efs(EFS_PCI, "")?;
        Ok(())
    }

    /// Зафиксировать сектор. earfcn_lock при этом снимается.
    pub fn lock_pci(&self, earfcn: u16, pci: u16) -> Result<(), String> {
        if pci > 503 {
            return Err(format!("PCI {} вне диапазона 0..503", pci));
        }
        let bytes = format!("{},{}", to_le16(earfcn), to_le16(pci));
        self.write_efs(EFS_PCI, &bytes)?;
        self.write_efs(EFS_EARFCN, "")?;
        Ok(())
    }

    pub fn unlock(&self) -> Result<(), String> {
        self.write_efs(EFS_EARFCN, "")?;
        self.write_efs(EFS_PCI, "")?;
        Ok(())
    }

    pub fn reset_modem(&self) -> Result<(), String> {
        let mut port = self
            .port
            .lock()
            .map_err(|_| "AT-порт отравлен".to_string())?;
        port.send_slow("at+cfun=1,1")?;
        Ok(())
    }

    // --- метрики ---

    pub fn poll_signal(&self) -> Signal {
        let cmd = self.caps().serving.unwrap_or_else(|| "AT+CESQ".to_string());

        let mut sig = Signal::default();
        if let Ok(r) = self.send(&cmd) {
            sig = parse_signal(&r.body);
        }

        // Оператор и факт регистрации — стандартные команды, есть почти везде.
        if let Ok(r) = self.send("AT+COPS?") {
            sig.operator = parse_operator(&r.body);
        }
        if let Ok(r) = self.send("AT+CEREG?") {
            sig.registered = parse_registered(&r.body);
        }

        if let Ok(mut last) = self.last_signal.lock() {
            *last = sig.clone();
        }
        if let Ok(mut h) = self.history.lock() {
            if h.len() >= HISTORY_CAP {
                h.pop_front();
            }
            h.push_back(Sample {
                ts: now_secs(),
                rsrp: sig.rsrp,
                rsrq: sig.rsrq,
                sinr: sig.sinr,
            });
        }
        sig
    }

    pub fn last_signal(&self) -> Signal {
        self.last_signal
            .lock()
            .map(|s| s.clone())
            .unwrap_or_default()
    }

    pub fn history(&self) -> Vec<Sample> {
        self.history
            .lock()
            .map(|h| h.iter().cloned().collect())
            .unwrap_or_default()
    }

    pub fn scan_neighbors(&self) -> Result<Vec<Neighbor>, String> {
        let cmd = self
            .caps()
            .neighbors
            .ok_or_else(|| "прошивка модема не отдаёт список соседних сот".to_string())?;
        let mut port = self
            .port
            .lock()
            .map_err(|_| "AT-порт отравлен".to_string())?;
        let r = port.send_slow(&cmd)?;
        drop(port);
        Ok(parse_neighbors(&r.body))
    }

    pub fn read_bands(&self) -> Result<String, String> {
        let cmd = self
            .caps()
            .bands_query
            .ok_or_else(|| "прошивка модема не поддерживает управление бэндами".to_string())?;
        let r = self.send(&cmd)?;
        Ok(r.body)
    }

    pub fn set_bands(&self, mask: &str) -> Result<(), String> {
        if !mask.chars().all(|c| c.is_ascii_hexdigit()) || mask.is_empty() {
            return Err("маска бэндов должна быть hex-числом".to_string());
        }
        let tmpl = self
            .caps()
            .bands_set
            .ok_or_else(|| "прошивка модема не поддерживает запись бэндов".to_string())?;
        let cmd = tmpl.replace("{}", mask);
        let r = self.send(&cmd)?;
        r.require_ok()
    }
}

fn band_set_template(query: &str) -> Option<String> {
    match query {
        "AT+GTACT?" => Some("AT+GTACT=2,,,{}".to_string()),
        "AT^BAND_PRI?" => Some("AT^BAND_PRI={}".to_string()),
        _ => None,
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Парсеры ответов
// ---------------------------------------------------------------------------

/// Числа из `+CESQ: rxlev,ber,rscp,ecno,rsrq,rsrp` переводятся в дБм/дБ по 3GPP 27.007.
fn cesq_rsrp(v: i64) -> Option<f64> {
    if v == 255 {
        None
    } else {
        Some(-140.0 + v as f64)
    }
}

fn cesq_rsrq(v: i64) -> Option<f64> {
    if v == 255 {
        None
    } else {
        Some(-20.0 + v as f64 / 2.0)
    }
}

/// `+CSQ: rssi,ber`, rssi 0..31 -> -113..-51 дБм.
fn csq_rssi(v: i64) -> Option<f64> {
    if v == 99 {
        None
    } else {
        Some(-113.0 + 2.0 * v as f64)
    }
}

pub fn parse_signal(body: &str) -> Signal {
    let mut s = Signal::default();

    for line in body.lines() {
        let t = line.trim();
        let upper = t.to_ascii_uppercase();

        if let Some(rest) = upper.strip_prefix("+CESQ:") {
            let nums = split_nums(rest);
            if nums.len() >= 6 {
                s.rsrq = cesq_rsrq(nums[4]);
                s.rsrp = cesq_rsrp(nums[5]);
            }
        } else if let Some(rest) = upper.strip_prefix("+CSQ:") {
            let nums = split_nums(rest);
            if !nums.is_empty() {
                s.rssi = csq_rssi(nums[0]);
            }
        } else if upper.contains("EARFCN") {
            // ^DEBUG: "EARFCN(DL/UL): 1575/19575"
            if let Some(v) = first_number_after(t, "EARFCN") {
                s.earfcn = Some(v as u32);
            }
        }

        if upper.contains("PCI") {
            // "eNB ID(PCI): 25733-2(161)" — нужен последний номер в скобках.
            if let Some(v) = last_parenthesised_number(t) {
                s.pci = Some(v as u16);
            }
        }

        for (key, slot) in [("RSRP", 0), ("RSRQ", 1), ("SINR", 2), ("RSSI", 3)] {
            if upper.contains(key) && !upper.starts_with("+CESQ") {
                if let Some(v) = signed_number_after(t, key) {
                    match slot {
                        0 if s.rsrp.is_none() => s.rsrp = Some(v),
                        1 if s.rsrq.is_none() => s.rsrq = Some(v),
                        2 if s.sinr.is_none() => s.sinr = Some(v),
                        3 if s.rssi.is_none() => s.rssi = Some(v),
                        _ => {}
                    }
                }
            }
        }
    }

    s
}

/// `+COPS: 0,0,"MegaFon",7` -> `MegaFon`
pub fn parse_operator(body: &str) -> Option<String> {
    let line = body.lines().find(|l| l.contains("+COPS:"))?;
    let start = line.find('"')?;
    let rest = &line[start + 1..];
    let end = rest.find('"')?;
    let name = rest[..end].trim().to_string();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

/// `+CEREG: 2,1` — stat 1 (home) или 5 (roaming) значит «зарегистрированы».
pub fn parse_registered(body: &str) -> bool {
    for line in body.lines() {
        if let Some(rest) = line.trim().to_ascii_uppercase().strip_prefix("+CEREG:") {
            let nums = split_nums(rest);
            if nums.len() >= 2 {
                return nums[1] == 1 || nums[1] == 5;
            }
        }
    }
    false
}

/// Соседи: `$QCRSRP: <pci>,<earfcn>,<rsrp>[,<rsrq>]` — по строке на соту.
pub fn parse_neighbors(body: &str) -> Vec<Neighbor> {
    let mut out = Vec::new();
    for line in body.lines() {
        let t = line.trim();
        let upper = t.to_ascii_uppercase();
        if !(upper.starts_with("$QCRSRP") || upper.starts_with("+VZWRSRP")) {
            continue;
        }
        let rest = match t.split_once(':') {
            Some((_, r)) => r,
            None => continue,
        };
        let vals: Vec<&str> = rest.split(',').map(|v| v.trim()).collect();
        if vals.len() < 3 {
            continue;
        }
        let pci = match vals[0].parse::<u16>() {
            Ok(v) => v,
            Err(_) => continue,
        };
        let earfcn = match vals[1].parse::<u32>() {
            Ok(v) => v,
            Err(_) => continue,
        };
        out.push(Neighbor {
            pci,
            earfcn,
            rsrp: vals[2].parse::<f64>().ok(),
            rsrq: vals.get(3).and_then(|v| v.parse::<f64>().ok()),
        });
    }
    // Лучший сигнал сверху; соты без RSRP — в конец.
    out.sort_by(|a, b| {
        b.rsrp
            .unwrap_or(f64::NEG_INFINITY)
            .partial_cmp(&a.rsrp.unwrap_or(f64::NEG_INFINITY))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out
}

// --- мелкие помощники разбора ---

fn split_nums(s: &str) -> Vec<i64> {
    s.split(',')
        .map(|p| p.trim())
        .map(|p| p.parse::<i64>().unwrap_or(-1))
        .collect()
}

/// Первое целое число после подстроки `key`.
fn first_number_after(line: &str, key: &str) -> Option<i64> {
    let idx = line.to_ascii_uppercase().find(key)?;
    let rest = &line[idx + key.len()..];
    let digits: String = rest
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

/// Число (возможно, отрицательное/дробное) после `key` — для `RSRP: -95`.
fn signed_number_after(line: &str, key: &str) -> Option<f64> {
    let idx = line.to_ascii_uppercase().find(key)?;
    let rest: Vec<char> = line[idx + key.len()..].chars().collect();

    // Начало числа: цифра либо знак, за которым сразу идёт цифра.
    // Знак без цифры следом — это дефис-разделитель, а не минус.
    let start = (0..rest.len()).find(|&i| {
        rest[i].is_ascii_digit()
            || ((rest[i] == '-' || rest[i] == '+')
                && rest.get(i + 1).is_some_and(|c| c.is_ascii_digit()))
    })?;

    let mut buf = String::new();
    let mut seen_dot = false;
    for (i, &c) in rest.iter().enumerate().skip(start) {
        if c.is_ascii_digit() || (i == start && (c == '-' || c == '+')) {
            buf.push(c);
        } else if c == '.' && !seen_dot && rest.get(i + 1).is_some_and(|n| n.is_ascii_digit()) {
            seen_dot = true;
            buf.push(c);
        } else {
            break;
        }
    }
    buf.parse().ok()
}

/// Последнее число в скобках: `25733-2(161)` -> 161.
fn last_parenthesised_number(line: &str) -> Option<i64> {
    let open = line.rfind('(')?;
    let rest = &line[open + 1..];
    let close = rest.find(')')?;
    rest[..close].trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn le16_roundtrip() {
        for v in [0u16, 1, 57, 300, 1575, 2850, 3048, 65535] {
            assert_eq!(from_le16(&to_le16(v)), Some(v), "v={}", v);
        }
    }

    #[test]
    fn le16_matches_forum_reference() {
        // Эталонные значения из исходного поста на 4PDA.
        assert_eq!(to_le16(2850), "22,0b");
        assert_eq!(to_le16(57), "39,00");
    }

    #[test]
    fn from_le16_accepts_upper_and_spaces() {
        assert_eq!(from_le16("22,0B"), Some(2850));
        assert_eq!(from_le16(" 22 , 0b "), Some(2850));
        assert_eq!(from_le16("22"), None);
        assert_eq!(from_le16("zz,0b"), None);
    }

    #[test]
    fn parses_efs_line() {
        assert_eq!(
            parse_efs_bytes("^EFS: /nv/item_files/modem/lte/rrc/csp/pci_lock, 22,0b,39,00"),
            Some("22,0b,39,00".to_string())
        );
        assert_eq!(
            parse_efs_bytes("^EFS: /nv/x/earfcn_lock, 22,0B"),
            Some("22,0b".to_string())
        );
    }

    #[test]
    fn parses_empty_and_garbage_efs() {
        assert_eq!(parse_efs_bytes("^EFS: /nv/x/earfcn_lock, "), None);
        assert_eq!(parse_efs_bytes("^EFS: /nv/x, zz,qq"), None);
        assert_eq!(parse_efs_bytes("OK"), None);
        assert_eq!(parse_efs_bytes(""), None);
    }

    #[test]
    fn conflict_detected() {
        let st = LockState {
            earfcn: Some(2850),
            pci: Some(57),
            ..Default::default()
        };
        assert!(st.has_conflict());
        assert!(!LockState {
            earfcn: Some(2850),
            ..Default::default()
        }
        .has_conflict());
    }

    #[test]
    fn parses_cesq() {
        // rsrq=17 -> -11.5 дБ, rsrp=45 -> -95 дБм
        let s = parse_signal("+CESQ: 99,99,255,255,17,45");
        assert_eq!(s.rsrp, Some(-95.0));
        assert_eq!(s.rsrq, Some(-11.5));
    }

    #[test]
    fn cesq_255_means_unknown() {
        let s = parse_signal("+CESQ: 99,99,255,255,255,255");
        assert_eq!(s.rsrp, None);
        assert_eq!(s.rsrq, None);
    }

    #[test]
    fn parses_debug_serving_cell() {
        let s = parse_signal("EARFCN(DL/UL): 1575/19575\neNB ID(PCI): 25733-2(161)");
        assert_eq!(s.earfcn, Some(1575));
        assert_eq!(s.pci, Some(161));
    }

    #[test]
    fn parses_labelled_metrics() {
        let s = parse_signal("RSRP: -95\nRSRQ: -11\nSINR: 12.5");
        assert_eq!(s.rsrp, Some(-95.0));
        assert_eq!(s.rsrq, Some(-11.0));
        assert_eq!(s.sinr, Some(12.5));
    }

    #[test]
    fn signed_number_handles_separators_and_decimals() {
        assert_eq!(signed_number_after("RSRP: -95 dBm", "RSRP"), Some(-95.0));
        assert_eq!(signed_number_after("SINR: 12.5", "SINR"), Some(12.5));
        assert_eq!(signed_number_after("RSRQ = +3", "RSRQ"), Some(3.0));
        // Дефис как разделитель, а не знак: берём 25733, не -2.
        assert_eq!(signed_number_after("ID: 25733-2", "ID"), Some(25733.0));
        // Точка в конце предложения не должна попасть в число.
        assert_eq!(signed_number_after("RSRP: -95.", "RSRP"), Some(-95.0));
        assert_eq!(signed_number_after("RSRP: n/a", "RSRP"), None);
        assert_eq!(signed_number_after("нет ключа", "RSRP"), None);
    }

    #[test]
    fn parses_operator() {
        assert_eq!(
            parse_operator("+COPS: 0,0,\"MegaFon\",7").as_deref(),
            Some("MegaFon")
        );
        assert_eq!(parse_operator("+COPS: 2"), None);
    }

    #[test]
    fn parses_registration() {
        assert!(parse_registered("+CEREG: 2,1"));
        assert!(parse_registered("+CEREG: 0,5"));
        assert!(!parse_registered("+CEREG: 0,0"));
        assert!(!parse_registered("+CEREG: 2,2"));
        assert!(!parse_registered(""));
    }

    #[test]
    fn parses_and_sorts_neighbors() {
        let body = "$QCRSRP: 161,1575,-95,-11\n$QCRSRP: 57,2850,-80,-9\n$QCRSRP: 22,1575,-110";
        let n = parse_neighbors(body);
        assert_eq!(n.len(), 3);
        // Отсортировано по убыванию RSRP.
        assert_eq!(n[0].pci, 57);
        assert_eq!(n[0].earfcn, 2850);
        assert_eq!(n[0].rsrq, Some(-9.0));
        assert_eq!(n[2].pci, 22);
        assert_eq!(n[2].rsrq, None);
    }

    #[test]
    fn ignores_malformed_neighbor_lines() {
        let body = "$QCRSRP: abc,1575,-95\n$QCRSRP: 5\nOK\n+VZWRSRP: 7,1300,-88";
        let n = parse_neighbors(body);
        assert_eq!(n.len(), 1);
        assert_eq!(n[0].pci, 7);
    }
}
