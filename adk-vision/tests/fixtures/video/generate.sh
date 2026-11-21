#!/usr/bin/env bash
# Generates the video fixtures with Python's Pillow and a user-installed ffmpeg: a warehouse
# aisle in which an orange forklift enters at a labelled time, and a short clip of the same
# scene for sam-3.1 video. `labels.json` states the ground truth.
set -euo pipefail
cd "$(dirname "$0")"
python3 -I sprites.py
# The forklift's left edge: off screen until 40 s, then 40 px/s from x = -200, so its forks
# cross the aisle entrance (x = 200) at 45 s and the whole forklift is inside at 50 s.
x="if(lt(t,40),-400,-200+40*(t-40))"
ffmpeg -y -loglevel error -loop 1 -framerate 10 -i background.png -loop 1 -framerate 10 -i forklift.png \
  -filter_complex "[0][1]overlay=x='${x}':y=135:shortest=0,format=yuv420p" -t 120 -r 10 \
  -c:v libx264 -preset veryslow -crf 30 -g 100 -movflags +faststart -an aisle-forklift-120s.mp4
# Three seconds from 44 s to 47 s, the forklift crossing the entrance: 30 frames.
ffmpeg -y -loglevel error -loop 1 -framerate 10 -i background.png -loop 1 -framerate 10 -i forklift.png \
  -filter_complex "[0][1]overlay=x='-200+40*(t+4)':y=135,format=yuv420p" -t 3 -r 10 \
  -c:v libx264 -preset veryslow -crf 28 -movflags +faststart -an forklift-crossing-3s.mp4
rm -f background.png forklift.png
