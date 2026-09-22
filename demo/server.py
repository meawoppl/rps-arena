#!/usr/bin/env python3
"""Local-only preview. Seeded synthetic records and a deterministic practice bot.
No database, credentials, external services, or production writes.
"""
import argparse
import hashlib
import json
import random
import uuid
from datetime import datetime, timedelta, timezone
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path
from urllib.parse import urlparse, parse_qs

ROOT = Path(__file__).resolve().parents[1]
THROWS = ['rock', 'paper', 'scissors']
MODELS = ['claude-opus-4-6', 'gpt-5.4', 'gemini-3.1-pro', 'deepseek-v3.2',
          'claude-sonnet-4-6', 'grok-4', 'qwen3.5', 'mistral-large-3', 'llama-4', 'human']

def stamp(value=None):
    return (value or datetime.now(timezone.utc)).isoformat()

def uid(rng=None):
    return str(uuid.UUID(int=rng.getrandbits(128))) if rng else str(uuid.uuid4())

def judge(a, b):
    return 'tie' if a == b else ('win' if (THROWS.index(a) - THROWS.index(b)) % 3 == 1 else 'lose')

def generate(seed=42, count=1248):
    rng = random.Random(seed)
    now = datetime.now(timezone.utc)
    details = []
    strategies = [
        'Their last sequence favors rock. I will test paper and watch for a counter.',
        'A repeated move may look unlikely. I will hold my choice for one more round.',
        'I expect a switch after that result. Scissors tests that read.',
        'The sample is small. I am varying my opening rather than overfitting a pattern.',
        'Their chat suggests paper, but I will treat the claim as a bluff.',
    ]
    chatter = ['Your move. I think I see the pattern.', 'That is exactly what I wanted you to think.',
               'Let’s see if you change your mind.', 'A little predictability can be useful.',
               'No tells this time.']
    for i in range(count):
        a, b = rng.sample(MODELS, 2)
        best = rng.choice([3, 5, 7])
        scores = [0, 0]
        rounds, chat = [], []
        number, attempt = 1, 1
        started = now - timedelta(minutes=(count - i) * 7)
        while max(scores) < best // 2 + 1:
            ta = rng.choice(THROWS)
            # Give the synthetic field varied skill while retaining valid RPS outcomes.
            advantage = .5 + (MODELS.index(b) - MODELS.index(a)) * .018
            outcome = rng.choices(['win', 'lose', 'tie'], [advantage * .7, (1 - advantage) * .7, .3])[0]
            tb = ta if outcome == 'tie' else THROWS[(THROWS.index(ta) + (2 if outcome == 'win' else 1)) % 3]
            rounds.append(dict(attempt_id=uid(rng), round_no=number, attempt_no=attempt,
                throw_a=ta, throw_b=tb, outcome_a=outcome,
                strategy_summary_a=rng.choice(strategies), strategy_summary_b=rng.choice(strategies)))
            if attempt == 1:
                chat.extend([dict(from_model=model, round_no=number, text=rng.choice(chatter),
                    created_at=stamp(started + timedelta(seconds=len(rounds) * 12 + j))) for j, model in enumerate([a, b])])
            if outcome != 'tie':
                scores[outcome == 'lose'] += 1
                number += 1
                attempt = 1
            else:
                attempt += 1
        summary = dict(match_id=uid(rng), best_of=best, model_a=a, model_b=b,
            score_a=scores[0], score_b=scores[1], winner_model=a if scores[0] > scores[1] else b,
            reason='win_by_score', started_at=stamp(started), ended_at=stamp(started + timedelta(seconds=len(rounds)*12)))
        details.append(dict(summary=summary, rounds=rounds, chat=chat))
    return details

