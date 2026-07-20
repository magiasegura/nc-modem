//! modemui — веб-интерфейс расширенных функций LTE-модема для роутеров Keenetic.
//!
//! Функции, которых нет в штатном интерфейсе Keenetic: фиксация несущей/сектора
//! через EFS модема, мониторинг сигнала с историей, скан соседних сот,
//! управление бэндами, AT-консоль.

mod api;
mod at;
mod http;
mod json;
mod modem;

use std::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;

const DEFAULT_PORT: u16 = 1010;
/// Период фонового опроса метрик.
const POLL_INTERVAL: Duration = Duration::from_secs(5);

struct Config {
    listen: String,
    iface: Option<String>,
    dev: Option<String>,
    user: Option<String>,
    pass: Option<String>,
    demo: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            listen: format!("0.0.0.0:{}", DEFAULT_PORT),
            iface: None,
            dev: None,
            user: None,
            pass: None,
            demo: false,
        }
    }
}

fn usage() -> String {
    format!(
        "modemui {} - веб-интерфейс LTE-модема для Keenetic

Использование: modemui [флаги]

  -l, --listen <addr:port>  адрес прослушивания (по умолчанию 0.0.0.0:{})
  -i, --iface <IF>          интерфейс ndmc (UsbQmi0/UsbLte0), иначе автоопределение
  -d, --dev <path>          AT-порт напрямую (/dev/ttyACM0), иначе автоопределение
  -u, --user <name>         логин Basic-аутентификации
  -p, --pass <pass>         пароль Basic-аутентификации
      --demo                фиктивный модем: посмотреть интерфейс без железа
  -h, --help                эта справка

Без -u/-p интерфейс доступен без пароля всем в локальной сети.",
        env!("CARGO_PKG_VERSION"),
        DEFAULT_PORT
    )
}

fn parse_args() -> Result<Config, String> {
    let mut cfg = Config::default();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;

    while i < args.len() {
        let take = |i: &mut usize, name: &str| -> Result<String, String> {
            *i += 1;
            args.get(*i)
                .cloned()
                .ok_or_else(|| format!("{} требует значение", name))
        };
        match args[i].as_str() {
            "-l" | "--listen" => cfg.listen = take(&mut i, "--listen")?,
            "-i" | "--iface" => cfg.iface = Some(take(&mut i, "--iface")?),
            "-d" | "--dev" => cfg.dev = Some(take(&mut i, "--dev")?),
            "-u" | "--user" => cfg.user = Some(take(&mut i, "--user")?),
            "-p" | "--pass" => cfg.pass = Some(take(&mut i, "--pass")?),
            "--demo" => cfg.demo = true,
            "-h" | "--help" => {
                println!("{}", usage());
                std::process::exit(0);
            }
            other => return Err(format!("неизвестный аргумент: {}", other)),
        }
        i += 1;
    }
    Ok(cfg)
}

fn main() {
    let cfg = match parse_args() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("modemui: {}\n\n{}", e, usage());
            std::process::exit(1);
        }
    };

    let detected = if cfg.demo {
        eprintln!("modemui: РЕЖИМ DEMO — модем не опрашивается, данные вымышленные");
        Some(at::Transport::Mock)
    } else {
        at::detect(cfg.iface.as_deref(), cfg.dev.as_deref())
    };

    let transport = match detected {
        Some(t) => t,
        None => {
            eprintln!(
                "modemui: не удалось найти модем.
  - ndmc: `ndmc -c 'show interface'` должен показывать UsbQmi0 или UsbLte0
    (AT-команды поддерживаются с KeeneticOS 3.9);
  - прямой порт: `ls /dev/ttyACM*`, порт может быть занят прошивкой;
  - можно указать явно: -i UsbQmi0 или -d /dev/ttyACM0"
            );
            std::process::exit(2);
        }
    };

    println!("modemui: транспорт {}", transport.describe());

    let modem = Arc::new(modem::Modem::new(transport));
    modem.probe();

    let caps = modem.caps();
    if !caps.efs {
        eprintln!(
            "modemui: ВНИМАНИЕ - модем не отвечает на at^efs, фиксация EARFCN/PCI работать не будет"
        );
    }
    println!(
        "modemui: возможности - efs={} serving={:?} neighbors={:?} bands={:?}",
        caps.efs, caps.serving, caps.neighbors, caps.bands_query
    );

    // Фоновый опрос метрик: HTTP-запросы отдают закэшированное значение,
    // иначе каждый тик UI дёргал бы AT-порт и блокировал операции записи.
    {
        let modem = Arc::clone(&modem);
        std::thread::spawn(move || loop {
            modem.poll_signal();
            std::thread::sleep(POLL_INTERVAL);
        });
    }

    let auth = match (cfg.user.clone(), cfg.pass.clone()) {
        (Some(u), Some(p)) => Some((u, p)),
        _ => {
            eprintln!(
                "modemui: ВНИМАНИЕ - аутентификация выключена, интерфейс открыт всем в LAN.\n\
                 Задайте -u <логин> -p <пароль>."
            );
            None
        }
    };

    let listener = match TcpListener::bind(&cfg.listen) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("modemui: не занять {}: {}", cfg.listen, e);
            std::process::exit(1);
        }
    };
    println!("modemui: слушаю http://{}", cfg.listen);

    let handler_modem = Arc::clone(&modem);
    if let Err(e) = http::serve(listener, auth, move |req| api::route(&handler_modem, req)) {
        eprintln!("modemui: сервер остановлен: {}", e);
        std::process::exit(1);
    }
}
