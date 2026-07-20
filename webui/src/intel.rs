//! Поддержка модемов на Intel XMM (Fibocom L850/L860 и родня).
//!
//! Отличия от Qualcomm принципиальные:
//!   * фиксация делается командой `at@sic:freq_lock(...)`, а не записью NV-файлов;
//!   * значения десятичные, никаких little-endian hex-байтов;
//!   * команда требует номер band, который приходится выводить из EARFCN;
//!   * прочитать текущую фиксацию из модема нечем — состояние приходится
//!     помнить самим, см. [`LockStore`].
//!
//! Парсеры калиброваны по реальному выводу Fibocom L860 (прошивка 2022-06-17).

use crate::bands::{band_from_earfcn, intel_band_code};
use crate::modem::{Neighbor, Signal};

/// RAT для LTE в `freq_lock`.
const RAT_LTE: u8 = 3;
/// Идентификатор SIM — у однослотовых модемов всегда 0.
const SIM_ID: u8 = 0;

/// Собрать команду фиксации/снятия.
///
/// Сигнатура из документации Fibocom:
/// `freq_lock($sim_id $rat $band $inter_frequency_lock_enable $frequency $psc_pci)`
pub fn freq_lock_cmd(earfcn: u32, pci: u16, enable: bool) -> Result<String, String> {
    let band = band_from_earfcn(earfcn)
        .ok_or_else(|| format!("не удалось определить LTE-диапазон для EARFCN {}", earfcn))?;
    Ok(format!(
        "at@sic:freq_lock({},{},{},{},{},{})",
        SIM_ID,
        RAT_LTE,
        intel_band_code(band),
        if enable { 1 } else { 0 },
        earfcn,
        pci
    ))
}

/// Команды перезапуска радиомодуля. Полная перезагрузка роутера не нужна.
pub const RADIO_CYCLE: [&str; 2] = ["at+cfun=4", "at+cfun=15"];

// ---------------------------------------------------------------------------
// Разбор ответов
// ---------------------------------------------------------------------------

/// Поле ответа: либо десятичное число, либо hex-строка в кавычках `"0x0108"`.
fn field(raw: &str) -> Option<u32> {
    let t = raw.trim().trim_matches('"').trim();
    if t.is_empty() {
        return None;
    }
    let v = if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        u32::from_str_radix(hex, 16).ok()?
    } else {
        t.parse::<u32>().ok()?
    };
    // 0xFFFFFFFF и 0x7FFFFFFF модем использует как «нет данных».
    if v == 0xFFFF_FFFF || v == 0x7FFF_FFFF {
        return None;
    }
    Some(v)
}

/// Разбить тело ответа на записи `+XMCI: …`.
///
/// Именно по маркеру, а не по строкам: длинные записи переносятся, и разбор
/// построчно рассыпался бы на реальном выводе.
fn xmci_records(body: &str) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    for chunk in body.split("+XMCI:").skip(1) {
        // Запись заканчивается там, где начинается следующая или OK.
        let stop = chunk.find("\nOK").unwrap_or(chunk.len());
        let fields: Vec<String> = chunk[..stop]
            .split(',')
            .map(|f| f.trim().trim_matches('"').trim().to_string())
            .collect();
        if fields.len() >= 7 {
            out.push(fields);
        }
    }
    out
}

/// Индексы полей `+XMCI: <TYPE>,<MCC>,<MNC>,<TAC>,<CI>,<PCI>,<DLUARFCN>,…`
const F_TYPE: usize = 0;
const F_PCI: usize = 5;
const F_EARFCN: usize = 6;
const F_RSRP: usize = 9;
const F_RSRQ: usize = 10;
const F_RSSNR: usize = 11;

/// TYPE=4 — служебная сота LTE, TYPE=5 — соседняя.
const TYPE_SERVING: u32 = 4;
const TYPE_NEIGHBOR: u32 = 5;

/// RSRP по 3GPP: индекс 0..97 -> дБм.
fn rsrp_dbm(v: u32) -> Option<f64> {
    if v > 140 {
        None
    } else {
        Some(v as f64 - 141.0)
    }
}

/// RSRQ по 3GPP: индекс 0..34 -> дБ.
fn rsrq_db(v: u32) -> Option<f64> {
    if v > 40 {
        None
    } else {
        Some(v as f64 / 2.0 - 20.0)
    }
}

