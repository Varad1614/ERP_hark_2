#!/usr/bin/env bash
# Records the facts of the host SutraERP is installed on (Part 2 · S5-deploy) — CPU, memory, disk,
# OS, kernel, virtualisation, network, listening ports, Docker — plus three quick micro-benchmarks
# (sequential write, fsync-per-write latency, single-core hashing) that make two hosts comparable.
# Used on the GitHub runner acting as the staging host, and meant to be run unchanged on the college
# server (INFRASTRUCTURE_REPORT.md "To complete on the college server").
#
#   host-facts.sh <out dir> [label]     → <out>/host-facts-<label>.md and .json
# Read-only except a 1 GiB temporary file in $HOST_FACTS_TMP (default /var/tmp), removed afterwards.
set -uo pipefail
out=${1:?usage: host-facts.sh <out dir> [label]}
label=${2:-host}
mkdir -p "$out"
md="$out/host-facts-$label.md"
tmpd=${HOST_FACTS_TMP:-/var/tmp}

sec() { printf '\n## %s\n\n```\n' "$1" >>"$md"; }
endsec() { printf '```\n' >>"$md"; }
run() { echo "\$ $*" >>"$md"; "$@" >>"$md" 2>&1 || echo "(exit $?)" >>"$md"; }

cpu_model=$(awk -F': ' '/^model name/ { print $2; exit }' /proc/cpuinfo)
vcpu=$(nproc)
mem_kib=$(awk '/MemTotal/ { print $2 }' /proc/meminfo)
mem_gib=$(awk -v k="$mem_kib" 'BEGIN { printf "%.1f", k / 1048576 }')
root_disk=$(df -BG --output=size,avail / | tail -1 | awk '{ print $1 "/" $2 }')
virt=$(systemd-detect-virt 2>/dev/null || echo unknown)
os=$(. /etc/os-release && echo "$PRETTY_NAME")
kernel=$(uname -r)

{
  echo "# Host facts — $label"
  echo
  echo "Recorded $(date -u +%FT%TZ) on \`$(hostname)\`."
  echo
  echo "| Fact | Value |"
  echo "|---|---|"
  echo "| CPU model | $cpu_model |"
  echo "| vCPU | $vcpu |"
  echo "| RAM | $mem_gib GiB |"
  echo "| Root disk (size/free) | $root_disk |"
  echo "| OS | $os |"
  echo "| Kernel | $kernel |"
  echo "| Virtualisation | $virt |"
  echo "| Docker | $(docker version -f '{{.Server.Version}}' 2>/dev/null || echo none) (compose $(docker compose version --short 2>/dev/null || echo none)) |"
} >"$md"

# Micro-benchmarks (comparable across hosts; not a capacity claim).
seq_mbs=$(dd if=/dev/zero of="$tmpd/hf.$$" bs=1M count=1024 oflag=direct conv=fsync 2>&1 | awk '/copied/ { print $(NF-1), $NF }')
rm -f "$tmpd/hf.$$"
fsync_line=$(dd if=/dev/zero of="$tmpd/hf-sync.$$" bs=8k count=2000 oflag=dsync 2>&1 | awk '/copied/ { print $(NF-3), $(NF-2) }')
rm -f "$tmpd/hf-sync.$$"
fsync_secs=$(echo "$fsync_line" | awk '{ print $1 }')
fsync_ms=$(awk -v s="${fsync_secs:-0}" 'BEGIN { printf "%.3f", s * 1000 / 2000 }')
sha_mbs=$(openssl speed -seconds 2 -bytes 16384 sha256 2>/dev/null | awk '/^sha256/ { v=$NF; sub(/k$/, "", v); printf "%.0f", v / 1000 }')
{
  echo "| Sequential write 1 GiB (O_DIRECT) | $seq_mbs |"
  echo "| 8 KiB O_DSYNC write latency (2000 writes) | ${fsync_ms} ms/write |"
  echo "| SHA-256 single core (16 KiB blocks) | ${sha_mbs} MB/s |"
} >>"$md"

sec "lscpu"; run lscpu; endsec
sec "memory"; run free -m; run swapon --show; endsec
sec "disks"; run lsblk -o NAME,SIZE,TYPE,ROTA,MOUNTPOINT,FSTYPE; run df -hT -x tmpfs -x overlay -x squashfs; endsec
sec "OS / kernel"; run cat /etc/os-release; run uname -a; run systemd-detect-virt; endsec
sec "time sync"; run timedatectl; endsec
sec "network"; run ip -brief address; run ip route; run cat /etc/resolv.conf; endsec
sec "listening ports"; run sudo ss -tlnp; endsec
sec "docker"; run docker version; run docker info --format '{{json .}}'; endsec
sec "limits"; run ulimit -a; run sysctl vm.swappiness vm.overcommit_memory fs.file-max net.core.somaxconn; endsec

python3 - "$out/host-facts-$label.json" <<PY
import json, sys
json.dump({"label": "$label", "cpuModel": "$cpu_model", "vcpu": $vcpu, "ramGiB": $mem_gib,
           "rootDiskSizeFreeGiB": "$root_disk", "os": "$os", "kernel": "$kernel", "virtualisation": "$virt",
           "seqWrite1GiB": "$seq_mbs", "dsync8kMsPerWrite": "$fsync_ms", "sha256MBs": "${sha_mbs:-0}"},
          open(sys.argv[1], "w"), indent=2)
PY
echo "host facts: $cpu_model, $vcpu vCPU, $mem_gib GiB RAM, disk $root_disk GiB, $os, kernel $kernel, $virt; seq write $seq_mbs; dsync ${fsync_ms} ms; sha256 ${sha_mbs} MB/s"
