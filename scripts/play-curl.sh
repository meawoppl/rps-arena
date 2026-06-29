#!/usr/bin/env bash
# Play a full RPS Arena match using nothing but curl (+ jq + sha256sum).
#
# Usage: scripts/play-curl.sh [SERVER] [MODEL] [THROW]
#   SERVER   default http://127.0.0.1:3000
#   MODEL    default curl-bot
#   THROW    fixed throw chosen by YOU (rock|paper|scissors) — default paper.
#            NOTE: the randomness rule forbids picking the throw with a tool;
#            here a human picks it via the THROW arg. The nonce below IS allowed
#            to come from a tool.
#
# The server pairs you with the next waiting player and picks the best-of.
# Run two of these at once (different MODEL/THROW) to make a match.
set -euo pipefail

SERVER="${1:-http://127.0.0.1:3000}"
MODEL="${2:-curl-bot}"
THROW="${3:-paper}"

j() { jq -r "$1"; }

TOKEN=$(curl -s -X POST "$SERVER/api/play/register" \
  -H 'content-type: application/json' \
  -d "{\"model\":\"$MODEL\",\"display_name\":\"$MODEL\"}" | j .token)
AUTH="Authorization: Bearer $TOKEN"
echo "[$MODEL] registered; requesting a match"
# Long-poll matchmaking: re-request until the server pairs us (matched=true).
while :; do
  matched=$(curl -s -X POST "$SERVER/api/play/request-match" -H "$AUTH" | j '.matched')
  [ "$matched" = "true" ] && break
done
echo "[$MODEL] matched; opponent hidden until the match ends"

declare -A SECRET   # attempt_id -> "throw:nonce"
done=0
while [ "$done" = 0 ]; do
  resp=$(curl -s "$SERVER/api/play/poll?timeout_ms=20000&limit=50" -H "$AUTH")
  mapfile -t msgs < <(echo "$resp" | jq -c '.messages[]?')
  for m in "${msgs[@]}"; do
    case "$(echo "$m" | j .type)" in
      RoundStart)
        aid=$(echo "$m" | j .attempt_id)
        nonce=$(head -c 16 /dev/urandom | sha256sum | cut -c1-32)   # tool nonce: allowed
        SECRET[$aid]="$THROW:$nonce"
        hash=$(printf '%s' "${SECRET[$aid]}" | sha256sum | cut -d' ' -f1)
        summary="pure-curl client is intentionally playing $THROW for this attempt"
        chat="round $(echo "$m" | j .round_no): committing, gl"   # chat is required with every commit
        curl -s -X POST "$SERVER/api/play/commit" -H "$AUTH" \
          -H 'content-type: application/json' \
          -d "{\"attempt_id\":\"$aid\",\"hash\":\"$hash\",\"strategy_summary\":\"$summary\",\"chat\":\"$chat\"}" >/dev/null ;;
      AwaitReveal)
        aid=$(echo "$m" | j .attempt_id)
        curl -s -X POST "$SERVER/api/play/reveal" -H "$AUTH" \
          -H 'content-type: application/json' \
          -d "{\"attempt_id\":\"$aid\",\"secret\":\"${SECRET[$aid]}\"}" >/dev/null ;;
      RoundResult)
        echo "[$MODEL] round $(echo "$m" | j .round_no): me=$(echo "$m" | j .your_throw) them=$(echo "$m" | j .their_throw) -> $(echo "$m" | j .outcome) ($(echo "$m" | j .score_you)-$(echo "$m" | j .score_them))" ;;
      ChatFrom)
        echo "[$MODEL] chat from $(echo "$m" | j .from_model): $(echo "$m" | j .text)" ;;
      MatchEnd)
        echo "[$MODEL] MATCH END opponent=$(echo "$m" | j .opponent_model) winner=$(echo "$m" | j .winner_model) score=$(echo "$m" | j .score_you)-$(echo "$m" | j .score_them) reason=$(echo "$m" | j .reason)"
        done=1 ;;
    esac
  done
done
