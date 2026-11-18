#!/bin/sh
# Transparent proxying on the EU relay box: nginx connects to the relay (4443) and the web sites (8443) from
# the client's own address, so the backends see players' real addresses. Their replies are marked and routed
# back to nginx instead of out to the internet. Idempotent; run before nginx starts (ember-tproxy.service).
set -e
for t in iptables ip6tables; do
    for port in 4443 8443; do
        $t -t mangle -C OUTPUT -p tcp --sport $port -j MARK --set-mark 0x1 2>/dev/null ||
            $t -t mangle -A OUTPUT -p tcp --sport $port -j MARK --set-mark 0x1
    done
done
ip rule show | grep -q "fwmark 0x1 lookup 100" || ip rule add fwmark 0x1 lookup 100
ip -6 rule show | grep -q "fwmark 0x1 lookup 100" || ip -6 rule add fwmark 0x1 lookup 100
ip route replace local 0.0.0.0/0 dev lo table 100
ip -6 route replace local ::/0 dev lo table 100
