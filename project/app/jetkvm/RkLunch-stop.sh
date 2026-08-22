#!/bin/sh

rcK()
{
	for i in $(ls /oem/usr/etc/init.d/S??*) ;do
		[ ! -f "$i" ] && continue
		case "$i" in
			*.sh)
				(
					trap - INT QUIT TSTP
					set stop
					. $i
				)
				;;
			*)
				$i stop
				;;
		esac
	done

	for i in /userdata/init.d/S??*;do
		[ ! -f "$i" ] && continue
		case "$i" in
			*.sh)
				(
					trap - INT QUIT TSTP
					set stop
					. $i
				)
				;;
			*)
				$i stop
				;;
		esac
	done
}

echo "Stop Application ..."
killall jetkvm-rdp 2>/dev/null || true
killall jetkvm_app
killall udhcpc

while [ 1 ];
do
	sleep 1
	ps|grep jetkvm_app|grep -v grep
	if [ $? -ne 0 ]; then
		echo "jetkvm_app exit"
		break
	else
		echo "jetkvm_app active"
	fi
done

rcK