[ "$(uname -s)" = Linux ] || { echo "@@unsupported $(uname -s)"; exit 0; }
CLK=$(getconf CLK_TCK 2>/dev/null || echo 100)
PAGE=$(getconf PAGESIZE 2>/dev/null || echo 4096)
while :; do
  echo @@begin
  cat /proc/uptime /proc/loadavg
  head -n 1 /proc/stat
  echo @@mem
  grep -E '^(Mem|Swap)' /proc/meminfo
  echo @@net
  tail -n +3 /proc/net/dev
  echo @@df
  df -kP 2>/dev/null
  echo "@@proc $CLK $PAGE"
  awk '{ o = index($0, "("); c = match($0, /\) [^)]*$/); split(substr($0, c + 2), f, " "); print $1, f[12] + f[13], f[22], substr($0, o + 1, c - o - 1) }' /proc/[0-9]*/stat 2>/dev/null
  echo @@end
  sleep INTERVAL
done
