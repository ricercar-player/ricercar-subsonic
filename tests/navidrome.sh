#!/bin/sh
# Start a throwaway Navidrome on 127.0.0.1:4533 with generated FLAC files
# (three 44.1 kHz / 16-bit tracks, two 192 kHz / 24-bit ones, folder covers) and an admin
# user admin / sesame, then run the end-to-end test against it.
#   tests/navidrome.sh [workdir]      (needs docker, ffmpeg, python3)
set -eu
W=${1:-$(mktemp -d)}
mkdir -p "$W/music/Ensemble/Sessions" "$W/music/Trio/HiRes" "$W/data"
for i in 1 2 3; do
  ffmpeg -loglevel error -y -f lavfi -i "sine=frequency=$((300 * i)):duration=20" \
    -ar 44100 -sample_fmt s16 -metadata title="Track $i" -metadata artist=Ensemble \
    -metadata album_artist=Ensemble -metadata album=Sessions -metadata date=2021 \
    -metadata track=$i -metadata genre=Jazz \
    -metadata REPLAYGAIN_TRACK_GAIN="-6.5 dB" -metadata REPLAYGAIN_TRACK_PEAK=0.9 \
    "$W/music/Ensemble/Sessions/0$i.flac"
done
for i in 1 2; do
  ffmpeg -loglevel error -y -f lavfi -i "sine=frequency=$((500 * i)):duration=15" \
    -ar 192000 -sample_fmt s32 -bits_per_raw_sample 24 -metadata title="Hi $i" \
    -metadata artist=Trio -metadata album_artist=Trio -metadata album=HiRes \
    -metadata date=2024 -metadata track=$i "$W/music/Trio/HiRes/0$i.flac"
done
# Folder covers: Navidrome 0.64 leaves `coverArt` out for albums without art.
for d in Ensemble/Sessions Trio/HiRes; do
  ffmpeg -loglevel error -y -f lavfi -i color=c=0x8a5a3c:s=300x300 -frames:v 1 \
    "$W/music/$d/cover.jpg"
done
docker rm -f ricercar-subsonic-test >/dev/null 2>&1 || true
docker run -d --name ricercar-subsonic-test -p 127.0.0.1:4533:4533 \
  -v "$W/music:/music:ro" -v "$W/data:/data" -e ND_SCANSCHEDULE=0 -e ND_LOGLEVEL=warn \
  deluan/navidrome:latest >/dev/null
trap 'docker rm -f ricercar-subsonic-test >/dev/null' EXIT
for _ in $(seq 30); do curl -sf 127.0.0.1:4533/ping >/dev/null && break; sleep 1; done
curl -sf -X POST 127.0.0.1:4533/auth/createAdmin -H 'Content-Type: application/json' \
  -d '{"username":"admin","password":"sesame"}' >/dev/null
A='u=admin&p=sesame&v=1.16.1&c=test&f=json'
curl -sf "127.0.0.1:4533/rest/startScan?$A" >/dev/null
for _ in $(seq 30); do
  sleep 1
  curl -sf "127.0.0.1:4533/rest/getScanStatus?$A" | grep -q '"scanning":false' && break
done
cargo build --release
python3 "$(dirname "$0")/e2e.py" "$W"
