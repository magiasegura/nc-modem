//! SIMCom SIM79XX / Fibocom A7908E-M2.
//!
//! У этого семейства `at^efs=…` вырезан из прошивки, а публичной AT-команды
//! фиксации соты нет. Зато на роутерах Keenetic/Netcraze штатная CLI
//! `interface <iface> mobile lte lock earfcn <n> [pci <n>]` уже умеет её
//! ставить (внутри неё вызов идёт по QMI). Мы используем ровно её.
//!
//! Прочитать состояние фиксации так же обратно невозможно — храним последнюю
//! запись сами, как для Intel-модуля.

use std::path::PathBuf;

/// CLI-подкоманда для роутера (без префикса `interface <iface>`).
pub fn lock_earfcn_cli(earfcn: u16) -> String {
    format!("mobile lte lock earfcn {}", earfcn)
}

pub fn lock_pci_cli(earfcn: u32, pci: u16) -> String {
    format!("mobile lte lock earfcn {} pci {}", earfcn, pci)
}

pub fn unlock_cli() -> &'static str {
    "no mobile lte lock"
}

/// Персистентная запись «что мы зафиксировали». Читать фиксацию у SIMCom
/// нечем, поэтому UI показывает наше собственное состояние и помечает это.
pub struct LockStore {
    path: PathBuf,
}

impl LockStore {
    pub fn new(base: &std::path::Path) -> Self {
        LockStore {
            path: base.join("simcom_lock.txt"),
        }
    }

    /// Сохранить пару (earfcn, pci). None очищает.
    pub fn save(&self, earfcn: Option<u32>, pci: Option<u16>) {
        if earfcn.is_none() && pci.is_none() {
            let _ = std::fs::remove_file(&self.path);
            return;
        }
        let text = format!(
            "{} {}\n",
            earfcn.map(|v| v.to_string()).unwrap_or_default(),
            pci.map(|v| v.to_string()).unwrap_or_default(),
        );
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&self.path, text);
    }

    /// Прочитать сохранённую фиксацию (earfcn, pci); любое из полей может быть None.
    pub fn load(&self) -> Option<(Option<u32>, Option<u16>)> {
        let raw = std::fs::read_to_string(&self.path).ok()?;
        let mut it = raw.split_whitespace();
        let earfcn = it.next().and_then(|v| v.parse::<u32>().ok());
        let pci = it.next().and_then(|v| v.parse::<u16>().ok());
        if earfcn.is_none() && pci.is_none() {
            None
        } else {
            Some((earfcn, pci))
        }
    }

    pub fn clear(&self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_lock_commands_for_a7908() {
        assert_eq!(lock_earfcn_cli(2850), "mobile lte lock earfcn 2850");
        assert_eq!(
            lock_pci_cli(2850, 392),
            "mobile lte lock earfcn 2850 pci 392"
        );
        assert_eq!(unlock_cli(), "no mobile lte lock");
    }

    #[test]
    fn lock_store_roundtrip() {
        let dir = tempdir();
        let s = LockStore::new(&dir);
        assert!(s.load().is_none());

        s.save(Some(2850), Some(392));
        assert_eq!(s.load(), Some((Some(2850), Some(392))));

        s.save(Some(2850), None);
        assert_eq!(s.load(), Some((Some(2850), None)));

        s.clear();
        assert!(s.load().is_none());
    }

    fn tempdir() -> PathBuf {
        let base =
            std::env::temp_dir().join(format!("nc-modem-simcom-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&base);
        base
    }
}
