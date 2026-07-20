//! Определение диапазона LTE по номеру несущей (EARFCN).
//!
//! Нужно для модемов Intel: команда `at@sic:freq_lock` требует номер band,
//! тогда как пользователь знает только EARFCN. Таблица — из 3GPP TS 36.101,
//! таблица 5.7.3-1 (E-UTRA channel numbers), диапазоны N_DL.

/// Границы N_DL: (band, первый EARFCN, последний EARFCN).
const NDL_RANGES: &[(u16, u32, u32)] = &[
    (1, 0, 599),
    (2, 600, 1199),
    (3, 1200, 1949),
    (4, 1950, 2399),
    (5, 2400, 2649),
    (6, 2650, 2749),
    (7, 2750, 3449),
    (8, 3450, 3799),
    (9, 3800, 4149),
    (10, 4150, 4749),
    (11, 4750, 4949),
    (12, 5010, 5179),
    (13, 5180, 5279),
    (14, 5280, 5379),
    (17, 5730, 5849),
    (18, 5850, 5999),
    (19, 6000, 6149),
    (20, 6150, 6449),
    (21, 6450, 6599),
    (22, 6600, 7399),
    (23, 7500, 7699),
    (24, 7700, 8039),
    (25, 8040, 8689),
    (26, 8690, 9039),
    (27, 9040, 9209),
    (28, 9210, 9659),
    (29, 9660, 9769),
    (30, 9770, 9869),
    (31, 9870, 9919),
    (32, 9920, 10359),
    (33, 36000, 36199),
    (34, 36200, 36349),
    (35, 36350, 36949),
    (36, 36950, 37549),
    (37, 37550, 37749),
    (38, 37750, 38249),
    (39, 38250, 38649),
    (40, 38650, 39649),
    (41, 39650, 41589),
    (42, 41590, 43589),
    (43, 43590, 45589),
    (44, 45590, 46589),
    (45, 46590, 46789),
    (46, 46790, 54539),
    (47, 54540, 55239),
    (48, 55240, 56739),
    (65, 65536, 66435),
    (66, 66436, 67335),
    (67, 67336, 67535),
    (68, 67536, 67835),
    (71, 68586, 68935),
];

/// Номер LTE-диапазона по EARFCN. `None` — если частота попала в пробел
/// между диапазонами (такие интервалы в таблице есть, например 4950..5009).
pub fn band_from_earfcn(earfcn: u32) -> Option<u16> {
    NDL_RANGES
        .iter()
        .find(|(_, lo, hi)| earfcn >= *lo && earfcn <= *hi)
        .map(|(band, _, _)| *band)
}

/// Кодировка диапазона для AT-команд Intel: LTE-диапазон N передаётся как 100+N.
///
/// Выведено из `AT+XACT?` реального L860: в списке одновременно есть
/// 1,2,4,5,8 (это UMTS) и 101,103,107,120,125,126,128,129,130,134,138..143,146,148,166 —
/// то есть LTE-диапазоны 1,3,7,20,25,26,28,29,30,34,38..43,46,48,66.
pub fn intel_band_code(band: u16) -> u16 {
    100 + band
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_bands_seen_on_real_modem() {
        // Значения из вывода AT+XMCI=1 на Fibocom L860 (МегаФон).
        assert_eq!(band_from_earfcn(1602), Some(3));
        assert_eq!(band_from_earfcn(1458), Some(3));
        assert_eq!(band_from_earfcn(225), Some(1));
        assert_eq!(band_from_earfcn(3048), Some(7));
    }

    #[test]
    fn resolves_band_edges() {
        assert_eq!(band_from_earfcn(0), Some(1));
        assert_eq!(band_from_earfcn(599), Some(1));
        assert_eq!(band_from_earfcn(600), Some(2));
        assert_eq!(band_from_earfcn(1200), Some(3));
        assert_eq!(band_from_earfcn(1949), Some(3));
        assert_eq!(band_from_earfcn(1950), Some(4));
        assert_eq!(band_from_earfcn(6150), Some(20));
        assert_eq!(band_from_earfcn(38650), Some(40));
        assert_eq!(band_from_earfcn(66436), Some(66));
    }

    #[test]
    fn gaps_between_bands_are_unknown() {
        // 4950..5009 и 10360..35999 в таблице не покрыты.
        assert_eq!(band_from_earfcn(5000), None);
        assert_eq!(band_from_earfcn(20000), None);
        assert_eq!(band_from_earfcn(69000), None);
    }

    #[test]
    fn intel_codes_match_xact_listing() {
        assert_eq!(intel_band_code(1), 101);
        assert_eq!(intel_band_code(3), 103);
        assert_eq!(intel_band_code(7), 107);
        assert_eq!(intel_band_code(20), 120);
        assert_eq!(intel_band_code(66), 166);
    }
}
