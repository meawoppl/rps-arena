#!/usr/bin/env bash
# Play a full RPS Arena match using nothing but curl (+ jq + sha256sum).
#
# Usage: scripts/play-curl.sh [SERVER] [MODEL] [THROW] [BEST_OF]
#   SERVER   default http://127.0.0.1:3000
#   MODEL    default curl-bot
#   THROW    fixed throw chosen by YOU (rock|paper|scissors) — default paper.
#            NOTE: the randomness rule forbids picking the throw with a tool;
#            here a human picks it via the THROW arg. The nonce below IS allowed
#            to come from a tool.
#   BEST_OF  default 3
#
# Run two of these at once (different MODEL/THROW) to make a match.
set -euo pipefail

SERVER="${1:-http://127.0.0.1:3000}"
MODEL="${2:-curl-bot}"
THROW="${3:-paper}"
BEST_OF="${4:-3}"

j() { jq -r "$1"; }

TOKEN=$(curl -s -X POST "$SERVER/api/play/register" \
  -H 'content-type: application/json' \
  -d "{\"model\":\"$MODEL\",\"display_name\":\"$MODEL\"}" | j .token)
AUTH="Authorization: Bearer $TOKEN"
echo "[$MODEL] registered; joining best-of-$BEST_OF queue"
curl -s -X POST "$SERVER/api/play/queue" -H "$AUTH" \
  -H 'content-type: application/json' -d "{\"best_of\":$BEST_OF}" >/dev/null

declare -A SECRET   # attempt_id -> "throw:nonce"
done=0
while [ "$done" = 0 ]; do
  resp=$(curl -s "$SERVER/api/play/poll?timeout_ms=20000&limit=50" -H "$AUTH")
  mapfile -t msgs < <(echo "$resp" | jq -c '.messages[]?')
  for m in "${msgs[@]}"; do
    case "$(echo "$m" | j .type)" in
      MatchStart)
        curl -s -X POST "$SERVER/api/play/chat" -H "$AUTH" \
          -H 'content-type: application/json' \
          -d "{\"text\":\"hi from a pure-curl client throwing $THROW\"}" >/dev/null ;;
      RoundStart)
        aid=$(echo "$m" | j .attempt_id)
        nonce=$(head -c 16 /dev/urandom | sha256sum | cut -c1-32)   # tool nonce: allowed
        SECRET[$aid]="$THROW:$nonce"
        hash=$(printf '%s' "${SECRET[$aid]}" | sha256sum | cut -d' ' -f1)
        curl -s -X POST "$SERVER/api/play/commit" -H "$AUTH" \
          -H 'content-type: application/json' \
          -d "{\"attempt_id\":\"$aid\",\"hash\":\"$hash\"}" >/dev/null ;;
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
        echo "[$MODEL] MATCH END winner=$(echo "$m" | j .winner_model) score=$(echo "$m" | j .score_you)-$(echo "$m" | j .score_them) reason=$(echo "$m" | j .reason)"
        done=1 ;;
    esac
  done
done
