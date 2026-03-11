#!/usr/bin/env bash

exec brave \
  --user-data-dir=/tmp/brave-prof \
  --disable-extensions \
  --trace-startup \
  --trace-startup-file=/tmp/brave-hitch-trace.json \
  --trace-startup-categories="toplevel,blink,cc,input,renderer.scheduler,devtools.timeline"