def leaderboard(details):
    rows = {}
    for detail in details:
        m = detail['summary']
        for model in [m['model_a'], m['model_b']]:
            rows.setdefault(model, dict(model=model, matches=0, match_wins=0, match_losses=0,
                match_draws=0, match_win_rate=0, rounds=0, round_wins=0, round_losses=0,
                round_ties=0, round_win_rate=0, elo=1500., throw_dist=[0, 0, 0]))
        a, b = rows[m['model_a']], rows[m['model_b']]
        expected = 1 / (1 + 10 ** ((b['elo'] - a['elo']) / 400))
        delta = 24 * ((m['winner_model'] == m['model_a']) - expected)
        a['elo'] += delta
        b['elo'] -= delta
        for seat, row in [('a', a), ('b', b)]:
            won = m['winner_model'] == row['model']
            row['matches'] += 1
            row['match_wins' if won else 'match_losses'] += 1
            for r in detail['rounds']:
                row['rounds'] += 1
                row['throw_dist'][THROWS.index(r['throw_' + seat])] += 1
                outcome = judge(r['throw_' + seat], r['throw_' + ('b' if seat == 'a' else 'a')])
                row[{'win': 'round_wins', 'lose': 'round_losses', 'tie': 'round_ties'}[outcome]] += 1
            row['match_win_rate'] = row['match_wins'] / row['matches']
            row['round_win_rate'] = row['round_wins'] / row['rounds']
    return sorted(rows.values(), key=lambda r: -r['elo'])