/// Служебная сота из ответа `AT+XMCI=1`.
pub fn parse_serving(body: &str) -> Option<Signal> {
    let rec = xmci_records(body)
        .into_iter()
        .find(|r| field(&r[F_TYPE]) == Some(TYPE_SERVING))?;

    let mut s = Signal {
        earfcn: rec.get(F_EARFCN).and_then(|v| field(v)),
        pci: rec.get(F_PCI).and_then(|v| field(v)).map(|v| v as u16),
        ..Default::default()
    };
    s.rsrp = rec.get(F_RSRP).and_then(|v| field(v)).and_then(rsrp_dbm);
    s.rsrq = rec.get(F_RSRQ).and_then(|v| field(v)).and_then(rsrq_db);
    // RSSNR идёт в шкале модема; точной формулы в документации нет,
    // поэтому отдаём как есть и не выдаём догадку за измерение.
    s.sinr = rec.get(F_RSSNR).and_then(|v| field(v)).map(|v| v as f64);
    s.band = s
        .earfcn
        .and_then(band_from_earfcn)
        .map(|b| format!("B{}", b));
    Some(s)
}

/// Соседние соты из того же ответа.
pub fn parse_neighbors(body: &str) -> Vec<Neighbor> {
    let mut out: Vec<Neighbor> = xmci_records(body)
        .into_iter()
        .filter(|r| field(&r[F_TYPE]) == Some(TYPE_NEIGHBOR))
        .filter_map(|r| {
            Some(Neighbor {
                pci: field(r.get(F_PCI)?)? as u16,
                earfcn: field(r.get(F_EARFCN)?)?,
                rsrp: r.get(F_RSRP).and_then(|v| field(v)).and_then(rsrp_dbm),
                rsrq: r.get(F_RSRQ).and_then(|v| field(v)).and_then(rsrq_db),
            })
        })
        .collect();

    out.sort_by(|a, b| {
        b.rsrp
            .unwrap_or(f64::NEG_INFINITY)
            .partial_cmp(&a.rsrp.unwrap_or(f64::NEG_INFINITY))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out
}

/// `+XCESQ: <n>,<rxlev>,<ber>,<rscp>,<ecno>,<rsrq>,<rsrp>,<sinr>,…`
pub fn parse_xcesq(body: &str) -> Option<Signal> {
    let line = body.lines().find(|l| l.contains("+XCESQ:"))?;
    let nums: Vec<Option<u32>> = line.split_once(':')?.1.split(',').map(field).collect();
    if nums.len() < 7 {
        return None;
    }

    // 255 в этих полях означает «неизвестно», отсекаем по границам шкал.
    Some(Signal {
        rsrq: nums[5].and_then(rsrq_db),
        rsrp: nums[6].and_then(rsrp_dbm),
        sinr: nums.get(7).copied().flatten().map(|v| v as f64),
        ..Default::default()
    })
}

/// `+XLEC: …,BAND_LTE_3,…` — состав агрегации. Отдаём как есть: формат
/// зависит от прошивки, и разбирать его по догадке смысла нет.
pub fn parse_xlec(body: &str) -> Option<String> {
    body.lines()
        .find(|l| l.contains("+XLEC:"))
        .map(|l| l.trim().to_string())
}

// ---------------------------------------------------------------------------
// Хранение состояния фиксации
// ---------------------------------------------------------------------------

/// Прочитать текущую фиксацию из модема Intel нечем: `freq_lock` только пишет.
/// Поэтому запоминаем сами. Состояние честно помечается в UI как «по нашим
/// данным», а не как показание модема.
pub struct LockStore {
    path: std::path::PathBuf,
}

impl LockStore {
    pub fn new(path: impl Into<std::path::PathBuf>) -> Self {
        LockStore { path: path.into() }
    }

    pub fn load(&self) -> Option<(u32, u16)> {
        let text = std::fs::read_to_string(&self.path).ok()?;
        parse_state(&text)
    }

    pub fn save(&self, earfcn: u32, pci: u16) {
        let _ = std::fs::write(&self.path, format!("{} {}\n", earfcn, pci));
    }

    pub fn clear(&self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn parse_state(text: &str) -> Option<(u32, u16)> {
    let mut it = text.split_whitespace();
    let earfcn = it.next()?.parse().ok()?;
    let pci = it.next()?.parse().ok()?;
    Some((earfcn, pci))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Дословный ответ Fibocom L860 (МегаФон), включая переносы строк,
    /// которыми модем разрывает длинные записи.
    const REAL_XMCI: &str = "+XMCI: 4,250,02,\"0x2608\",\"0x026D741E\",\"0x0108\",\"0x00000642\",\"0x00004C92\",\n\"0xFFFFFFFF\",58,22,38,\"0x00000003\",\"0x00000000\"\n\
+XMCI: 5,000,000,\"0xFFFE\",\"0xFFFFFFFF\",\"0x01B7\",\"0x00000BE8\",\"0xFFFFFFFF\",\n\"0xFFFFFFFF\",51,14,255,\"0x7FFFFFFF\",\"0x00000000\"\n\
+XMCI: 5,000,000,\"0xFFFE\",\"0xFFFFFFFF\",\"0x0108\",\"0x000000E1\",\"0xFFFFFFFF\",\n\"0xFFFFFFFF\",54,14,255,\"0x7FFFFFFF\",\"0x00000000\"\n\
+XMCI: 5,000,000,\"0xFFFE\",\"0xFFFFFFFF\",\"0x0108\",\"0x000005B2\",\"0xFFFFFFFF\",\n\"0xFFFFFFFF\",59,23,255,\"0x7FFFFFFF\",\"0x00000000\"\n\
+XMCI: 5,000,000,\"0xFFFE\",\"0xFFFFFFFF\",\"0x00EC\",\"0x00000642\",\"0xFFFFFFFF\",\n\"0xFFFFFFFF\",44,0,255,\"0x7FFFFFFF\",\"0x00000000\"\n\
+XMCI: 5,000,000,\"0xFFFE\",\"0xFFFFFFFF\",\"0x00EC\",\"0x000000E1\",\"0xFFFFFFFF\",\n\"0xFFFFFFFF\",41,0,255,\"0x7FFFFFFF\",\"0x00000000\"\nOK";

    #[test]
    fn parses_serving_cell_from_real_output() {
        let s = parse_serving(REAL_XMCI).expect("служебная сота должна найтись");
        assert_eq!(s.earfcn, Some(1602));
        assert_eq!(s.pci, Some(264));
        assert_eq!(s.rsrp, Some(-83.0));
        assert_eq!(s.rsrq, Some(-9.0));
        assert_eq!(s.band.as_deref(), Some("B3"));
    }

    #[test]
    fn parses_all_neighbors_from_real_output() {
        let n = parse_neighbors(REAL_XMCI);
        assert_eq!(n.len(), 5, "в ответе пять записей TYPE=5");
        // Отсортировано по убыванию RSRP: лучший — PCI 264 на 1458.
        assert_eq!((n[0].pci, n[0].earfcn), (264, 1458));
        assert_eq!(n[0].rsrp, Some(-82.0));
        // Худший — PCI 236 на 225.
        assert_eq!((n[4].pci, n[4].earfcn), (236, 225));
        assert_eq!(n[4].rsrp, Some(-100.0));
    }

    #[test]
    fn serving_cell_is_not_listed_as_neighbor() {
        let n = parse_neighbors(REAL_XMCI);
        assert!(!n.iter().any(|c| c.pci == 264 && c.earfcn == 1602));
    }

    #[test]
    fn treats_sentinel_values_as_missing() {
        assert_eq!(field("\"0xFFFFFFFF\""), None);
        assert_eq!(field("\"0x7FFFFFFF\""), None);
        assert_eq!(field(""), None);
        assert_eq!(field("\"0x0108\""), Some(264));
        assert_eq!(field("58"), Some(58));
    }

    #[test]
    fn parses_real_xcesq() {
        // Реальный ответ: +XCESQ: 0,99,99,255,255,24,62,34,255,255,255,255
        let s = parse_xcesq("+XCESQ: 0,99,99,255,255,24,62,34,255,255,255,255").unwrap();
        assert_eq!(s.rsrp, Some(-79.0));
        assert_eq!(s.rsrq, Some(-8.0));
    }

    #[test]
    fn xcesq_out_of_range_is_unknown() {
        let s = parse_xcesq("+XCESQ: 0,99,99,255,255,255,255,255").unwrap();
        assert_eq!(s.rsrp, None);
        assert_eq!(s.rsrq, None);
    }

    #[test]
    fn builds_lock_command_for_real_cell() {
        // B3 -> код 103, значения десятичные.
        assert_eq!(
            freq_lock_cmd(1602, 264, true).unwrap(),
            "at@sic:freq_lock(0,3,103,1,1602,264)"
        );
        assert_eq!(
            freq_lock_cmd(1602, 264, false).unwrap(),
            "at@sic:freq_lock(0,3,103,0,1602,264)"
        );
        assert_eq!(
            freq_lock_cmd(3048, 439, true).unwrap(),
            "at@sic:freq_lock(0,3,107,1,3048,439)"
        );
    }

    #[test]
    fn refuses_earfcn_outside_known_bands() {
        assert!(freq_lock_cmd(20000, 1, true).is_err());
    }

    #[test]
    fn parses_xlec_line() {
        let raw = parse_xlec("+XLEC: 0,4,5,5,4,3,BAND_LTE_3,0,7,1,3\nOK").unwrap();
        assert!(raw.contains("BAND_LTE_3"));
    }

    #[test]
    fn lock_state_roundtrip() {
        assert_eq!(parse_state("1602 264\n"), Some((1602, 264)));
        assert_eq!(parse_state(""), None);
        assert_eq!(parse_state("мусор"), None);
    }
}
