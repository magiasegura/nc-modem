//! Транспорт AT-команд к модему.
//!
//! Два способа, как и в CLI-скрипте `lockband`:
//!   1. `ndmc -c 'interface UsbQmi0 tty send …'` — штатный путь Keenetic (OS 3.9+);
//!   2. прямой символьный AT-порт `/dev/ttyACM*` в raw-режиме.
//!
//! AT-порт модема **один и не реентерабельный**: параллельные команды дают
//! перемешанные ответы. Поэтому наружу торчит только [`AtPort::send`], а сам
//! `AtPort` всегда живёт под `Mutex` (см. `modem::Modem`).

use std::io::{Read, Write};
use std::process::Command;
use std::time::{Duration, Instant};

/// Сколько ждём ответа модема целиком.
const READ_TIMEOUT: Duration = Duration::from_secs(5);
/// `at+cfun=1,1` и сканирование сот отвечают дольше обычного.
const READ_TIMEOUT_SLOW: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq)]
pub enum Transport {
    /// Через CLI роутера.
    Ndmc { iface: String },
    /// Напрямую в символьное устройство.
    Serial { dev: String },
    /// Фиктивный модем для `--demo`: посмотреть UI без железа.
    Mock,
    /// Фиктивный модем Intel XMM для `--demo-intel`.
    MockIntel,
}

impl Transport {
    pub fn describe(&self) -> String {
        match self {
            Transport::Ndmc { iface } => format!("ndmc:{}", iface),
            Transport::Serial { dev } => format!("serial:{}", dev),
            Transport::Mock => "demo qualcomm (модем не подключён)".to_string(),
            Transport::MockIntel => "demo intel (модем не подключён)".to_string(),
        }
    }
}

/// Разобранный ответ модема.
#[derive(Debug, Clone)]
pub struct AtResponse {
    /// Ответ без эха команды и без финального OK.
    pub body: String,
    /// Сырой ответ целиком — для AT-консоли и диагностики.
    pub raw: String,
    /// Модем ответил `OK`.
    pub ok: bool,
    /// Текст ошибки, если пришло `ERROR` / `+CME ERROR: …`.
    pub error: Option<String>,
}

impl AtResponse {
    /// Успех либо содержательная ошибка модема.
    pub fn require_ok(&self) -> Result<(), String> {
        if self.ok {
            Ok(())
        } else {
            Err(self
                .error
                .clone()
                .unwrap_or_else(|| "модем не ответил OK".to_string()))
        }
    }
}

pub struct AtPort {
    transport: Transport,
}

impl AtPort {
    pub fn new(transport: Transport) -> Self {
        AtPort { transport }
    }

    pub fn transport(&self) -> &Transport {
        &self.transport
    }

    /// Отправить команду и дождаться ответа.
    pub fn send(&mut self, cmd: &str) -> Result<AtResponse, String> {
        self.send_with_timeout(cmd, READ_TIMEOUT)
    }

    /// То же, но с увеличенным таймаутом — для сканирования и `at+cfun`.
    pub fn send_slow(&mut self, cmd: &str) -> Result<AtResponse, String> {
        self.send_with_timeout(cmd, READ_TIMEOUT_SLOW)
    }

    fn send_with_timeout(&mut self, cmd: &str, timeout: Duration) -> Result<AtResponse, String> {
        let raw = match &self.transport {
            Transport::Ndmc { iface } => ndmc_send(iface, cmd)?,
            Transport::Serial { dev } => serial_send(dev, cmd, timeout)?,
            Transport::Mock => mock_send(cmd),
            Transport::MockIntel => mock_intel_send(cmd),
        };
        Ok(parse_response(cmd, &raw))
    }

    /// Перейти на прямой порт: CLI Keenetic умеет портить кавычки в `at^efs="…"`.
    pub fn fallback_to_serial(&mut self) -> bool {
        if matches!(self.transport, Transport::Serial { .. }) {
            return false;
        }
        match detect_serial(None) {
            Some(t) => {
                self.transport = t;
                true
            }
            None => false,
        }
    }
}

// ---------------------------------------------------------------------------
// ndmc
// ---------------------------------------------------------------------------

fn ndmc_send(iface: &str, cmd: &str) -> Result<String, String> {
    let out = Command::new("ndmc")
        .arg("-c")
        .arg(format!("interface {} tty send {}", iface, cmd))
        .output()
        .map_err(|e| format!("не запустить ndmc: {}", e))?;

    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    if text.trim().is_empty() {
        text = String::from_utf8_lossy(&out.stderr).into_owned();
    }
    Ok(text)
}

