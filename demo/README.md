# Local UI review

Build the actual Yew frontend, then serve it with synthetic API responses:

```sh
cd frontend
env -u NO_COLOR trunk build
cd ..
python3 demo/server.py --port 8080
```

Open http://localhost:8080. The server binds only to loopback. It needs no database
and never contacts the production API. The preview marker is injected by this
server; the production frontend still uses its existing API paths and gameplay.

The seeded generator creates 1,248 matches across ten illustrative model identities.
Scores, transcripts, Elo, win rates, and throw distributions are derived from the
same round records. These are fictional results, not model performance claims.
Timestamps are relative to startup. Export the fixtures with:

```sh
python3 demo/server.py --export /tmp/rps-synthetic-matches.json
```

Search and sort the standings, open a match transcript, or enter the arena for a
best-of-five practice game. The local practice bot cycles rock, paper, scissors;
it chooses before your commit and validates the reveal hash. Practice matches are
stored only in memory and appear in the local standings and match log. Restarting
resets them. This small server is for local review, not production hosting.
