#!/bin/sh

source /oem/usr/share/jetkvm-lib.sh

rcS()
{
	for i in /oem/usr/etc/init.d/S??* ;do

		# Ignore dangling symlinks (if any).
		[ ! -f "$i" ] && continue

		case "$i" in
			*.sh)
				(
					trap - INT QUIT TSTP
					set start
					. $i
				)
				;;
			*)
				$i start
				;;
		esac
	done

	for i in /userdata/init.d/S??* ;do
		[ ! -f "$i" ] && continue
		case "$i" in
			*.sh)
				(
					trap - INT QUIT TSTP
					set start
					. $i
				)
				;;
			*)
				$i start
				;;
		esac
	done
}

check_linker()
{
        [ ! -L "$2" ] && ln -sf $1 $2
}

network_init()
{
	ifup lo
	ifconfig eth0 down
	set_up_mac_address | tee /dev/kmsg

	ifconfig eth0 up && (
		hostname=$(hostname 2>/dev/null)
		if echo "$hostname" | grep -Eq '^[a-zA-Z0-9]([a-zA-Z0-9\-]{0,61}[a-zA-Z0-9])?$'; then
			udhcpc -i eth0 -x hostname:"$hostname"
		else
			udhcpc -i eth0
		fi
	)
}

start_rdp_console()
{
	RDP_BIN=/userdata/jetkvm/bin/jetkvm-rdp
	RDP_SOCKET=${JETKVM_RDP_SOCKET:-/run/jetkvm-rdp.sock}
	[ ! -x "$RDP_BIN" ] && return 0

	(
		# Do not listen on 3389 until jetkvm_app has initialised the native
		# HDMI/HID bridge. If jetkvm_app is still booting, MSTSC should see a
		# closed port rather than connecting to a console that cannot start video.
		while [ ! -S "$RDP_SOCKET" ]; do
			sleep .2
		done

		while true; do
			JETKVM_RDP_BIND=${JETKVM_RDP_BIND:-0.0.0.0:3389} \
			JETKVM_RDP_SOCKET="$RDP_SOCKET" \
			RUST_LOG=${RUST_LOG:-info} \
			"$RDP_BIN" >> /userdata/jetkvm/rdp.log 2>&1
			echo "jetkvm-rdp exited; restarting in 2 seconds" >> /userdata/jetkvm/rdp.log
			sleep 2
		done
	) &
}

post_chk()
{
	cnt=0
	while [ $cnt -lt 30 ];
	do
		cnt=$(( cnt + 1 ))
		if mount | grep -w userdata; then
			break
		fi
		sleep .1
	done

	default_ko_dir=/ko
	if [ -f "/oem/usr/ko/insmod_ko.sh" ];then
		default_ko_dir=/oem/usr/ko
	fi
	if [ -f "$default_ko_dir/insmod_ko.sh" ];then
		cd $default_ko_dir && sh insmod_ko.sh && cd -
	fi

	modules_path="/lib/modules/$(uname -r)"
	if [ ! -d "/lib/modules" ]; then
		mkdir -p "/lib/modules"
	fi
	if [ ! -e "$modules_path" ]; then
		ln -s "$default_ko_dir" "$modules_path"
	fi

	network_init &
	if [ -f "/userdata/jetkvm/jetkvm_app.update" ]; then
		mv -f /userdata/jetkvm/jetkvm_app.update /userdata/jetkvm/bin/jetkvm_app
	fi

	dropbear.sh &
	chmod +x /userdata/jetkvm/bin/jetkvm_app
	/userdata/jetkvm/bin/jetkvm_app > /userdata/jetkvm/last.log 2>&1 &
	start_rdp_console
}

rcS

ulimit -c unlimited
echo "/data/core-%p-%e" > /proc/sys/kernel/core_pattern

echo 1 > /proc/sys/vm/overcommit_memory

post_chk &