// ---------------------------------------------------------------------------
// Фиктивный модем (--demo)
// ---------------------------------------------------------------------------

/// Правдоподобные ответы, чтобы посмотреть интерфейс без железа.
/// Состояние фиксации живёт в памяти процесса и сбрасывается при перезапуске.
fn mock_send(cmd: &str) -> String {
    use std::sync::Mutex;
    use std::sync::OnceLock;

    static EFS: OnceLock<Mutex<Vec<(String, String)>>> = OnceLock::new();
    let store = EFS.get_or_init(|| Mutex::new(Vec::new()));

    let c = cmd.trim();
    let upper = c.to_ascii_uppercase();

    if let Some(rest) = c
        .strip_prefix("at^efs=")
        .or_else(|| c.strip_prefix("AT^EFS="))
    {
        let args: Vec<&str> = rest.split(',').collect();
        let path = args[0].trim_matches('"').to_string();
        let mut store = store.lock().unwrap();

        // Чтение: только путь, без длины.
        if args.len() == 1 {
            let val = store
                .iter()
                .find(|(p, _)| *p == path)
                .map(|(_, v)| v.clone())
                .unwrap_or_default();
            return format!("OK\r\n\r\n^EFS: {}, {}\r\nOK\r\n", path, val);
        }

        // Запись или удаление (длина 0).
        let value = if args.get(1).map(|l| l.trim()) == Some("0") {
            String::new()
        } else {
            rest.split_once(',')
                .and_then(|(_, r)| r.split_once(','))
                .map(|(_, v)| v.trim().trim_matches('"').to_string())
                .unwrap_or_default()
        };
        store.retain(|(p, _)| *p != path);
        if !value.is_empty() {
            store.push((path, value));
        }
        return "OK\r\n".to_string();
    }

    match upper.as_str() {
        "ATI" => "Foxconn\r\nT77W968\r\nRevision: DEMO.0.0\r\nOK\r\n".to_string(),
        "ATE0" => "OK\r\n".to_string(),
        "AT^DEBUG?" => {
            // Слегка «дышащий» RSRP, чтобы график не был прямой линией.
            let t = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let jitter = (t % 11) as i64 - 5;
            format!(
                "EARFCN(DL/UL): 1575/19575\r\neNB ID(PCI): 25733-2(161)\r\n\
                 RSRP: {}\r\nRSRQ: -10\r\nSINR: {}\r\nOK\r\n",
                -95 + jitter,
                12 + jitter / 2
            )
        }
        "AT+COPS?" => "+COPS: 0,0,\"DemoNet\",7\r\nOK\r\n".to_string(),
        "AT+CEREG?" => "+CEREG: 2,1\r\nOK\r\n".to_string(),
        "AT$QCRSRP?" => "$QCRSRP: 161,1575,-95,-10\r\n\
                         $QCRSRP: 57,2850,-88,-9\r\n\
                         $QCRSRP: 304,1300,-104,-14\r\nOK\r\n"
            .to_string(),
        "AT+GTACT?" => "+GTACT: 2,,,8000084\r\nOK\r\n".to_string(),
        "AT+CFUN=1,1" => "OK\r\n".to_string(),
        _ if upper.starts_with("AT+GTACT=") => "OK\r\n".to_string(),
        _ => "ERROR\r\n".to_string(),
    }
}

