#!/bin/sh
#
# Удаление modemui + lockband с роутера Keenetic.
#
#   curl -fsSL https://raw.githubusercontent.com/magiasegura/nc-modem/main/uninstall.sh | sh -s -- --unlock
#
# Флаги:
#   --unlock      снять фиксацию соты перед удалением (настоятельно рекомендуется)
#   --keep-lock   удалить, оставив фиксацию как есть
#   --purge       удалить и конфиг /opt/etc/modemui.conf
#   -y, --yes     не задавать вопросов
#
# ВАЖНО. Фиксация несущей/сектора живёт в NV-памяти модема, а не в этих файлах.
# После удаления снять её будет нечем: если зафиксированной соты нет в эфире,
# модем останется без регистрации, то есть и без интернета. Поэтому скрипт
# отказывается работать при активной фиксации, пока вы не выберете --unlock
# или --keep-lock осознанно.

set -eu

BIN_DIR="/opt/bin"
CONF="/opt/etc/modemui.conf"
SERVICE="/opt/etc/init.d/S99modemui"

DO_UNLOCK=""
PURGE=0
ASSUME_YES=0

say() { echo "==> $*"; }
warn() { echo "предупреждение: $*" >&2; }
die() {
	echo "ошибка: $*" >&2
	exit 1
}

while [ $# -gt 0 ]; do
	case "$1" in
	--unlock) DO_UNLOCK=yes ;;
	--keep-lock) DO_UNLOCK=no ;;
	--purge) PURGE=1 ;;
	-y | --yes) ASSUME_YES=1 ;;
	-h | --help)
		sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'
		exit 0
		;;
	*) die "неизвестный аргумент: $1" ;;
	esac
	shift
done

# При запуске через `curl | sh` stdin занят телом скрипта, и read прочитал бы
# не ответ пользователя, а следующие строки самого скрипта.
interactive() {
	[ -t 0 ] && [ "$ASSUME_YES" -eq 0 ]
}

ask() {
	if ! interactive; then
		return 1
	fi
	printf '%s [y/N] ' "$1"
	read -r _a
	case "$_a" in y | Y | yes | YES) return 0 ;; *) return 1 ;; esac
}

# --- 1. фиксация соты -------------------------------------------------------

lock_is_active() {
	[ -x "$BIN_DIR/lockband" ] || return 1
	_out=$("$BIN_DIR/lockband" status 2>/dev/null) || return 1
	printf '%s\n' "$_out" |
		grep -E '^(earfcn_lock|pci_lock)' |
		grep -qv 'not set'
}

if [ -x "$BIN_DIR/lockband" ]; then
	if lock_is_active; then
		echo
		warn "в модеме активна фиксация соты:"
		"$BIN_DIR/lockband" status 2>/dev/null | sed 's/^/    /'
		echo
		echo "    Она хранится в NV-памяти модема и удалением файлов не снимается."
		echo "    После удаления снять её будет нечем."
		echo

		if [ -z "$DO_UNLOCK" ]; then
			if interactive; then
				if ask "Снять фиксацию сейчас?"; then
					DO_UNLOCK=yes
				else
					DO_UNLOCK=no
				fi
			else
				# Спросить некого (curl | sh либо -y), а выбор слишком
				# ответственный, чтобы делать его за пользователя молча.
				echo "    Запустите повторно с одним из флагов:" >&2
				echo "      --unlock      снять фиксацию и удалить (обычно нужен этот)" >&2
				echo "      --keep-lock   удалить, оставив фиксацию в модеме" >&2
				die "нужен явный выбор, что делать с фиксацией"
			fi
		fi

		if [ "$DO_UNLOCK" = yes ]; then
			say "снимаю фиксацию"
			"$BIN_DIR/lockband" -y unlock || die "не удалось снять фиксацию, удаление прервано"
			say "фиксация снята, модем перезагружается"
		else
			warn "фиксация оставлена активной — это ваш осознанный выбор"
		fi
	else
		say "фиксация соты не установлена"
	fi
else
	warn "lockband не найден, проверить фиксацию нечем"
	warn "если она была установлена — снимите её до удаления"
	if [ -z "$DO_UNLOCK" ] && ! ask "Продолжить удаление?"; then
		die "отменено"
	fi
fi

# --- 2. сервис --------------------------------------------------------------

if [ -x "$SERVICE" ]; then
	say "останавливаю сервис"
	"$SERVICE" stop >/dev/null 2>&1 || warn "сервис не остановился, продолжаю"
	rm -f "$SERVICE"
	say "удалён $SERVICE"
fi

# Подстраховка: сервис мог быть остановлен неудачно, а держать порт открытым.
if command -v pidof >/dev/null 2>&1 && pidof modemui >/dev/null 2>&1; then
	warn "процесс modemui ещё жив, завершаю"
	kill "$(pidof modemui)" 2>/dev/null || true
fi

# --- 3. файлы ---------------------------------------------------------------

for f in "$BIN_DIR/modemui" "$BIN_DIR/lockband"; do
	if [ -e "$f" ]; then
		rm -f "$f"
		say "удалён $f"
	fi
done

if [ -f "$CONF" ]; then
	if [ "$PURGE" -eq 1 ]; then
		rm -f "$CONF"
		say "удалён $CONF"
	else
		say "$CONF оставлен (для удаления запустите с --purge)"
	fi
fi

echo
say "готово"
if [ "${DO_UNLOCK:-}" = no ]; then
	echo
	echo "    ПОМНИТЕ: фиксация соты осталась в модеме. Чтобы снять её позже,"
	echo "    придётся поставить lockband заново либо отправить модему вручную:"
	echo "      at^efs=\"/nv/item_files/modem/lte/rrc/csp/earfcn_lock\",0"
	echo "      at^efs=\"/nv/item_files/modem/lte/rrc/csp/pci_lock\",0"
	echo "      at+cfun=1,1"
fi
