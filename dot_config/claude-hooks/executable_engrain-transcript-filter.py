#!/usr/bin/env python3
# Reduces a Claude Code transcript (JSONL on stdin) to plain dialogue text:
# user and assistant message text blocks only, tool payloads dropped, each
# turn truncated, total output capped. Keeps digest prompts inside the model
# context limit no matter how tool-heavy the session was.
import json
import sys

PER_TURN_CAP = 1500
TOTAL_CAP = 60_000

out = []
total = 0
for raw in sys.stdin:
    raw = raw.strip()
    if not raw:
        continue
    try:
        entry = json.loads(raw)
    except json.JSONDecodeError:
        continue
    role = entry.get("type")
    if role not in ("user", "assistant"):
        continue
    msg = entry.get("message") or {}
    content = msg.get("content")
    texts = []
    if isinstance(content, str):
        texts.append(content)
    elif isinstance(content, list):
        for block in content:
            if isinstance(block, dict) and block.get("type") == "text":
                texts.append(block.get("text", ""))
    text = " ".join(t.strip() for t in texts if t and t.strip())
    if not text or text.startswith("<system-reminder>"):
        continue
    line = f"{role.upper()}: {text[:PER_TURN_CAP]}"
    out.append(line)
    total += len(line)

# Keep the tail if over budget: the end of a session carries the freshest facts.
result = "\n".join(out)
if len(result) > TOTAL_CAP:
    result = result[-TOTAL_CAP:]
print(result)