/// Фиктивный Intel-модем. Ответы — дословный вывод реального Fibocom L860.
fn mock_intel_send(cmd: &str) -> String {
    use std::sync::Mutex;
    use std::sync::OnceLock;

    static LOCKED: OnceLock<Mutex<Option<String>>> = OnceLock::new();
    let locked = LOCKED.get_or_init(|| Mutex::new(None));

    let c = cmd.trim();
    let upper = c.to_ascii_uppercase();

    if upper.starts_with("AT@SIC:FREQ_LOCK") {
        *locked.lock().unwrap() = Some(c.to_string());
        return "OK
"
        .to_string();
    }

    match upper.as_str() {
        "ATI" => "\".Built@Jun-17-2022:06:30:44\"
OK
"
        .to_string(),
        "ATE0" | "AT+CFUN=4" | "AT+CFUN=15" => "OK
"
        .to_string(),
        "AT+XCESQ?" => "+XCESQ: 0,99,99,255,255,24,62,34,255,255,255,255
OK
"
        .to_string(),
        "AT+XMCI=1" => {
            "+XMCI: 4,250,02,\"0x2608\",\"0x026D741E\",\"0x0108\",\"0x00000642\",\"0x00004C92\",
             \"0xFFFFFFFF\",58,22,38,\"0x00000003\",\"0x00000000\"
             +XMCI: 5,000,000,\"0xFFFE\",\"0xFFFFFFFF\",\"0x01B7\",\"0x00000BE8\",\"0xFFFFFFFF\",
             \"0xFFFFFFFF\",51,14,255,\"0x7FFFFFFF\",\"0x00000000\"
             +XMCI: 5,000,000,\"0xFFFE\",\"0xFFFFFFFF\",\"0x0108\",\"0x000005B2\",\"0xFFFFFFFF\",
             \"0xFFFFFFFF\",59,23,255,\"0x7FFFFFFF\",\"0x00000000\"
OK
"
            .to_string()
        }
        "AT+XLEC?" => "+XLEC: 0,4,5,5,4,3,BAND_LTE_3,0,7,1,3
OK
"
        .to_string(),
        "AT+COPS?" => "+COPS: 0,0,\"MegaFon\",7
OK
"
        .to_string(),
        "AT+CEREG?" => "+CEREG: 2,1
OK
"
        .to_string(),
        // Всё Qualcomm-специфичное честно отвечает «не поддерживается».
        _ => "+CME ERROR: 4
"
        .to_string(),
    }
}

// ---------------------------------------------------------------------------
// Прямой порт
// ---------------------------------------------------------------------------

/// Перевести порт в raw-режим. `min 0 / time 20` = read() вернётся max через 2 с,
/// что даёт неблокирующее чтение без libc и ioctl.
fn stty_raw(dev: &str) {
    let args = [
        "-F", dev, "raw", "-echo", "-echoe", "-echok", "115200", "min", "0", "time", "20",
    ];
    if Command::new("stty").args(args).status().is_ok() {
        return;
    }
    // BSD-подобный busybox использует -f вместо -F.
    let mut alt = args;
    alt[0] = "-f";
    let _ = Command::new("stty").args(alt).status();
}

fn serial_send(dev: &str, cmd: &str, timeout: Duration) -> Result<String, String> {
    stty_raw(dev);

    let mut port = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(dev)
        .map_err(|e| {
            format!(
                "не открыть {}: {} (порт может быть занят прошивкой)",
                dev, e
            )
        })?;

    port.write_all(format!("{}\r", cmd).as_bytes())
        .map_err(|e| format!("не записать в {}: {}", dev, e))?;
    port.flush().ok();

    let started = Instant::now();
    let mut acc = String::new();
    let mut buf = [0u8; 1024];
    let mut empty_reads = 0;

    while started.elapsed() < timeout {
        match port.read(&mut buf) {
            Ok(0) => {
                // VTIME истёк без данных.
                empty_reads += 1;
                if empty_reads >= 2 && !acc.trim().is_empty() {
                    break;
                }
                if empty_reads >= 3 {
                    break;
                }
            }
            Ok(n) => {
                empty_reads = 0;
                acc.push_str(&String::from_utf8_lossy(&buf[..n]));
                if has_terminator(&acc) {
                    break;
                }
            }
            Err(e) => return Err(format!("ошибка чтения {}: {}", dev, e)),
        }
    }

    Ok(acc)
}

/// Пришёл ли финальный код результата.
fn has_terminator(s: &str) -> bool {
    s.lines().any(|l| {
        let l = l.trim();
        l == "OK"
            || l == "ERROR"
            || l.starts_with("+CME ERROR")
            || l.starts_with("+CMS ERROR")
            || l == "NO CARRIER"
    })
}

/// Найти живой AT-порт: первый, ответивший `OK` на `ATE0`.
pub fn detect_serial(preferred: Option<&str>) -> Option<Transport> {
    let mut candidates: Vec<String> = Vec::new();
    if let Some(p) = preferred {
        candidates.push(p.to_string());
    }
    for i in 0..4 {
        candidates.push(format!("/dev/ttyACM{}", i));
    }
    for i in 0..4 {
        candidates.push(format!("/dev/ttyUSB{}", i));
    }

    for dev in candidates {
        if !std::path::Path::new(&dev).exists() {
            continue;
        }
        if let Ok(resp) = serial_send(&dev, "ATE0", Duration::from_secs(2)) {
            if has_terminator(&resp) && resp.contains("OK") {
                return Some(Transport::Serial { dev });
            }
        }
    }
    None
}

