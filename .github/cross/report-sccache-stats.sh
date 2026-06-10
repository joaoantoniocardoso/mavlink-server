#!/usr/bin/env bash
set -euo pipefail

log_file="${1:-sccache-build.log}"

format_duration() {
  local duration="$1"
  local secs nanos ms
  secs="$(echo "$duration" | jq '.secs // 0')"
  nanos="$(echo "$duration" | jq '.nanos // 0')"
  ms=$((nanos / 1000000))
  echo "${secs}s ${ms}ms"
}

stats="$(grep '^{"stats"' "$log_file" | tail -1)"
human_stats="$(
  awk '/>>> SCCACHE_HUMAN_STATS_START >>>/,/>>> SCCACHE_HUMAN_STATS_END >>>/' "$log_file" \
    | sed '1d;$d'
)"

if [[ -z "$stats" ]]; then
  echo "::warning title=sccache stats::No sccache stats found in build log"
  if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
    {
      echo "## sccache stats"
      echo
      echo "No sccache stats found in \`${log_file}\`."
    } >>"$GITHUB_STEP_SUMMARY"
  fi
  exit 0
fi

hits="$(echo "$stats" | jq '[.stats.cache_hits.counts | to_entries[]?.value // 0] | add // 0')"
misses="$(echo "$stats" | jq '[.stats.cache_misses.counts | to_entries[]?.value // 0] | add // 0')"
errors="$(echo "$stats" | jq '[.stats.cache_errors.counts | to_entries[]?.value // 0] | add // 0')"
executed="$(echo "$stats" | jq '.stats.requests_executed // 0')"
compile_requests="$(echo "$stats" | jq '.stats.compile_requests // 0')"
cache_writes="$(echo "$stats" | jq '.stats.cache_writes // 0')"
cache_write_errors="$(echo "$stats" | jq '.stats.cache_write_errors // 0')"
cache_write_duration="$(format_duration "$(echo "$stats" | jq '.stats.cache_write_duration // {}')")"
cache_read_hit_duration="$(format_duration "$(echo "$stats" | jq '.stats.cache_read_hit_duration // {}')")"
compiler_write_duration="$(format_duration "$(echo "$stats" | jq '.stats.compiler_write_duration // {}')")"

total=$((hits + misses + errors))
if (( total > 0 )); then
  rate=$((hits * 100 / total))
else
  rate=0
fi

echo "::notice title=sccache stats::${rate}% - ${hits} hits, ${misses} misses, ${errors} errors"

if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
  {
    echo "## sccache stats"
    echo
    echo "| Cache hit % | ${rate}% |"
    echo "| Cache hits | ${hits} |"
    echo "| Cache misses | ${misses} |"
    echo "| Cache errors | ${errors} |"
    echo "| Compile requests | ${compile_requests} |"
    echo "| Requests executed | ${executed} |"
    echo "| Cache writes | ${cache_writes} |"
    echo "| Cache write errors | ${cache_write_errors} |"
    echo "| Cache write duration | ${cache_write_duration} |"
    echo "| Cache read hit duration | ${cache_read_hit_duration} |"
    echo "| Compiler write duration | ${compiler_write_duration} |"
    echo
    echo "<details>"
    echo "<summary>Full human-readable stats</summary>"
    echo
    echo '```'
    if [[ -n "$human_stats" ]]; then
      echo "$human_stats"
    else
      echo "(not captured in build log)"
    fi
    echo '```'
    echo
    echo "</details>"
    echo
    echo "<details>"
    echo "<summary>Full JSON Stats</summary>"
    echo
    echo '```json'
    echo "$stats" | jq '.'
    echo '```'
    echo
    echo "</details>"
  } >>"$GITHUB_STEP_SUMMARY"
fi
