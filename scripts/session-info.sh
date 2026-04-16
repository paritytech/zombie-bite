#!/usr/bin/env bash
curl -s -H "Content-Type: application/json" \
  -d '{"id":1,"jsonrpc":"2.0","method":"chain_getHeader","params":[]}' \
  https://rpc.polkadot.io | python3 -c "
import sys, json
n = int(json.load(sys.stdin)['result']['number'], 16)
session_len = 2400
current_session = (n // session_len) * session_len
next_session = current_session + session_len
print(f'Current block:          {n}')
print(f'Current session at:     {current_session}')
print(f'Previous session at:    {current_session - session_len}')
print(f'Next session at:        {next_session}, in {next_session - n} blocks (~{round((next_session - n) * 6 / 60)} min)')
"
