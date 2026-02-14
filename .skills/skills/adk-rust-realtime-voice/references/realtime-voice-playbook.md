# Realtime Voice Playbook

## Required checks
- audio format config
- VAD mode and thresholds
- server event handling for error, speech start/stop, response done

## Reliability checks
- interrupted session resume path
- tool call done and response merge behavior
- stream cancellation behavior
