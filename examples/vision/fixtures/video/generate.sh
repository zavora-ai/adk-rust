#!/usr/bin/env bash
# Generates S13's clip with Python's Pillow and a user-installed ffmpeg: twenty minutes of
# adk-vision's warehouse aisle (its `tests/fixtures/video/sprites.py`), in which the orange
# forklift enters at 12:45. `labels.json` states the ground truth.
set -euo pipefail
cd "$(dirname "$0")"
python3 -I ../../../../adk-vision/tests/fixtures/video/sprites.py "$@"
# The forklift's left edge: off screen until 760 s, then 40 px/s from x = -200, so its forks
# cross the aisle entrance (x = 200) at 765 s and the whole forklift is inside at 770 s.
x="if(lt(t,760),-400,-200+40*(t-760))"
ffmpeg -y -loglevel error -loop 1 -framerate 5 -i background.png -loop 1 -framerate 5 -i forklift.png \
  -filter_complex "[0][1]overlay=x='${x}':y=135:shortest=0,format=yuv420p" -t 1200 -r 5 \
  -c:v libx264 -preset veryslow -crf 30 -g 300 -movflags +faststart -an aisle-forklift-20min.mp4
mkdir -p sprites
mv background.png forklift.png sprites/