class Preview(BaseHTTPRequestHandler):
    details = generate()
    sessions = {}

    def send_json(self, data, status=200):
        body = json.dumps(data).encode()
        self.send_response(status)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(body)))
        self.send_header('Cache-Control', 'no-store')
        self.end_headers()
        self.wfile.write(body)

    def session(self):
        return self.sessions.get(self.headers.get('Authorization', '').removeprefix('Bearer '))

    def do_GET(self):
        parsed = urlparse(self.path)
        path = parsed.path
        if path == '/api/leaderboard':
            return self.send_json(leaderboard(self.details))
        if path == '/api/matches':
            try:
                limit = min(100, max(0, int(parse_qs(parsed.query).get('limit', ['25'])[0])))
            except ValueError:
                return self.send_json({'error': 'invalid limit'}, 400)
            return self.send_json([d['summary'] for d in self.details[::-1][:limit]])
        if path.startswith('/api/matches/'):
            detail = next((d for d in self.details if d['summary']['match_id'] == path.split('/')[-1]), None)
            return self.send_json(detail or {'error': 'Match not found'}, 200 if detail else 404)
        if path == '/api/play/poll':
            game = self.session()
            if not game:
                return self.send_json({'error': 'Unknown practice session'}, 401)
            messages, game['messages'] = game['messages'], []
            return self.send_json({'messages': messages})
        if path.startswith('/api/'):
            return self.send_json({'error': 'Preview route not found'}, 404)
        root = (ROOT / 'frontend/dist').resolve()
        file = (root / path.lstrip('/')).resolve()
        if not file.is_relative_to(root):
            return self.send_json({'error': 'Not found'}, 404)
        if path == '/' or path == '/play' or path.startswith('/matches/'):
            file = root / 'index.html'
        if not file.is_file():
            return self.send_json({'error': 'Not found'}, 404)
        body = file.read_bytes()
        if file.name == 'index.html':
            body = body.replace(b'<head>', b'<head><meta name="arena-preview" content="synthetic">')
        mime = {'.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.wasm': 'application/wasm', '.svg': 'image/svg+xml'}.get(file.suffix, 'application/octet-stream')
        self.send_response(200)
        self.send_header('Content-Type', mime)
        self.send_header('Content-Length', str(len(body)))
        self.send_header('Cache-Control', 'no-store')
        self.end_headers()
        self.wfile.write(body)

    def next_round(self, game):
        game['attempt'] = uid()
        game['opponent_throw'] = THROWS[game['turn'] % 3]  # chosen before human commits
        game['messages'].append(dict(type='RoundStart', match_id=game['match'], attempt_id=game['attempt'],
            round_no=game['round'], attempt_no=game['retry'], score_you=game['you'], score_them=game['them'], rules='Local practice against a deterministic bot.'))

    def do_POST(self):
        try:
            data = json.loads(self.rfile.read(int(self.headers.get('Content-Length', '0'))) or b'{}')
        except (ValueError, json.JSONDecodeError):
            return self.send_json({'error': 'Invalid JSON'}, 400)
        path = urlparse(self.path).path
        if path == '/api/play/register':
            token = uid()
            self.sessions[token] = dict(match=uid(), you=0, them=0, round=1, retry=1, turn=0,
                messages=[], rounds=[], chat=[], started=stamp(), committed=None, done=False)
            return self.send_json(dict(token=token, agent_id=uid()))
        game = self.session()
        if not game:
            return self.send_json({'error': 'Unknown practice session'}, 401)
        if path == '/api/play/request-match':
            game['messages'].append(dict(type='MatchStart', match_id=game['match'], best_of=5, rules='Local synthetic practice opponent.'))
            self.next_round(game)
            return self.send_json(dict(matched=True, match_id=game['match'], best_of=5))
        if game['done'] or data.get('attempt_id') != game.get('attempt'):
            return self.send_json({'error': 'No active attempt'}, 400)
        if path == '/api/play/commit':
            if not data.get('chat', '').strip() or not data.get('strategy_summary', '').strip() or game['committed']:
                return self.send_json({'error': 'Supply a comment and strategy; commit only once.'}, 400)
            game['committed'] = data
            game['messages'].extend([dict(type='ChatFrom', from_model='opponent', text='Practice round. Can you spot my pattern?'),
                dict(type='AwaitReveal', attempt_id=game['attempt'])])
            return self.send_json({'ok': True})
        if path == '/api/play/reveal':
            commit = game['committed']
            secret = data.get('secret', '')
            ta = secret.split(':')[0]
            if not commit or ta not in THROWS or hashlib.sha256(secret.encode()).hexdigest() != commit['hash']:
                return self.send_json({'error': 'Invalid reveal'}, 400)
            tb = game['opponent_throw']
            outcome = judge(ta, tb)
            game['you'] += outcome == 'win'
            game['them'] += outcome == 'lose'
            game['messages'].append(dict(type='RoundResult', attempt_id=game['attempt'], round_no=game['round'],
                attempt_no=game['retry'], your_throw=ta, their_throw=tb, outcome=outcome, score_you=game['you'], score_them=game['them']))
            game['rounds'].append(dict(attempt_id=game['attempt'], round_no=game['round'], attempt_no=game['retry'],
                throw_a=ta, throw_b=tb, outcome_a=outcome, strategy_summary_a=commit['strategy_summary'],
                strategy_summary_b='Practice bot cycles rock, paper, scissors.'))
            game['chat'].append(dict(from_model='human', round_no=game['round'], text=commit['chat'], created_at=stamp()))
            game['committed'] = None
            if max(game['you'], game['them']) == 3:
                winner = 'human' if game['you'] == 3 else 'practice-bot'
                game['done'] = True
                game['messages'].append(dict(type='MatchEnd', winner_model=winner, opponent_model='practice-bot',
                    score_you=game['you'], score_them=game['them'], reason='win_by_score'))
                self.details.append(dict(summary=dict(match_id=game['match'], best_of=5, model_a='human', model_b='practice-bot',
                    score_a=game['you'], score_b=game['them'], winner_model=winner, reason='win_by_score',
                    started_at=game['started'], ended_at=stamp()), rounds=game['rounds'], chat=game['chat']))
            else:
                game['turn'] += 1
                game['round'] += outcome != 'tie'
                game['retry'] = game['retry'] + 1 if outcome == 'tie' else 1
                self.next_round(game)
            return self.send_json({'ok': True})
        self.send_json({'error': 'Unknown preview route'}, 404)

if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--port', type=int, default=8080)
    parser.add_argument('--export', type=Path, help='Write reproducible synthetic match fixtures as JSON and exit')
    args = parser.parse_args()
    if args.export:
        args.export.write_text(json.dumps(Preview.details, indent=2))
        print(f'Exported {len(Preview.details)} matches to {args.export}')
    else:
        print(f'RPS Arena preview: http://localhost:{args.port} — {len(Preview.details)} synthetic matches', flush=True)
        HTTPServer(('127.0.0.1', args.port), Preview).serve_forever()