/// Найти интерфейс Keenetic, за которым живёт модем.
pub fn detect_ndmc(preferred: Option<&str>) -> Option<Transport> {
    if let Some(iface) = preferred {
        return Some(Transport::Ndmc {
            iface: iface.to_string(),
        });
    }

    let out = Command::new("ndmc")
        .arg("-c")
        .arg("show interface")
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let names = extract_iface_names(&text);

    for iface in names {
        let probe = ndmc_send(&iface, "ATE0").unwrap_or_default();
        if probe.contains("OK") {
            return Some(Transport::Ndmc { iface });
        }
    }
    None
}

/// Вытащить имена вида `UsbQmi0` / `UsbLte0` / `UsbModem0`, QMI вперёд.
fn extract_iface_names(text: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    let mut cur = String::new();

    // Разбиваем по не-алфацифровым символам и оставляем подходящие токены.
    for ch in text.chars().chain(std::iter::once(' ')) {
        if ch.is_ascii_alphanumeric() {
            cur.push(ch);
        } else {
            if is_modem_iface(&cur) && !found.contains(&cur) {
                found.push(cur.clone());
            }
            cur.clear();
        }
    }

    found.sort_by_key(|n| match () {
        _ if n.starts_with("UsbQmi") => 0,
        _ if n.starts_with("UsbLte") => 1,
        _ => 2,
    });
    found
}

fn is_modem_iface(s: &str) -> bool {
    for prefix in ["UsbQmi", "UsbLte", "UsbModem"] {
        if let Some(rest) = s.strip_prefix(prefix) {
            return !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit());
        }
    }
    false
}

/// Определить транспорт при старте: сначала ndmc, потом прямой порт.
pub fn detect(pref_iface: Option<&str>, pref_dev: Option<&str>) -> Option<Transport> {
    if pref_dev.is_none() {
        if let Some(t) = detect_ndmc(pref_iface) {
            return Some(t);
        }
    }
    detect_serial(pref_dev)
}

// ---------------------------------------------------------------------------
// Разбор ответа
// ---------------------------------------------------------------------------

fn parse_response(cmd: &str, raw: &str) -> AtResponse {
    let cleaned = raw.replace('\r', "\n");
    let mut body_lines: Vec<&str> = Vec::new();
    let mut ok = false;
    let mut error = None;

    for line in cleaned.lines() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        // Эхо отправленной команды.
        if t.eq_ignore_ascii_case(cmd.trim()) {
            continue;
        }
        if t == "OK" {
            ok = true;
            continue;
        }
        if t == "ERROR" {
            error = Some("ERROR".to_string());
            continue;
        }
        if t.starts_with("+CME ERROR") || t.starts_with("+CMS ERROR") {
            error = Some(t.to_string());
            continue;
        }
        body_lines.push(t);
    }

    AtResponse {
        body: body_lines.join("\n"),
        raw: raw.to_string(),
        ok,
        error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ok_with_body() {
        let r = parse_response(
            "at^efs=\"/nv/x\"",
            "at^efs=\"/nv/x\"\r\nOK\r\n\r\n^EFS: /nv/x, 22,0b\r\nOK\r\n",
        );
        assert!(r.ok);
        assert_eq!(r.body, "^EFS: /nv/x, 22,0b");
        assert!(r.error.is_none());
    }

    #[test]
    fn parses_cme_error() {
        let r = parse_response("AT^DEBUG?", "AT^DEBUG?\r\n+CME ERROR: 4\r\n");
        assert!(!r.ok);
        assert_eq!(r.error.as_deref(), Some("+CME ERROR: 4"));
        assert!(r.body.is_empty());
    }

    #[test]
    fn plain_error_is_reported() {
        let r = parse_response("ATX", "ERROR\r\n");
        assert!(!r.ok);
        assert!(r.require_ok().is_err());
    }

    #[test]
    fn detects_terminators() {
        assert!(has_terminator("foo\r\nOK\r\n"));
        assert!(has_terminator("+CME ERROR: 100\r\n"));
        assert!(!has_terminator("^EFS: /nv/x, 22,0b\r\n"));
        // "OKAY" не терминатор.
        assert!(!has_terminator("OKAY\r\n"));
    }

    #[test]
    fn picks_qmi_interface_first() {
        let text = "Interface: UsbLte0\n  state: up\nInterface: UsbQmi0\n  state: up\n";
        assert_eq!(extract_iface_names(text), vec!["UsbQmi0", "UsbLte0"]);
    }

    #[test]
    fn ignores_non_modem_interfaces() {
        let text = "Bridge0 GigabitEthernet1 UsbQmi0 Wireguard0 UsbQmiFoo";
        assert_eq!(extract_iface_names(text), vec!["UsbQmi0"]);
    }
}
