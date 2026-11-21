# Runs as root in ubuntu:24.04 under `bash -euo pipefail`. /in holds the
# export with debian/ in place; the built package goes to /out.
export DEBIAN_FRONTEND=noninteractive
apt-get update
apt-get install -y --no-install-recommends build-essential dpkg-dev lintian
mkdir -p /work
cp -a /in /work/spectral-mic
cd /work/spectral-mic
apt-get build-dep -y ./
dpkg-buildpackage -b -us -uc
cp ../spectral-mic_*.deb /out/
# Findings are triaged by hand, never a reason to stop.
lintian ../spectral-mic_*.changes || true
