#!/bin/sh
#
# Установщик modemui + lockband для Keenetic с Entware.
#
#   curl -fsSL https://raw.githubusercontent.com/magiasegura/nc-modem/main/setup.sh | sh
#
# Переменные окружения:
#   MODEMUI_REPO   репозиторий GitHub (по умолчанию magiasegura/nc-modem)
#   MODEMUI_PORT   порт веб-интерфейса (по умолчанию 1010)
#   MODEMUI_USER   логин Basic-аутентификации
#   MODEMUI_PASS   пароль Basic-аутентификации

set -eu

REPO="${MODEMUI_REPO:-magiasegura/nc-modem}"
RELEASE="https://github.com/$REPO/releases/latest/download"
PORT="${MODEMUI_PORT:-1010}"
BIN_DIR="/opt/bin"
ETC_DIR="/opt/etc"
INIT_DIR="/opt/etc/init.d"
CONF="$ETC_DIR/modemui.conf"
SERVICE="$INIT_DIR/S99modemui"

say() { echo "==> $*"; }
die() { echo "ошибка: $*" >&2; exit 1; }

[ -d /opt ] || die "не найден /opt — сначала установите Entware"
command -v curl >/dev/null 2>&1 || die "нужен curl: opkg install curl"

# --- определение архитектуры ------------------------------------------------

detect_target() {
	_m=$(uname -m 2>/dev/null || echo unknown)
	case "$_m" in
	aarch64 | arm64)
		echo "aarch64-unknown-linux-musl"
		return
		;;
	armv7l | armv7 | armhf)
		echo "armv7-unknown-linux-musleabihf"
		return
		;;
	esac

	# mips: uname не отличает порядок байт, спрашиваем opkg.
	_a=$(opkg print-architecture 2>/dev/null | awk '{print $2}' | tr '\n' ' ')
	case "$_a" in
	*mipsel*) echo "mipsel-unknown-linux-musl" ;;
	*mips*) echo "mips-unknown-linux-musl" ;;
	*aarch64*) echo "aarch64-unknown-linux-musl" ;;
	*arm*) echo "armv7-unknown-linux-musleabihf" ;;
	*) die "не удалось определить архитектуру (uname -m: $_m, opkg: $_a)" ;;
	esac
}

TARGET=$(detect_target)
say "архитектура: $TARGET"

# --- загрузка ---------------------------------------------------------------

URL="$RELEASE/modemui-$TARGET"
TMP="/tmp/modemui.$$"

say "качаю $URL"
curl -fsSL --retry 3 -o "$TMP" "$URL" || die "не скачать бинарь для $TARGET (возможно, релиза под эту архитектуру нет)"
chmod +x "$TMP"

mkdir -p "$BIN_DIR" "$ETC_DIR" "$INIT_DIR"

# Сервис мог быть запущен — останавливаем, иначе «Text file busy».
[ -x "$SERVICE" ] && "$SERVICE" stop >/dev/null 2>&1 || true
mv "$TMP" "$BIN_DIR/modemui"
say "установлен $BIN_DIR/modemui"

say "качаю lockband (CLI)"
if curl -fsSL --retry 3 -o "$BIN_DIR/lockband" "$RELEASE/lockband"; then
	chmod +x "$BIN_DIR/lockband"
	say "установлен $BIN_DIR/lockband"
else
	echo "предупреждение: lockband не скачался, веб-интерфейс это не ломает" >&2
fi

# --- конфиг -----------------------------------------------------------------

if [ ! -f "$CONF" ]; then
	cat >"$CONF" <<EOF
# Настройки modemui. После правки: $SERVICE restart

# Адрес и порт веб-интерфейса.
LISTEN="0.0.0.0:$PORT"

# Логин и пароль. Пустые значения = вход без пароля для всех в локальной сети.
USER="${MODEMUI_USER:-}"
PASS="${MODEMUI_PASS:-}"

# Явный выбор транспорта. Пусто = автоопределение.
# IFACE="UsbQmi0"
# DEV="/dev/ttyACM0"
IFACE=""
DEV=""
EOF
	chmod 600 "$CONF"
	say "создан $CONF"
else
	say "$CONF уже есть, не трогаю"
fi

# --- сервис -----------------------------------------------------------------

cat >"$SERVICE" <<'EOF'
#!/bin/sh

ENABLED=yes
PROCS=modemui
ARGS=""
PREARGS=""
DESC="Modem web UI"
PATH=/opt/sbin:/opt/bin:/opt/usr/sbin:/opt/usr/bin:/sbin:/bin:/usr/sbin:/usr/bin

CONF=/opt/etc/modemui.conf
[ -f "$CONF" ] && . "$CONF"

ARGS="-l ${LISTEN:-0.0.0.0:1010}"
[ -n "${USER:-}" ] && ARGS="$ARGS -u $USER"
[ -n "${PASS:-}" ] && ARGS="$ARGS -p $PASS"
[ -n "${IFACE:-}" ] && ARGS="$ARGS -i $IFACE"
[ -n "${DEV:-}" ] && ARGS="$ARGS -d $DEV"

. /opt/etc/init.d/rc.func
EOF
chmod 755 "$SERVICE"
say "создан $SERVICE"

if [ ! -f /opt/etc/init.d/rc.func ]; then
	die "нет /opt/etc/init.d/rc.func — установите пакет: opkg install entware-rc"
fi

"$SERVICE" restart || die "сервис не запустился, смотрите: $BIN_DIR/modemui -l 127.0.0.1:$PORT"

echo
say "готово"
echo "    Веб-интерфейс: http://$(ip -4 addr show br0 2>/dev/null | awk '/inet /{print $2}' | cut -d/ -f1 | head -n1):${PORT}"
echo "    Настройки:     $CONF"
echo "    CLI:           lockband --help"
if [ -z "${MODEMUI_USER:-}" ]; then
	echo
	echo "    ВНИМАНИЕ: пароль не задан, интерфейс открыт всем в локальной сети."
	echo "    Задайте USER и PASS в $CONF и выполните '$SERVICE restart'."
fi
